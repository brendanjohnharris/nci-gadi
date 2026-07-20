//! Per-job usage panel — the Gadi-tailored replacement for the physics
//! cluster's `qlload` node graph.
//!
//! Gadi is far too large (and locked down) to graph whole-cluster load from a
//! login node, so the first tab focuses on what you can and do care about:
//! *your* running jobs' resource usage. One `qstat -f <ids…>` per poll yields,
//! per job: CPU efficiency (instantaneous `cpupercent` and whole-run `cput`),
//! memory used vs requested (exceeding the request gets the job killed, and
//! over-requesting burns SUs — both directions matter), GPU utilisation where
//! `ngpus` is requested, and a walltime progress bar.
//!
//! Rendered as ANSI text shared by the TUI (via `ansi-to-tui`) and the
//! standalone `qusage` binary, mirroring how qlload was shared upstream.

use crate::pbs::{Job, JobState};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";

// Eighth blocks for the walltime bar (shared style with qarray's progress bars).
const BLOCKS: [&str; 8] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇"];
const BAR_WIDTH: usize = 10;

fn wrap(s: &str, color: &str) -> String {
    format!("{color}{s}{RESET}")
}

/// One running job's parsed `qstat -f` record (only the fields the panel uses).
#[derive(Debug, Clone, Default)]
pub struct UsageRec {
    pub id: String,    // display id: server suffix stripped, array index kept
    pub name: String,
    pub queue: String,
    pub nodes: String, // first exec host ("gadi-" trimmed), "+N" for multi-node
    pub ncpus: i64,
    pub ngpus: i64,
    pub cpupercent: Option<i64>,
    pub cput_sec: Option<i64>,
    pub used_mem_kb: Option<f64>,
    pub req_mem_kb: Option<f64>,
    pub used_wall_sec: Option<i64>,
    pub req_wall_sec: Option<i64>,
    pub gpu_util: Option<i64>,
}

/// "HH:MM:SS" (H may exceed 99) to seconds.
fn wall_to_sec(s: &str) -> Option<i64> {
    let mut it = s.split(':');
    let h: i64 = it.next()?.trim().parse().ok()?;
    let m: i64 = it.next()?.trim().parse().ok()?;
    let sec: i64 = it.next()?.trim().parse().ok()?;
    Some(h * 3600 + m * 60 + sec)
}

/// Memory string to KB. Gadi mixes suffixes: `resources_used.mem` is `…kb`
/// while `Resource_List.mem` is raw bytes (`96636764160b`).
fn parse_mem(s: &str) -> Option<f64> {
    let m = s.trim().to_lowercase();
    if let Some(v) = m.strip_suffix("kb") {
        v.trim().parse().ok()
    } else if let Some(v) = m.strip_suffix("mb") {
        v.trim().parse::<f64>().ok().map(|x| x * 1024.0)
    } else if let Some(v) = m.strip_suffix("gb") {
        v.trim().parse::<f64>().ok().map(|x| x * 1024.0 * 1024.0)
    } else if let Some(v) = m.strip_suffix("tb") {
        v.trim().parse::<f64>().ok().map(|x| x * 1024.0 * 1024.0 * 1024.0)
    } else if let Some(v) = m.strip_suffix('b') {
        v.trim().parse::<f64>().ok().map(|x| x / 1024.0)
    } else if m.is_empty() {
        None
    } else {
        m.parse().ok() // assume KB
    }
}

/// Value of an indented `key = value` line, if this line is exactly that key.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let t = line.trim_start();
    t.strip_prefix(key).and_then(|r| r.strip_prefix(" = ")).map(|v| v.trim())
}

/// Join qstat's tab-indented continuation lines back onto their attribute line.
/// Needed here (unlike qlload upstream) because multi-node `exec_host` values
/// wrap, and a half-parsed host list would under-count nodes.
fn unwrap_continuations(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        match line.strip_prefix('\t') {
            Some(rest) if !out.is_empty() => out.last_mut().unwrap().push_str(rest),
            _ => out.push(line.to_string()),
        }
    }
    out
}

