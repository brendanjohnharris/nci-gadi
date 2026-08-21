#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Running,
    Exiting,
    Queued,
    Held,
    Suspended,
    Other(char),
}

impl JobState {
    fn from_char(c: char) -> JobState {
        match c {
            'R' => JobState::Running,
            'E' => JobState::Exiting,
            'Q' => JobState::Queued,
            'H' => JobState::Held,
            'S' => JobState::Suspended,
            other => JobState::Other(other),
        }
    }

    /// Occupying a node right now (Exiting jobs are still finishing on theirs).
    pub fn is_running(&self) -> bool {
        matches!(self, JobState::Running | JobState::Exiting)
    }

    /// Waiting to (re)start: queued, held, or suspended.
    pub fn is_waiting(&self) -> bool {
        matches!(self, JobState::Queued | JobState::Held | JobState::Suspended)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub owner: String,
    pub queue: String,
    pub name: String,
    pub state: JobState,
    pub node: Option<String>,
    pub cpus: Option<u32>,
    pub mem: Option<String>,
    pub req_walltime: Option<String>,
    pub elapsed: Option<String>,
}

/// True if the field is a job-id-shaped token: starts with a digit and the
/// only non-alphanumeric characters before the first '.' are '[' / ']'.
fn looks_like_job_id(field: &str) -> bool {
    let head = field.split('.').next().unwrap_or(field);
    let mut chars = head.chars();
    match chars.next() {
        Some(c) if c.is_ascii_digit() => {}
        _ => return false,
    }
    head.chars().all(|c| c.is_ascii_digit() || c == '[' || c == ']')
}

/// Numeric job number: digits before the first '[' or '.'.
pub fn job_number(id: &str) -> &str {
    let end = id.find(|c| c == '[' || c == '.').unwrap_or(id.len());
    &id[..end]
}

pub fn has_array_jobs(jobs: &[Job]) -> bool {
    jobs.iter().any(|j| j.id.contains('['))
}

/// Parse `qstat -w -u <user> -t -n1` output into jobs. Header/separator/blank
/// lines are skipped: only rows whose first field looks like a job id are kept.
pub fn parse_qstat(raw: &str) -> Vec<Job> {
    let mut jobs = Vec::new();
    for line in raw.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 || !looks_like_job_id(fields[0]) {
            continue;
        }
        let state_field = fields[9];
        if state_field.len() != 1 {
            continue;
        }
        let state = JobState::from_char(state_field.chars().next().unwrap());
        let node = fields.get(11).and_then(|n| {
            if *n == "--" {
                None
            } else {
                n.split('/').next().map(|h| h.to_string())
            }
        });
        let cpus = fields.get(6).and_then(|s| s.parse::<u32>().ok());
        let mem = fields.get(7).filter(|s| **s != "--").map(|s| s.to_string());
        let req_walltime = fields.get(8).filter(|s| **s != "--").map(|s| s.to_string());
        let elapsed = fields.get(10).filter(|s| **s != "--").map(|s| s.to_string());
        jobs.push(Job {
            id: fields[0].to_string(),
            owner: fields[1].to_string(),
            queue: fields[2].to_string(),
            name: fields[3].to_string(),
            state,
            node,
            cpus,
            mem,
            req_walltime,
            elapsed,
        });
    }
    jobs
}

use anyhow::{Context, Result};
use std::process::Command;

/// Run a PBS client command locally. On Gadi qstat/qcat live in
/// /opt/pbs/default/bin, which is on PATH on both login and compute nodes,
/// so no SSH hop is needed (unlike the physics-cluster original).
fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn `{cmd}`"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("`{cmd}` exited {}: {}", out.status, err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Like `run`, but tolerate a nonzero exit as long as stdout is non-empty.
/// `qstat -f <id1> <id2> …` exits nonzero when ANY id has vanished (finished
/// between the jobs poll and this call) while still printing the records of the
/// ids it does know — keep those instead of failing the whole panel.
fn run_lenient(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn `{cmd}`"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() && stdout.trim().is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("`{cmd}` exited {}: {}", out.status, err.trim());
    }
    Ok(stdout)
}

pub fn fetch_job_detail(jobid: &str) -> Result<String> {
    match run("qstat", &["-f", jobid]) {
        Ok(text) => Ok(text),
        Err(first_err) => {
            // Finished jobs stay visible in Gadi's job history via -x.
            if let Ok(text) = run("qstat", &["-xf", jobid]) {
                return Ok(text);
            }

            // Jobs can disappear between the jobs poll and details fetch.
            // Treat this as informational, not a hard UI error.
            let msg = first_err.to_string();
            // "Unknown Job" subsumes "Unknown Job Id"; keep the lowercase variant too.
            if msg.contains("Unknown Job") || msg.contains("unknown job id") {
                return Ok(format!(
                    "Job {jobid} is no longer available in the scheduler (it may have finished or been purged)."
                ));
            }

            Err(first_err)
        }
    }
}

