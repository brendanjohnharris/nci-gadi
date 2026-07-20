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
pub const USAGE_CAP: usize = 32;

/// Per-job usage panel (the Gadi replacement for the cluster-wide qlload):
/// one `qstat -f <ids…>` over the running jobs, rendered by `usage::render`.
/// With nothing running this makes no PBS call and renders the summary alone.
pub fn fetch_usage(jobs: &[Job]) -> Result<String> {
    let running: Vec<&str> = jobs
        .iter()
        .filter(|j| j.state.is_running())
        .map(|j| j.id.as_str())
        .collect();
    if running.is_empty() {
        return Ok(crate::usage::render(jobs, "", 0));
    }
    let truncated = running.len().saturating_sub(USAGE_CAP);
    let mut args = vec!["-f"];
    args.extend(running.iter().take(USAGE_CAP));
    let detail = run_lenient("qstat", &args)?;
    Ok(crate::usage::render(jobs, &detail, truncated))
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