/// `123456789[3].gadi-pbs` -> `123456789[3]`; keeps the array index, drops the server.
fn strip_server(id: &str) -> String {
    id.split('.').next().unwrap_or(id).to_string()
}

/// Condense `exec_host` (`gadi-cpu-clx-0001/0*48+gadi-cpu-clx-0002/0*48`) to a
/// short label: first host with the `gadi-` prefix trimmed, `+N` for the rest.
fn hosts_label(exec_host: &str) -> String {
    let mut hosts: Vec<&str> = Vec::new();
    for seg in exec_host.split('+') {
        let host = seg.split('/').next().unwrap_or(seg).trim();
        if !host.is_empty() && !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    match hosts.split_first() {
        None => "--".to_string(),
        Some((first, rest)) => {
            let short = first.strip_prefix("gadi-").unwrap_or(first);
            if rest.is_empty() {
                short.to_string()
            } else {
                format!("{short}+{}", rest.len())
            }
        }
    }
}

/// Parse `qstat -f` output (one or more records) into usage rows. Records begin
/// at a `Job Id:` line; fields are matched by exact key on continuation-unwrapped
/// lines.
pub fn parse_usage(raw: &str) -> Vec<UsageRec> {
    let mut recs: Vec<UsageRec> = Vec::new();
    for line in unwrap_continuations(raw) {
        if let Some(rest) = line.strip_prefix("Job Id:") {
            recs.push(UsageRec {
                id: strip_server(rest.trim()),
                nodes: "--".into(),
                ..Default::default()
            });
            continue;
        }
        let Some(rec) = recs.last_mut() else { continue };
        if let Some(v) = field(&line, "Job_Name") {
            rec.name = v.to_string();
        } else if let Some(v) = field(&line, "queue") {
            rec.queue = v.trim_end_matches("-exec").to_string();
        } else if let Some(v) = field(&line, "exec_host") {
            rec.nodes = hosts_label(v);
        } else if let Some(v) = field(&line, "Resource_List.ncpus") {
            rec.ncpus = v.parse().unwrap_or(0);
        } else if let Some(v) = field(&line, "Resource_List.ngpus") {
            rec.ngpus = v.parse().unwrap_or(0);
        } else if let Some(v) = field(&line, "Resource_List.mem") {
            rec.req_mem_kb = parse_mem(v);
        } else if let Some(v) = field(&line, "Resource_List.walltime") {
            rec.req_wall_sec = wall_to_sec(v);
        } else if let Some(v) = field(&line, "resources_used.cpupercent") {
            rec.cpupercent = v.parse().ok();
        } else if let Some(v) = field(&line, "resources_used.cput") {
            rec.cput_sec = wall_to_sec(v);
        } else if let Some(v) = field(&line, "resources_used.mem") {
            rec.used_mem_kb = parse_mem(v);
        } else if let Some(v) = field(&line, "resources_used.walltime") {
            rec.used_wall_sec = wall_to_sec(v);
        } else if let Some(v) = field(&line, "resources_used.gpu_util") {
            rec.gpu_util = v.parse().ok();
        }
    }
    recs
}

/// Instantaneous per-core CPU utilisation, 0-100: `cpupercent` accumulates
/// ~100 per busy core, so normalise by ncpus.
fn cpu_now_pct(r: &UsageRec) -> Option<i64> {
    let cp = r.cpupercent?;
    if r.ncpus > 0 {
        Some(((cp as f64 / r.ncpus as f64).round() as i64).clamp(0, 999))
    } else {
        None
    }
}

/// Whole-run CPU efficiency, 0-100: cput / (walltime × ncpus), the same measure
/// nqstat_anu's %CPU column reports.
fn eff_pct(r: &UsageRec) -> Option<i64> {
    let (cput, wall) = (r.cput_sec?, r.used_wall_sec?);
    if wall > 0 && r.ncpus > 0 {
        Some(((100.0 * cput as f64 / (wall as f64 * r.ncpus as f64)).round() as i64).clamp(0, 999))
    } else {
        None
    }
}

/// GPU utilisation normalised per GPU, mirroring the cpupercent treatment.
fn gpu_pct(r: &UsageRec) -> Option<i64> {
    let g = r.gpu_util?;
    if r.ngpus > 0 {
        Some(((g as f64 / r.ngpus as f64).round() as i64).clamp(0, 999))
    } else {
        None
    }
}

/// Utilisation colour: high is good (you're using what you're charged for).
fn util_color(p: i64) -> &'static str {
    if p >= 80 {
        GREEN
    } else if p >= 40 {
        YELLOW
    } else {
        RED
    }
}