/// Local file path from a `qstat -f` record's `Output_Path` attribute
/// (`host:/abs/path` or a bare path), un-wrapping qstat's tab-continued lines
/// and dropping the `host:` prefix. Used as a log-preview fallback. `None` if
/// the attribute is absent.
pub fn output_path(details: &str) -> Option<std::path::PathBuf> {
    let mut lines = details.lines();
    while let Some(line) = lines.next() {
        if let Some(first) = line.trim_start().strip_prefix("Output_Path = ") {
            let mut val = first.to_string();
            for cont in lines.by_ref() {
                match cont.strip_prefix('\t') {
                    Some(rest) => val.push_str(rest), // qstat wraps long values, tab-indented
                    None => break,
                }
            }
            let path = val.split_once(':').map(|(_, p)| p).unwrap_or(&val);
            return Some(std::path::PathBuf::from(path.trim()));
        }
    }
    None
}

pub fn fetch_user_jobs(user: &str) -> Result<Vec<Job>> {
    // Wide mode so long ids (e.g. `123456789[1234].gadi-pbs`) are never truncated.
    let raw = run("qstat", &["-w", "-u", user, "-t", "-n1"])?;
    Ok(parse_qstat(&raw))
}

/// At most this many running jobs are detailed per usage poll, to keep the
/// `qstat -f` output (a few KB per job) bounded for huge array sweeps.
pub const RUN_CAP: usize = 24;
/// … and this many waiting jobs (for the QUEUED est-start/comment block).
pub const WAIT_CAP: usize = 8;
/// At most this many nodes queried per `pbsnodes` call for the NODES block.
pub const NODE_CAP: usize = 12;
/// Finished jobs shown in the RECENT panel.
pub const RECENT_CAP: usize = 8;

/// Query id for a waiting job: array subjobs collapse to their `[]` master
/// (which carries the est-start/comment for the whole array), so a queued
/// 1000-subjob array costs one record instead of a thousand.
fn waiting_query_id(id: &str) -> String {
    match (id.find('['), id.find(']')) {
        (Some(lb), Some(rb)) if rb > lb + 1 => format!("{}[]{}", &id[..lb], &id[rb + 1..]),
        _ => id.to_string(),
    }
}

/// Per-job usage panel (the Gadi replacement for the cluster-wide qlload):
/// one `qstat -f` over the running jobs (plus waiting masters, for the QUEUED
/// diagnosis block), one `pbsnodes` over the nodes those jobs occupy, and one
/// `qstat -Q` for queue pressure — all rendered by `usage::render_full`.
/// With no jobs at all this makes no PBS call and renders the summary alone.
pub fn fetch_usage(jobs: &[Job]) -> Result<String> {
    let running: Vec<&str> = jobs
        .iter()
        .filter(|j| j.state.is_running())
        .map(|j| j.id.as_str())
        .collect();
    let mut waiting: Vec<String> = Vec::new();
    for j in jobs.iter().filter(|j| j.state.is_waiting()) {
        let id = waiting_query_id(&j.id);
        if !waiting.contains(&id) {
            waiting.push(id);
        }
    }
    let truncated = running.len().saturating_sub(RUN_CAP);
    let mut ids: Vec<String> = running.iter().take(RUN_CAP).map(|s| s.to_string()).collect();
    ids.extend(waiting.into_iter().take(WAIT_CAP));
    if ids.is_empty() {
        return Ok(crate::usage::render_full(jobs, "", 0, None, None));
    }
    let mut args: Vec<&str> = vec!["-f"];
    args.extend(ids.iter().map(|s| s.as_str()));
    let detail = run_lenient("qstat", &args)?;

    // The nodes my running jobs occupy, from the freshly fetched detail. The
    // side queries degrade silently: a pbsnodes/qstat -Q hiccup should never
    // take the whole panel down.
    let mut hosts: Vec<String> = Vec::new();
    for rec in crate::usage::parse_usage(&detail) {
        if rec.is_running() {
            for h in rec.hosts {
                if !hosts.contains(&h) {
                    hosts.push(h);
                }
            }
        }
    }
    hosts.truncate(NODE_CAP);
    let nodes_raw = if hosts.is_empty() {
        None
    } else {
        let node_args: Vec<&str> = hosts.iter().map(|s| s.as_str()).collect();
        run("pbsnodes", &node_args).ok()
    };
    let queues_raw = run("qstat", &["-Q"]).ok();

    Ok(crate::usage::render_full(
        jobs,
        &detail,
        truncated,
        nodes_raw.as_deref(),
        queues_raw.as_deref(),
    ))
}