/// Memory colour: near the request is dangerous (PBS kills the job past it),
/// far below it is wasteful (mem is charged at 1 core per 4GB on Gadi).
fn mem_color(pct: f64) -> &'static str {
    if pct >= 90.0 {
        RED
    } else if pct >= 40.0 {
        GREEN
    } else {
        YELLOW
    }
}

fn time_color(pct: f64) -> &'static str {
    if pct >= 90.0 {
        RED
    } else if pct >= 75.0 {
        YELLOW
    } else {
        GREEN
    }
}

/// KB -> "12.3G"-style value (one decimal, G for everything — Gadi requests are
/// nearly always GB-scale).
fn fmt_gb(kb: f64) -> String {
    format!("{:.1}G", kb / (1024.0 * 1024.0))
}

/// Seconds -> "HH:MM" (hours may exceed 99).
fn fmt_hm(sec: i64) -> String {
    format!("{:02}:{:02}", sec / 3600, (sec % 3600) / 60)
}

/// Eighth-block walltime bar, coloured by how close the job is to its limit.
fn time_bar(used: i64, req: i64) -> String {
    let pct = if req > 0 { 100.0 * used as f64 / req as f64 } else { 0.0 };
    let color = time_color(pct);
    let eighths_total = (BAR_WIDTH * 8) as i64;
    let filled = if req > 0 { (eighths_total * used / req).clamp(0, eighths_total) } else { 0 };
    let mut bar = String::from("▕");
    for i in 0..BAR_WIDTH as i64 {
        let cell = (filled - i * 8).clamp(0, 8);
        let glyph = if cell >= 8 { "▇" } else { BLOCKS[cell as usize] };
        bar.push_str(&wrap(glyph, color));
    }
    bar.push('▏');
    bar
}

/// The bold summary line: running/waiting job and core counts from the (cheap)
/// jobs poll, so it stays correct even for jobs beyond the usage cap.
fn summary_line(jobs: &[Job]) -> String {
    let count = |pred: &dyn Fn(&JobState) -> bool| -> (usize, u64) {
        jobs.iter()
            .filter(|j| pred(&j.state))
            .fold((0, 0), |(n, c), j| (n + 1, c + j.cpus.unwrap_or(0) as u64))
    };
    let (rn, rc) = count(&JobState::is_running);
    let (qn, qc) = count(&JobState::is_waiting);
    let mut s = format!("{BOLD}{rn} running{RESET} ({rc} cores)");
    s.push_str(&format!(" · {BOLD}{qn} queued/held{RESET} ({qc} cores)"));
    s
}

/// Render the whole panel: summary, then one aligned row per running job.
pub fn render(jobs: &[Job], qstat_f: &str, truncated: usize) -> String {
    let mut out = summary_line(jobs);
    out.push('\n');

    let recs = parse_usage(qstat_f);
    if recs.is_empty() {
        out.push_str(&format!("{DIM}No running jobs.{RESET}\n"));
        return out;
    }
    out.push('\n');

    let any_gpu = recs.iter().any(|r| r.ngpus > 0);
    let opt_pct = |p: Option<i64>| p.map(|v| format!("{v}%")).unwrap_or_else(|| "--".into());

    // Build plain cells first, then measure, pad, and colour.
    struct Cells {
        id: String,
        name: String,
        queue: String,
        nodes: String,
        cpus: String,
        cpu: String,
        eff: String,
        gpu: String,
        mem: String,
        mem_pct: f64,
        cpu_pct: Option<i64>,
        eff_pct: Option<i64>,
        gpu_pct: Option<i64>,
        time: String,
        bar: String,
    }
    let cells: Vec<Cells> = recs
        .iter()
        .map(|r| {
            let cpus = if r.ngpus > 0 {
                format!("{}c+{}g", r.ncpus, r.ngpus)
            } else {
                format!("{}c", r.ncpus)
            };
            let (mem, mem_pct) = match (r.used_mem_kb, r.req_mem_kb) {
                (Some(u), Some(q)) if q > 0.0 => {
                    (format!("{}/{}", fmt_gb(u), fmt_gb(q)), 100.0 * u / q)
                }
                (Some(u), _) => (format!("{}/--", fmt_gb(u)), 0.0),
                _ => ("--".into(), 0.0),
            };
            let (time, bar) = match (r.used_wall_sec, r.req_wall_sec) {
                (Some(u), Some(q)) => (
                    format!("{}/{} {:>3.0}%", fmt_hm(u), fmt_hm(q), (100.0 * u as f64 / q.max(1) as f64).floor()),
                    time_bar(u, q),
                ),
                (None, Some(q)) => (format!("--/{}", fmt_hm(q)), time_bar(0, q)),
                _ => ("--".into(), time_bar(0, 1)),
            };
            Cells {
                id: r.id.clone(),
                name: r.name.clone(),
                queue: r.queue.clone(),
                nodes: r.nodes.clone(),
                cpus,
                cpu: opt_pct(cpu_now_pct(r)),
                eff: opt_pct(eff_pct(r)),
                gpu: opt_pct(gpu_pct(r)),
                mem,
                mem_pct,
                cpu_pct: cpu_now_pct(r),
                eff_pct: eff_pct(r),
                gpu_pct: gpu_pct(r),
                time,
                bar,
            }
        })
        .collect();

    let w = |f: &dyn Fn(&Cells) -> usize, min: usize| cells.iter().map(f).max().unwrap_or(0).max(min);
    let iw = w(&|c| c.id.chars().count(), 3);
    let nw = w(&|c| c.name.chars().count(), 4);
    let qw = w(&|c| c.queue.chars().count(), 5);
    let hw = w(&|c| c.nodes.chars().count(), 5);
    let cw = w(&|c| c.cpus.chars().count(), 4);
    let pw = w(&|c| c.cpu.chars().count(), 4);
    let ew = w(&|c| c.eff.chars().count(), 4);
    let gw = if any_gpu { w(&|c| c.gpu.chars().count(), 4) } else { 0 };
    let mw = w(&|c| c.mem.chars().count(), 3);
    let tw = w(&|c| c.time.chars().count(), 4);

    let mut header = format!(
        "{:<iw$}  {:<nw$}  {:<qw$}  {:<hw$}  {:>cw$}  {:>pw$}  {:>ew$}",
        "JOB", "NAME", "QUEUE", "NODES", "CPUS", "CPU%", "EFF%",
    );
    if any_gpu {
        header.push_str(&format!("  {:>gw$}", "GPU%"));
    }
    header.push_str(&format!("  {:>mw$}  {:<tw$}  WALLTIME", "MEM", "TIME"));
    out.push_str(&format!("{BOLD}{header}{RESET}\n"));

    for c in &cells {
        let colored_pct = |txt: &str, width: usize, pct: Option<i64>| {
            let padded = format!("{:>width$}", txt);
            match pct {
                Some(p) => wrap(&padded, util_color(p)),
                None => padded,
            }
        };
        let mut line = format!(
            "{:<iw$}  {:<nw$}  {:<qw$}  {:<hw$}  {:>cw$}",
            c.id, c.name, c.queue, c.nodes, c.cpus,
        );
        line.push_str("  ");
        line.push_str(&colored_pct(&c.cpu, pw, c.cpu_pct));
        line.push_str("  ");
        line.push_str(&colored_pct(&c.eff, ew, c.eff_pct));
        if any_gpu {
            line.push_str("  ");
            line.push_str(&colored_pct(&c.gpu, gw, c.gpu_pct));
        }
        line.push_str("  ");
        let mem_padded = format!("{:>mw$}", c.mem);
        line.push_str(&if c.mem == "--" { mem_padded } else { wrap(&mem_padded, mem_color(c.mem_pct)) });
        line.push_str(&format!("  {:<tw$}  ", c.time));
        line.push_str(&c.bar);
        line.push('\n');
        out.push_str(&line);
    }

    if truncated > 0 {
        out.push_str(&format!(
            "{DIM}… {truncated} more running jobs not shown (usage detail capped at {}){RESET}\n",
            crate::pbs::USAGE_CAP
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pbs::parse_qstat;

    // Abridged from a real `qstat -fx` record on Gadi (job 174170283).
    const RECORD: &str = "\
Job Id: 174170283.gadi-pbs
    Job_Name = train-gpu
    Job_Owner = bh5941@gadi-login-07.gadi.nci.org.au
    resources_used.cpupercent = 9
    resources_used.cput = 00:00:18
    resources_used.gpu_mem = 0
    resources_used.gpu_util = 0
    resources_used.jobfs = 1484b
    resources_used.mem = 2323996kb
    resources_used.ncpus = 12
    resources_used.vmem = 2323996kb
    resources_used.walltime = 00:05:16
    job_state = R
    queue = gpuvolta-exec
    server = gadi-pbs-01.gadi.nci.org.au
    exec_host = gadi-gpu-v100-0100/0*12
    exec_vnode = (gadi-gpu-v100-0100:ncpus=12:mem=94371840kb:jobfs=104857600kb:
\tngpus=1)
    Resource_List.jobfs = 107374182400b
    Resource_List.mem = 96636764160b
    Resource_List.mpiprocs = 12
    Resource_List.ncpus = 12
    Resource_List.ngpus = 1
    Resource_List.nodect = 1
    Resource_List.walltime = 01:00:00
    Resource_List.storage = gdata/xr78+scratch/xr78
";

    const MULTI: &str = "\
Job Id: 174200000.gadi-pbs
    Job_Name = big-mpi
    queue = normal-exec
    resources_used.cpupercent = 9410
    resources_used.cput = 500:00:00
    resources_used.mem = 100000000kb
    resources_used.walltime = 05:30:00
    job_state = R
    exec_host = gadi-cpu-clx-0001/0*48+gadi-cpu-clx-0002/0*48+gadi-cpu-clx-0003
\t/0*48+gadi-cpu-clx-0004/0*48
    Resource_List.mem = 202937204736b
    Resource_List.ncpus = 96
    Resource_List.walltime = 10:00:00
";

    #[test]
    fn parses_real_gadi_record() {
        let recs = parse_usage(RECORD);
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.id, "174170283");
        assert_eq!(r.name, "train-gpu");
        assert_eq!(r.queue, "gpuvolta"); // -exec suffix trimmed
        assert_eq!(r.nodes, "gpu-v100-0100"); // gadi- prefix trimmed
        assert_eq!(r.ncpus, 12);
        assert_eq!(r.ngpus, 1);
        assert_eq!(r.cpupercent, Some(9));
        assert_eq!(r.cput_sec, Some(18));
        assert_eq!(r.used_wall_sec, Some(316));
        assert_eq!(r.req_wall_sec, Some(3600));
        // Resource_List.mem is bytes: 96636764160b = 94371840kb = 90GB.
        assert_eq!(r.req_mem_kb, Some(94371840.0));
        assert_eq!(r.used_mem_kb, Some(2323996.0));
        assert_eq!(r.gpu_util, Some(0));
    }

    #[test]
    fn unwraps_multinode_exec_host() {
        let recs = parse_usage(MULTI);
        assert_eq!(recs.len(), 1);
        // 4 hosts, the 3rd wrapped across a tab continuation: first + 3 others.
        assert_eq!(recs[0].nodes, "cpu-clx-0001+3");
        assert_eq!(recs[0].ncpus, 96);
    }

    #[test]
    fn derived_percentages() {
        let recs = parse_usage(MULTI);
        let r = &recs[0];
        // cpupercent 9410 over 96 cores ≈ 98%/core.
        assert_eq!(cpu_now_pct(r), Some(98));
        // cput 500h over 5.5h × 96 cores ≈ 95%.
        assert_eq!(eff_pct(r), Some(95));
        assert_eq!(gpu_pct(r), None); // no gpus requested

        let gpu = &parse_usage(RECORD)[0];
        assert_eq!(gpu_pct(gpu), Some(0));
        assert_eq!(cpu_now_pct(gpu), Some(1)); // 9/12 rounds to 1
    }

    #[test]
    fn renders_summary_and_rows() {
        let jobs = parse_qstat(
            "174170283.gadi-pbs bh5941 gpuvolta-exec train-gpu 123 1 12 90gb 01:00 R 00:05 gadi-gpu-v100-0100/0*12\n\
             174187629.gadi-pbs bh5941 normal-exec bigjob -- 1 16 64gb 48:00 Q -- --\n",
        );
        let out = render(&jobs, RECORD, 0);
        assert!(out.contains("1 running"));
        assert!(out.contains("(12 cores)"));
        assert!(out.contains("1 queued/held"));
        assert!(out.contains("(16 cores)"));
        assert!(out.contains("174170283")); // id, server-stripped
        assert!(!out.contains("gadi-pbs")); // …completely
        assert!(out.contains("2.2G/90.0G")); // used/req mem
        assert!(out.contains("00:05/01:00")); // used/req walltime
        assert!(out.contains("GPU%")); // gpu column present for a gpu job
        assert!(out.contains("12c+1g"));
        assert!(out.contains('▕')); // walltime bar drawn
    }

    #[test]
    fn renders_no_running_and_truncation() {
        let jobs = parse_qstat(
            "174187629.gadi-pbs bh5941 normal-exec bigjob -- 1 16 64gb 48:00 Q -- --\n",
        );
        let out = render(&jobs, "", 0);
        assert!(out.contains("0 running"));
        assert!(out.contains("No running jobs."));

        let out2 = render(&jobs, MULTI, 7);
        assert!(out2.contains("7 more running jobs not shown"));
        assert!(!out2.contains("GPU%")); // no gpu jobs -> no gpu column
    }

    #[test]
    fn mem_and_time_helpers() {
        assert_eq!(parse_mem("2gb"), Some(2.0 * 1024.0 * 1024.0));
        assert_eq!(parse_mem("1024kb"), Some(1024.0));
        assert_eq!(parse_mem("96636764160b"), Some(94371840.0));
        assert_eq!(parse_mem(""), None);
        assert_eq!(fmt_gb(94371840.0), "90.0G");
        assert_eq!(fmt_hm(316), "00:05");
        assert_eq!(fmt_hm(48 * 3600), "48:00");
        assert_eq!(wall_to_sec("100:00:30"), Some(360030));
    }

    #[test]
    fn colours_by_utilisation() {
        assert_eq!(util_color(95), GREEN);
        assert_eq!(util_color(50), YELLOW);
        assert_eq!(util_color(10), RED);
        assert_eq!(mem_color(95.0), RED); // about to hit the request → killed
        assert_eq!(mem_color(60.0), GREEN);
        assert_eq!(mem_color(10.0), YELLOW); // over-requested → wasted SUs
        assert_eq!(time_color(95.0), RED);
    }

    #[test]
    fn hosts_label_variants() {
        assert_eq!(hosts_label("gadi-cpu-clx-0001/0*48"), "cpu-clx-0001");
        assert_eq!(
            hosts_label("gadi-cpu-clx-0001/0*24+gadi-cpu-clx-0001/1*24+gadi-cpu-clx-0002/0*48"),
            "cpu-clx-0001+1" // duplicate host deduped
        );
        assert_eq!(hosts_label(""), "--");
    }
}