/// The RECENT panel: finished jobs from Gadi's job history. `qstat -fx -u` is
/// not supported on Gadi, so this is a bounded two-step: the `-xw` history
/// table for F-state ids (newest last), then one `qstat -fx` over the last
/// `RECENT_CAP` of them for Exit_status/obittime.
pub fn fetch_recent(user: &str) -> Result<Option<String>> {
    let table = run("qstat", &["-xw", "-u", user])?;
    let finished: Vec<String> = parse_qstat(&table)
        .into_iter()
        .filter(|j| j.state == JobState::Other('F'))
        .map(|j| j.id)
        .collect();
    if finished.is_empty() {
        return Ok(None);
    }
    let start = finished.len().saturating_sub(RECENT_CAP);
    let mut args = vec!["-fx"];
    args.extend(finished[start..].iter().map(|s| s.as_str()));
    let detail = run_lenient("qstat", &args)?;
    Ok(crate::usage::render_recent(&detail))
}

/// SU + storage quota chart from `nci_account -P <project>` plus the
/// home-directory quota from `quota`. nci_account is slow (a few seconds), so
/// this is polled on its own long cadence. Either source may fail alone (the
/// chart just loses those rows); only a total failure surfaces an error.
pub fn fetch_account(project: &str) -> Result<Option<String>> {
    // `quota` exits nonzero when a quota is exceeded — exactly when the output
    // matters most — hence the lenient runner.
    let home = run_lenient("quota", &[]).ok();
    let nci = if project.is_empty() {
        Ok(String::new())
    } else {
        run("nci_account", &["-P", project])
    };
    let nci_raw = match nci {
        Ok(s) => s,
        Err(e) => {
            if home.is_none() {
                return Err(e);
            }
            String::new()
        }
    };
    Ok(crate::usage::render_account(&nci_raw, home.as_deref()))
}

/// Scratch-expiry warning from `nci-file-expiry list-warnings` (None when
/// nothing is scheduled to expire).
pub fn fetch_expiry() -> Result<Option<String>> {
    let raw = run("nci-file-expiry", &["list-warnings"])?;
    Ok(crate::usage::render_expiry(&raw))
}

/// Process listing of a running job via Gadi's `qps` (a `ps` run on the job's
/// compute nodes). For GPU jobs, try `qps_gpu` first — same listing plus GPU
/// utilisation — falling back to plain `qps` if it's absent or unhappy.
pub fn fetch_procs(jobid: &str, gpu: bool) -> Result<String> {
    if gpu {
        if let Ok(out) = run("qps_gpu", &[jobid]) {
            return Ok(out);
        }
    }
    run("qps", &[jobid])
}

/// Array-job progress, rendered by the in-process qarray port (see `qarray.rs`).
/// Finds the user's array masters, then fetches each one's subjobs (`qstat -w -t`;
/// wide so large subjob indices don't truncate the id column).
pub fn fetch_array_progress(user: &str) -> Result<String> {
    let list = run("qstat", &["-w", "-u", user])?;
    let mut lines = Vec::new();
    for base in crate::qarray::array_bases(&list) {
        let detail = run("qstat", &["-w", "-t", &base])?;
        if let Some(line) = crate::qarray::render_array(&base, &detail) {
            lines.push(line);
        }
    }
    Ok(lines.join("\n"))
}

/// Spooled stdout of a running job via Gadi's `qcat`. Works only while the job
/// is running (qcat reads the spool on the job's head compute node); errors are
/// returned for the caller to render as a hint.
pub fn qcat_stdout(jobid: &str) -> Result<String> {
    run("qcat", &["-o", jobid])
}

/// Candidate usernames for the `u` fuzzy switch: members of the caller's unix
/// groups (on Gadi each project is a group, so this is "people I share a project
/// with"), resolved via getent. Best-effort — failures just mean fewer fuzzy
/// candidates; a full username typed into the box always works regardless.
pub fn fetch_known_users() -> Vec<String> {
    let mut users: Vec<String> = Vec::new();
    if let Ok(me) = std::env::var("USER") {
        users.push(me);
    }
    let groups = Command::new("id")
        .arg("-Gn")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    for group in groups.split_whitespace() {
        if let Ok(out) = Command::new("getent").args(["group", group]).output() {
            let line = String::from_utf8_lossy(&out.stdout);
            // getent group format: name:passwd:gid:member1,member2,…
            if let Some(members) = line.trim_end().rsplit(':').next() {
                users.extend(
                    members
                        .split(',')
                        .map(|m| m.trim())
                        .filter(|m| !m.is_empty())
                        .map(|m| m.to_string()),
                );
            }
        }
    }
    users.sort();
    users.dedup();
    users
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\ngadi-pbs: \n                                                                                                   Req'd  Req'd   Elap\nJob ID                         Username        Queue           Jobname         SessID   NDS  TSK   Memory Time  S Time\n------------------------------ --------------- --------------- --------------- -------- ---- ----- ------ ----- - -----\n174170283.gadi-pbs             bh5941          gpuvolta-exec   train-gpu       2040841  1    12    90gb   01:00 R 00:05  gadi-gpu-v100-0100/0*12\n174187629.gadi-pbs             bh5941          normal-exec     bigjob          --       1    16    64gb   48:00 Q --      -- \n174190001[3].gadi-pbs          bh5941          normal-exec     arr             --       1    1     4gb    02:00 R 00:05  gadi-cpu-clx-0001/2\n174190002.gadi-pbs             bh5941          normal-exec     paused          --       1    8     32gb   10:00 S 00:44  gadi-cpu-clx-0002/0*8\n";

    #[test]
    fn parses_states_and_nodes() {
        let jobs = parse_qstat(SAMPLE);
        assert_eq!(jobs.len(), 4);

        assert_eq!(jobs[0].id, "174170283.gadi-pbs");
        assert_eq!(jobs[0].state, JobState::Running);
        assert_eq!(jobs[0].node.as_deref(), Some("gadi-gpu-v100-0100"));
        assert_eq!(jobs[0].owner, "bh5941");
        assert_eq!(jobs[0].queue, "gpuvolta-exec");

        assert_eq!(jobs[1].state, JobState::Queued);
        assert_eq!(jobs[1].node, None);

        assert_eq!(jobs[2].state, JobState::Running);
        assert_eq!(jobs[2].node.as_deref(), Some("gadi-cpu-clx-0001"));

        // Gadi suspends jobs (e.g. for express work); S must land in the waiting bucket.
        assert_eq!(jobs[3].state, JobState::Suspended);
        assert!(jobs[3].state.is_waiting());
        assert!(!jobs[3].state.is_running());
    }

    #[test]
    fn running_covers_exiting() {
        assert!(JobState::from_char('E').is_running());
        assert!(JobState::from_char('R').is_running());
        assert!(!JobState::from_char('Q').is_running());
        assert_eq!(JobState::from_char('B'), JobState::Other('B'));
    }

    #[test]
    fn ignores_headers_and_blank_lines() {
        assert_eq!(parse_qstat("\n\ngadi-pbs: \nJob ID Username\n--- ---\n").len(), 0);
    }

    #[test]
    fn detects_array_jobs() {
        let jobs = parse_qstat(SAMPLE);
        assert!(has_array_jobs(&jobs));
        assert_eq!(job_number("174190001[3].gadi-pbs"), "174190001");
        assert_eq!(job_number("174170283.gadi-pbs"), "174170283");
    }

    #[test]
    fn parses_output_path_unwrapping_continuation() {
        // qstat -f wraps long values onto tab-indented continuation lines.
        let d = "    Job_Name = x\n    Output_Path = gadi-login-07.gadi.nci.org.au:/scratch/xr78/bh5941/runs/VA\n\tSP.out\n    Priority = 0\n";
        assert_eq!(
            output_path(d),
            Some(std::path::PathBuf::from("/scratch/xr78/bh5941/runs/VASP.out"))
        );
        assert_eq!(output_path("no attribute here"), None);
    }

    #[test]
    fn waiting_ids_collapse_to_array_master() {
        assert_eq!(waiting_query_id("174190001[3].gadi-pbs"), "174190001[].gadi-pbs");
        assert_eq!(waiting_query_id("174190001[].gadi-pbs"), "174190001[].gadi-pbs");
        assert_eq!(waiting_query_id("174187629.gadi-pbs"), "174187629.gadi-pbs");
    }

    #[test]
    fn history_rows_parse_as_finished() {
        // qstat -xw history rows carry state F and HH:MM:SS elapsed times.
        let jobs = parse_qstat(
            "174190352.gadi-pbs   bh5941   copyq-exec   montest   501688   1   1   1024m 00:08 F 00:01:32\n",
        );
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, JobState::Other('F'));
    }

    #[test]
    fn parses_resource_columns() {
        let jobs = parse_qstat(SAMPLE);
        assert_eq!(jobs[0].cpus, Some(12));
        assert_eq!(jobs[0].mem.as_deref(), Some("90gb"));
        assert_eq!(jobs[0].req_walltime.as_deref(), Some("01:00"));
        assert_eq!(jobs[0].elapsed.as_deref(), Some("00:05"));
        // queued job: Elap Time is "--", so elapsed is None
        assert_eq!(jobs[1].cpus, Some(16));
        assert_eq!(jobs[1].elapsed, None);
    }
}
