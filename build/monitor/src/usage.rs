//! Per-job usage panel — the Gadi-tailored replacement for the physics
//! cluster's `qlload` node graph — plus the renderers for the other Gadi data
//! sources (SU accounting, storage quotas, node status, queue pressure,
//! recently finished jobs, scratch-expiry warnings).
//!
//! Gadi is far too large (and locked down) to graph whole-cluster load from a
//! login node, so the first tab focuses on what you can and do care about:
//! *your* jobs. One `qstat -f <ids…>` per poll yields, per running job: CPU
//! efficiency (instantaneous `cpupercent` and whole-run `cput`), memory and
//! jobfs used vs requested, GPU utilisation where `ngpus` is requested, and a
//! walltime progress bar. The same call covers queued/held jobs, whose
//! scheduler `comment` ("Not Running: …") and `estimated.start_time` become
//! the QUEUED block. `pbsnodes` on the nodes your jobs occupy becomes the
//! NODES block, and `qstat -Q` the queue-pressure footer.
//!
//! Everything renders as ANSI text shared by the TUI (via `ansi-to-tui`) and
//! the standalone `qusage` binary, mirroring how qlload was shared upstream.

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

/// One job's parsed `qstat -f` record (only the fields the panels use).
#[derive(Debug, Clone, Default)]
pub struct UsageRec {
    pub id: String,    // display id: server suffix stripped, array index kept
    pub name: String,
    pub queue: String,
    pub state: char,   // job_state: R/E running, Q/H/S waiting, F finished
    pub nodes: String, // first exec host ("gadi-" trimmed), "+N" for multi-node
    pub hosts: Vec<String>, // full distinct exec hosts (for pbsnodes)
    pub ncpus: i64,
    pub ngpus: i64,
    pub cpupercent: Option<i64>,
    pub cput_sec: Option<i64>,
    pub used_mem_kb: Option<f64>,
    pub req_mem_kb: Option<f64>,
    pub used_jobfs_kb: Option<f64>,
    pub req_jobfs_kb: Option<f64>,
    pub used_wall_sec: Option<i64>,
    pub req_wall_sec: Option<i64>,
    pub gpu_util: Option<i64>,
    pub comment: Option<String>,   // scheduler comment (queued: "Not Running: …")
    pub est_start: Option<String>, // estimated.start_time, raw ctime string
    pub exit_status: Option<i64>,  // finished jobs (qstat -fx)
    pub obittime: Option<String>,  // finished jobs: raw ctime string
}

impl UsageRec {
    pub fn is_running(&self) -> bool {
        matches!(self.state, 'R' | 'E')
    }
    pub fn is_waiting(&self) -> bool {
        matches!(self.state, 'Q' | 'H' | 'S')
    }
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
/// and long scheduler comments wrap, and a half-parsed value is worse than none.
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

/// Distinct hosts from `exec_host` (`gadi-cpu-clx-0001/0*48+gadi-cpu-clx-0002/…`).
fn hosts_of(exec_host: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for seg in exec_host.split('+') {
        let host = seg.split('/').next().unwrap_or(seg).trim();
        if !host.is_empty() && !hosts.iter().any(|h| h == host) {
            hosts.push(host.to_string());
        }
    }
    hosts
}

/// Condense a host list to a short label: first host with the `gadi-` prefix
/// trimmed, `+N` for the rest.
fn hosts_label(hosts: &[String]) -> String {
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
        } else if let Some(v) = field(&line, "job_state") {
            rec.state = v.chars().next().unwrap_or(' ');
        } else if let Some(v) = field(&line, "exec_host") {
            rec.hosts = hosts_of(v);
            rec.nodes = hosts_label(&rec.hosts);
        } else if let Some(v) = field(&line, "Resource_List.ncpus") {
            rec.ncpus = v.parse().unwrap_or(0);
        } else if let Some(v) = field(&line, "Resource_List.ngpus") {
            rec.ngpus = v.parse().unwrap_or(0);
        } else if let Some(v) = field(&line, "Resource_List.mem") {
            rec.req_mem_kb = parse_mem(v);
        } else if let Some(v) = field(&line, "Resource_List.jobfs") {
            rec.req_jobfs_kb = parse_mem(v);
        } else if let Some(v) = field(&line, "Resource_List.walltime") {
            rec.req_wall_sec = wall_to_sec(v);
        } else if let Some(v) = field(&line, "resources_used.cpupercent") {
            rec.cpupercent = v.parse().ok();
        } else if let Some(v) = field(&line, "resources_used.cput") {
            rec.cput_sec = wall_to_sec(v);
        } else if let Some(v) = field(&line, "resources_used.mem") {
            rec.used_mem_kb = parse_mem(v);
        } else if let Some(v) = field(&line, "resources_used.jobfs") {
            rec.used_jobfs_kb = parse_mem(v);
        } else if let Some(v) = field(&line, "resources_used.walltime") {
            rec.used_wall_sec = wall_to_sec(v);
        } else if let Some(v) = field(&line, "resources_used.gpu_util") {
            rec.gpu_util = v.parse().ok();
        } else if let Some(v) = field(&line, "comment") {
            rec.comment = Some(v.to_string());
        } else if let Some(v) = field(&line, "estimated.start_time") {
            rec.est_start = Some(v.to_string());
        } else if let Some(v) = field(&line, "Exit_status") {
            rec.exit_status = v.parse().ok();
        } else if let Some(v) = field(&line, "obittime") {
            rec.obittime = Some(v.to_string());
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

/// jobfs colour: only nearing the request is a problem (writes start failing);
/// low usage is normal (jobfs is often only touched at peaks), so no yellow.
fn jobfs_color(pct: f64) -> &'static str {
    if pct >= 90.0 {
        RED
    } else {
        GREEN
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

/// Eighth-block meter: `frac` (0..=1) filled, coloured `color`. A tiny but
/// nonzero fraction always draws at least a sliver (matching qarray's
/// never-round-away rule), so "barely used" and "unused" look different.
fn meter(frac: f64, color: &str) -> String {
    let frac = frac.clamp(0.0, 1.0);
    let total = (BAR_WIDTH * 8) as f64;
    let mut filled = (total * frac).round() as i64;
    if frac > 0.0 && filled == 0 {
        filled = 1;
    }
    let mut bar = String::from("▕");
    for i in 0..BAR_WIDTH as i64 {
        let cell = (filled - i * 8).clamp(0, 8);
        let glyph = if cell >= 8 { "▇" } else { BLOCKS[cell as usize] };
        bar.push_str(&wrap(glyph, color));
    }
    bar.push('▏');
    bar
}

/// Walltime meter, coloured by how close the job is to its limit.
fn time_bar(used: i64, req: i64) -> String {
    let frac = if req > 0 { used as f64 / req as f64 } else { 0.0 };
    meter(frac, time_color(frac * 100.0))
}

/// Fullness colour for quota-style meters (SU, storage space, inodes): red
/// when nearly exhausted, yellow when getting close, green otherwise.
fn fullness_color(pct: f64) -> &'static str {
    if pct >= 90.0 {
        RED
    } else if pct >= 75.0 {
        YELLOW
    } else {
        GREEN
    }
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

/// "Tue Jul 21 03:00:00 2026" -> "Tue 03:00" (estimated starts are near-term).
fn fmt_est(raw: &str) -> String {
    let t: Vec<&str> = raw.split_whitespace().collect();
    match (t.first(), t.get(3)) {
        (Some(day), Some(time)) if time.len() >= 5 => format!("{day} {}", &time[..5]),
        _ => raw.to_string(),
    }
}

/// "Mon Jul 20 13:29:34 2026" -> "Jul 20 13:29".
fn fmt_obit(raw: &str) -> String {
    let t: Vec<&str> = raw.split_whitespace().collect();
    match (t.get(1), t.get(2), t.get(3)) {
        (Some(mon), Some(day), Some(time)) if time.len() >= 5 => {
            format!("{mon} {day:>2} {}", &time[..5])
        }
        _ => raw.to_string(),
    }
}

/// Sortable key for a ctime-style date string: (year, month, day, "HH:MM:SS").
fn date_key(raw: &str) -> (i32, u8, u8, String) {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let t: Vec<&str> = raw.split_whitespace().collect();
    let year = t.get(4).and_then(|y| y.parse().ok()).unwrap_or(0);
    let mon = t
        .get(1)
        .and_then(|m| MONTHS.iter().position(|x| x == m))
        .map(|i| i as u8 + 1)
        .unwrap_or(0);
    let day = t.get(2).and_then(|d| d.parse().ok()).unwrap_or(0);
    let time = t.get(3).map(|s| s.to_string()).unwrap_or_default();
    (year, mon, day, time)
}

/// The running-jobs usage table (header + one aligned row per running record).
fn running_table(recs: &[&UsageRec]) -> String {
    let any_gpu = recs.iter().any(|r| r.ngpus > 0);
    // Show jobfs only when someone asked for a meaningful amount (>= 1GiB);
    // every Gadi job gets a token default allocation that would just be noise.
    let any_jobfs = recs
        .iter()
        .any(|r| r.req_jobfs_kb.map(|q| q >= 1024.0 * 1024.0).unwrap_or(false));
    let opt_pct = |p: Option<i64>| p.map(|v| format!("{v}%")).unwrap_or_else(|| "--".into());

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
        jobfs: String,
        jobfs_pct: f64,
        cpu_pct: Option<i64>,
        eff_pct: Option<i64>,
        gpu_pct: Option<i64>,
        time: String,
        bar: String,
    }
    let ratio_cell = |used: Option<f64>, req: Option<f64>| -> (String, f64) {
        match (used, req) {
            (Some(u), Some(q)) if q > 0.0 => {
                (format!("{}/{}", fmt_gb(u), fmt_gb(q)), 100.0 * u / q)
            }
            (Some(u), _) => (format!("{}/--", fmt_gb(u)), 0.0),
            _ => ("--".into(), 0.0),
        }
    };
    let cells: Vec<Cells> = recs
        .iter()
        .map(|r| {
            let cpus = if r.ngpus > 0 {
                format!("{}c+{}g", r.ncpus, r.ngpus)
            } else {
                format!("{}c", r.ncpus)
            };
            let (mem, mem_pct) = ratio_cell(r.used_mem_kb, r.req_mem_kb);
            let (jobfs, jobfs_pct) = ratio_cell(r.used_jobfs_kb, r.req_jobfs_kb);
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
                jobfs,
                jobfs_pct,
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
    let fw = if any_jobfs { w(&|c| c.jobfs.chars().count(), 5) } else { 0 };
    let tw = w(&|c| c.time.chars().count(), 4);

    let mut out = String::new();
    let mut header = format!(
        "{:<iw$}  {:<nw$}  {:<qw$}  {:<hw$}  {:>cw$}  {:>pw$}  {:>ew$}",
        "JOB", "NAME", "QUEUE", "NODES", "CPUS", "CPU%", "EFF%",
    );
    if any_gpu {
        header.push_str(&format!("  {:>gw$}", "GPU%"));
    }
    header.push_str(&format!("  {:>mw$}", "MEM"));
    if any_jobfs {
        header.push_str(&format!("  {:>fw$}", "JOBFS"));
    }
    header.push_str(&format!("  {:<tw$}  WALLTIME", "TIME"));
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
        if any_jobfs {
            line.push_str("  ");
            let fs_padded = format!("{:>fw$}", c.jobfs);
            line.push_str(&if c.jobfs == "--" {
                fs_padded
            } else {
                wrap(&fs_padded, jobfs_color(c.jobfs_pct))
            });
        }
        line.push_str(&format!("  {:<tw$}  ", c.time));
        line.push_str(&c.bar);
        line.push('\n');
        out.push_str(&line);
    }
    out
}

/// The QUEUED block: why each waiting job isn't running, and when the
/// scheduler estimates it will.
fn queued_block(recs: &[&UsageRec]) -> String {
    let mut out = format!("{BOLD}QUEUED{RESET}\n");
    let iw = recs.iter().map(|r| r.id.chars().count()).max().unwrap_or(3);
    let nw = recs.iter().map(|r| r.name.chars().count()).max().unwrap_or(4);
    for r in recs {
        let est = match &r.est_start {
            Some(e) => wrap(&format!("est {}", fmt_est(e)), GREEN),
            None => format!("est {}", "--"),
        };
        let mut comment = r.comment.clone().unwrap_or_default();
        if comment.chars().count() > 90 {
            comment = comment.chars().take(89).collect::<String>() + "…";
        }
        out.push_str(&format!(
            "{:<iw$}  {}  {:<nw$}  {est}  {DIM}{comment}{RESET}\n",
            r.id, r.state, r.name,
        ));
    }
    out
}

/// A parsed `pbsnodes <host…>` record (Gadi nodes are single-vnode).
#[derive(Debug, Clone, Default)]
pub struct NodeRec {
    pub name: String,
    pub state: String,
    pub jobs: Vec<String>, // job numbers (server suffix and slot stripped)
    pub avail_cpus: i64,
    pub used_cpus: i64,
    pub avail_mem_kb: f64,
    pub used_mem_kb: f64,
    pub avail_gpus: i64,
}

/// Parse `pbsnodes` output: blank-line-separated records, name unindented.
pub fn parse_nodes(raw: &str) -> Vec<NodeRec> {
    let mut nodes: Vec<NodeRec> = Vec::new();
    for block in raw.split("\n\n") {
        let mut lines = block.lines().filter(|l| !l.trim().is_empty());
        let Some(name) = lines.next() else { continue };
        if name.starts_with(' ') || name.trim().is_empty() {
            continue;
        }
        let mut n = NodeRec { name: name.trim().to_string(), ..Default::default() };
        for line in lines {
            if let Some(v) = field(line, "state") {
                n.state = v.to_string();
            } else if let Some(v) = field(line, "jobs") {
                let mut seen: Vec<String> = Vec::new();
                for ent in v.split(',') {
                    let id = ent.trim().split('/').next().unwrap_or("");
                    let num = strip_server(id);
                    if !num.is_empty() && !seen.contains(&num) {
                        seen.push(num);
                    }
                }
                n.jobs = seen;
            } else if let Some(v) = field(line, "resources_available.ncpus") {
                n.avail_cpus = v.parse().unwrap_or(0);
            } else if let Some(v) = field(line, "resources_assigned.ncpus") {
                n.used_cpus = v.parse().unwrap_or(0);
            } else if let Some(v) = field(line, "resources_available.mem") {
                n.avail_mem_kb = parse_mem(v).unwrap_or(0.0);
            } else if let Some(v) = field(line, "resources_assigned.mem") {
                n.used_mem_kb = parse_mem(v).unwrap_or(0.0);
            } else if let Some(v) = field(line, "resources_available.ngpus") {
                n.avail_gpus = v.parse().unwrap_or(0);
            }
        }
        nodes.push(n);
    }
    nodes
}

/// The NODES block: state and occupancy of each node your jobs are on.
/// `mine` is the set of your job numbers (server-stripped, array index kept).
fn nodes_block(nodes: &[NodeRec], mine: &[String]) -> String {
    let mut out = format!("{BOLD}NODES{RESET}\n");
    let short = |n: &str| n.strip_prefix("gadi-").unwrap_or(n).to_string();
    let nw = nodes.iter().map(|n| short(&n.name).chars().count()).max().unwrap_or(4);
    let sw = nodes.iter().map(|n| n.state.chars().count()).max().unwrap_or(4);
    for n in nodes {
        let state_col = if n.state.contains("down") || n.state.contains("offline") {
            RED
        } else if n.state.contains("free") {
            GREEN
        } else {
            YELLOW // job-busy etc.
        };
        let my_count = n.jobs.iter().filter(|j| mine.contains(j)).count();
        let cpus = format!("{}/{}c", n.used_cpus, n.avail_cpus);
        let mem = format!("{}/{}", fmt_gb(n.used_mem_kb), fmt_gb(n.avail_mem_kb));
        let gpus = if n.avail_gpus > 0 { format!("  {}g", n.avail_gpus) } else { String::new() };
        out.push_str(&format!(
            "{:<nw$}  {}  {:>7}  {:>13}{}  {} jobs · {} mine\n",
            short(&n.name),
            wrap(&format!("{:<sw$}", n.state), state_col),
            cpus,
            mem,
            gpus,
            n.jobs.len(),
            my_count,
        ));
    }
    out
}

/// The queue-pressure footer: running/queued counts (from `qstat -Q`) for the
/// queues your jobs are in.
fn queues_line(jobs: &[Job], qstat_q: &str) -> Option<String> {
    let mut my_queues: Vec<&str> = jobs.iter().map(|j| j.queue.as_str()).collect();
    my_queues.sort();
    my_queues.dedup();
    if my_queues.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for line in qstat_q.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        // Queue Max Tot Ena Str Que Run Hld Wat Trn Ext Type
        if f.len() < 12 || !my_queues.contains(&f[0]) {
            continue;
        }
        let (que, run) = (f[5], f[6]);
        parts.push(format!("{} {run}R/{que}Q", f[0].trim_end_matches("-exec")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("{DIM}queues: {}{RESET}", parts.join(" · ")))
    }
}

/// Render the whole Job Usage panel: summary, running table, queued diagnosis,
/// node states, queue pressure.
pub fn render_full(
    jobs: &[Job],
    qstat_f: &str,
    truncated: usize,
    nodes_raw: Option<&str>,
    queues_raw: Option<&str>,
) -> String {
    let mut out = summary_line(jobs);
    out.push('\n');

    let recs = parse_usage(qstat_f);
    let running: Vec<&UsageRec> = recs.iter().filter(|r| r.is_running()).collect();
    let waiting: Vec<&UsageRec> = recs.iter().filter(|r| r.is_waiting()).collect();

    if running.is_empty() {
        out.push_str(&format!("{DIM}No running jobs.{RESET}\n"));
    } else {
        out.push('\n');
        out.push_str(&running_table(&running));
        if truncated > 0 {
            out.push_str(&format!(
                "{DIM}… {truncated} more running jobs not shown (usage detail capped){RESET}\n"
            ));
        }
    }

    if !waiting.is_empty() {
        out.push('\n');
        out.push_str(&queued_block(&waiting));
    }

    if let Some(raw) = nodes_raw {
        let nodes = parse_nodes(raw);
        if !nodes.is_empty() {
            let mine: Vec<String> = jobs
                .iter()
                .filter(|j| j.state.is_running())
                .map(|j| strip_server(&j.id))
                .collect();
            out.push('\n');
            out.push_str(&nodes_block(&nodes, &mine));
        }
    }

    if let Some(raw) = queues_raw {
        if let Some(line) = queues_line(jobs, raw) {
            out.push('\n');
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Back-compat single-purpose renderer (summary + running table only).
pub fn render(jobs: &[Job], qstat_f: &str, truncated: usize) -> String {
    render_full(jobs, qstat_f, truncated, None, None)
}

/// Render the RECENT JOBS panel from `qstat -fx <ids…>` records: newest first,
/// exit status colour-coded. `None` when there are no finished records.
pub fn render_recent(qstat_fx: &str) -> Option<String> {
    let mut recs: Vec<UsageRec> = parse_usage(qstat_fx)
        .into_iter()
        .filter(|r| r.state == 'F')
        .collect();
    if recs.is_empty() {
        return None;
    }
    recs.sort_by(|a, b| {
        let (ka, kb) = (
            a.obittime.as_deref().map(date_key).unwrap_or_default(),
            b.obittime.as_deref().map(date_key).unwrap_or_default(),
        );
        kb.cmp(&ka) // newest first
    });

    let exit_cell = |r: &UsageRec| -> (String, &'static str) {
        match r.exit_status {
            None => ("--".into(), DIM),
            Some(0) => ("ok".into(), GREEN),
            Some(n) if n >= 256 => (format!("sig {}", n - 256), RED),
            Some(n) if n < 0 => (format!("pbs {n}"), RED),
            Some(n) => (format!("exit {n}"), RED),
        }
    };
    let wall = |r: &UsageRec| r.used_wall_sec.map(fmt_hm).unwrap_or_else(|| "--".into());
    let obit = |r: &UsageRec| r.obittime.as_deref().map(fmt_obit).unwrap_or_default();

    let iw = recs.iter().map(|r| r.id.chars().count()).max().unwrap_or(3);
    let nw = recs.iter().map(|r| r.name.chars().count()).max().unwrap_or(4);
    let qw = recs.iter().map(|r| r.queue.chars().count()).max().unwrap_or(5);
    let ww = recs.iter().map(|r| wall(r).chars().count()).max().unwrap_or(4);
    let ew = recs.iter().map(|r| exit_cell(r).0.chars().count()).max().unwrap_or(2);

    let mut out = String::new();
    for r in &recs {
        let (exit, color) = exit_cell(r);
        out.push_str(&format!(
            "{:<iw$}  {:<nw$}  {:<qw$}  {:>ww$}  {}  {DIM}{}{RESET}\n",
            r.id,
            r.name,
            r.queue,
            wall(r),
            wrap(&format!("{:<ew$}", exit), color),
            obit(r),
        ));
    }
    Some(out.trim_end().to_string())
}

/// SU units ("SU"/"KSU"/"MSU") to plain SUs.
fn parse_su(val: &str, unit: &str) -> Option<f64> {
    let v: f64 = val.parse().ok()?;
    match unit {
        "SU" => Some(v),
        "KSU" => Some(v * 1e3),
        "MSU" => Some(v * 1e6),
        _ => None,
    }
}

fn fmt_su(v: f64) -> String {
    if v >= 1e6 {
        format!("{:.2}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.1}K", v / 1e3)
    } else {
        format!("{:.1}", v)
    }
}

/// "237.79 MiB"-style (value, unit) to bytes.
fn size_to_bytes(val: &str, unit: &str) -> Option<f64> {
    let v: f64 = val.parse().ok()?;
    let mult: f64 = match unit {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0f64.powi(2),
        "GiB" => 1024.0f64.powi(3),
        "TiB" => 1024.0f64.powi(4),
        "PiB" => 1024.0f64.powi(5),
        _ => return None,
    };
    Some(v * mult)
}

fn fmt_bytes(b: f64) -> String {
    let (val, unit) = if b >= 1024.0f64.powi(4) {
        (b / 1024.0f64.powi(4), "T")
    } else if b >= 1024.0f64.powi(3) {
        (b / 1024.0f64.powi(3), "G")
    } else if b >= 1024.0f64.powi(2) {
        (b / 1024.0f64.powi(2), "M")
    } else if b >= 1024.0 {
        (b / 1024.0, "K")
    } else {
        (b, "B")
    };
    format!("{val:.1}{unit}")
}

fn fmt_count(c: f64) -> String {
    if c >= 1e6 {
        format!("{:.1}M", c / 1e6)
    } else if c >= 1e3 {
        format!("{:.1}K", c / 1e3)
    } else {
        format!("{}", c as i64)
    }
}

/// Parse a token stream of `value [unit] value [unit] …` where unit tokens are
/// optional (counts may be bare numbers). Returns (numeric value, multiplier
/// applied) pairs resolved to (bytes-or-count) f64s.
fn number_unit_pairs(tokens: &[&str]) -> Vec<f64> {
    let mut vals: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if let Ok(v) = tokens[i].parse::<f64>() {
            let mut val = v;
            if let Some(unit) = tokens.get(i + 1) {
                if let Some(b) = size_to_bytes("1", unit) {
                    val = v * b;
                    i += 1;
                } else if *unit == "K" {
                    val = v * 1e3;
                    i += 1;
                } else if *unit == "M" {
                    val = v * 1e6;
                    i += 1;
                }
            }
            vals.push(val);
        }
        i += 1;
    }
    vals
}

/// The user's home-directory quota, parsed from `quota` output (raw 1K blocks
/// and inode counts; no `-s`, so no ambiguous humanised suffixes).
#[derive(Debug, Clone, PartialEq)]
pub struct HomeQuota {
    pub used_kb: f64,
    pub quota_kb: f64,
    pub files: f64,
    /// None when the inode quota is absent or the "effectively unlimited"
    /// sentinel (~2^32 on Gadi's home filesystem).
    pub files_quota: Option<f64>,
}

/// Find the `/home` row in `quota` output. Long filesystem names sit on their
/// own line with the values on the next; over-quota rows tag values with `*`
/// and insert a grace column ("6days"), so values are collected numerically
/// rather than positionally.
pub fn parse_home_quota(raw: &str) -> Option<HomeQuota> {
    let mut lines = raw.lines();
    while let Some(line) = lines.next() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.is_empty() || t[0] == "Filesystem" || line.starts_with("Disk quotas") {
            continue;
        }
        if !t[0].contains("home") {
            continue;
        }
        // Values share the fs line, or sit on the next when the name is long.
        let values: Vec<&str> = if t.len() > 1 {
            t[1..].to_vec()
        } else {
            match lines.next() {
                Some(v) => v.split_whitespace().collect(),
                None => continue,
            }
        };
        let nums: Vec<f64> = values
            .iter()
            .map(|s| s.trim_end_matches('*'))
            .filter_map(|s| s.parse::<f64>().ok())
            .collect();
        if nums.len() < 6 {
            continue;
        }
        let quota_kb = if nums[1] > 0.0 { nums[1] } else { nums[2] };
        let fq = if nums[4] > 0.0 { nums[4] } else { nums[5] };
        let files_quota = if fq <= 0.0 || fq >= 1e9 { None } else { Some(fq) };
        return Some(HomeQuota { used_kb: nums[0], quota_kb, files: nums[3], files_quota });
    }
    None
}

/// Render the SU + storage quota chart: a meter row for the quarter's SUs
/// (from `nci_account -P <proj>`), then one row per filesystem — home (from
/// `quota`) first, then the project filesystems — each with a space meter
/// and, where a real inode quota exists, an inode meter:
///
/// ```text
/// SU (2026.q3)  ▕▁         ▏   0% · 299.0K of 299.0K avail
/// home          ▕▇▇▇▇▇▇▇   ▏  70% · 7.0G/10.0G     files 90.6K
/// scratch       ▕▁         ▏   1% · 8.6G/1.0T      files ▕▇▇▄  ▏ 25% · 50.1K/202.0K
/// ```
///
/// `None` when neither source yields anything chartable.
pub fn render_account(raw: &str, quota_raw: Option<&str>) -> Option<String> {
    let mut period = String::new();
    let mut grant: Option<f64> = None;
    let mut used: Option<f64> = None;
    let mut avail: Option<f64> = None;
    // name, used bytes, files used, alloc bytes, inode quota (None = unlimited)
    let mut storage: Vec<(String, f64, f64, f64, Option<f64>)> = Vec::new();
    let mut in_storage = false;

    if let Some(home) = quota_raw.and_then(parse_home_quota) {
        storage.push((
            "home".to_string(),
            home.used_kb * 1024.0,
            home.files,
            home.quota_kb * 1024.0,
            home.files_quota,
        ));
    }

    for line in raw.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if let Some(p) = line.split("Period=").nth(1) {
            period = p.trim().to_string();
        }
        let su_of = |t: &[&str]| -> Option<f64> {
            match (t.get(1), t.get(2)) {
                (Some(v), Some(u)) => parse_su(v, u),
                _ => None,
            }
        };
        match t.first() {
            Some(&"Grant:") => grant = su_of(&t),
            Some(&"Used:") => used = su_of(&t),
            Some(&"Avail:") => avail = su_of(&t),
            Some(&"Filesystem") => in_storage = true,
            Some(name) if in_storage && !name.starts_with('=') => {
                let vals = number_unit_pairs(&t[1..]);
                if vals.len() == 4 {
                    // Normalise gdata6/scratch1 -> gdata/scratch.
                    let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
                    storage.push((base.to_string(), vals[0], vals[1], vals[2], Some(vals[3])));
                }
            }
            _ => {}
        }
    }

    if grant.is_none() && storage.is_empty() {
        return None;
    }

    // One meter row per metric; labels share a padded column so the bars line
    // up into a small chart.
    let su_label = if period.is_empty() { "SU".to_string() } else { format!("SU ({period})") };
    let lw = storage
        .iter()
        .map(|(n, ..)| n.chars().count())
        .chain(grant.iter().map(|_| su_label.chars().count()))
        .max()
        .unwrap_or(2);

    let pct_cell = |frac: f64, color: &str| wrap(&format!("{:>3}%", (frac * 100.0).round() as i64), color);
    let mut rows: Vec<String> = Vec::new();

    if let Some(grant) = grant {
        let used_su = used.or_else(|| avail.map(|a| (grant - a).max(0.0))).unwrap_or(0.0);
        let su_frac = if grant > 0.0 { used_su / grant } else { 0.0 };
        let su_color = fullness_color(su_frac * 100.0);
        rows.push(format!(
            "{BOLD}{su_label:<lw$}{RESET}  {} {} · {} of {} avail",
            meter(su_frac, su_color),
            pct_cell(su_frac, su_color),
            avail.map(fmt_su).unwrap_or_else(|| "?".into()),
            fmt_su(grant),
        ));
    }

    let size_cell = |u: f64, a: f64| format!("{}/{}", fmt_bytes(u), fmt_bytes(a));
    let sw = storage
        .iter()
        .map(|(_, u, _, a, _)| size_cell(*u, *a).chars().count())
        .max()
        .unwrap_or(0);
    for (name, used, iused, alloc, ialloc) in &storage {
        let sfrac = if *alloc > 0.0 { used / alloc } else { 0.0 };
        let sc = fullness_color(sfrac * 100.0);
        let mut row = format!(
            "{BOLD}{name:<lw$}{RESET}  {} {} · {:<sw$}  ",
            meter(sfrac, sc),
            pct_cell(sfrac, sc),
            size_cell(*used, *alloc),
        );
        match ialloc {
            Some(ia) if *ia > 0.0 => {
                let ifrac = iused / ia;
                let ic = fullness_color(ifrac * 100.0);
                row.push_str(&format!(
                    "{DIM}files{RESET} {} {} · {}/{}",
                    meter(ifrac, ic),
                    pct_cell(ifrac, ic),
                    fmt_count(*iused),
                    fmt_count(*ia),
                ));
            }
            // No (finite) inode quota: show the count alone, no meter.
            _ => row.push_str(&format!("{DIM}files{RESET} {}", fmt_count(*iused))),
        }
        rows.push(row);
    }
    Some(rows.join("\n"))
}

/// Render the scratch-expiry warning from `nci-file-expiry list-warnings`
/// output. `None` when nothing is scheduled to expire.
pub fn render_expiry(raw: &str) -> Option<String> {
    let rows = raw
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.contains("EXPIRES AT"))
        .count();
    if rows == 0 {
        None
    } else {
        Some(wrap(
            &format!(
                "⚠ {rows} scratch path{} scheduled for expiry — run 'nci-file-expiry list-warnings'",
                if rows == 1 { "" } else { "s" }
            ),
            RED,
        ))
    }
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

    const QUEUED: &str = "\
Job Id: 174187629.gadi-pbs
    Job_Name = bigjob
    job_state = Q
    queue = normal-exec
    Resource_List.ncpus = 16
    Resource_List.mem = 68719476736b
    Resource_List.walltime = 48:00:00
    comment = Not Running: Insufficient amount of resource: ncpus
    estimated.start_time = Tue Jul 21 03:00:00 2026
";

    const FINISHED: &str = "\
Job Id: 174190352.gadi-pbs
    Job_Name = montest
    job_state = F
    queue = copyq-exec
    resources_used.walltime = 00:01:32
    obittime = Mon Jul 20 13:29:34 2026
    Exit_status = 271

Job Id: 174170283.gadi-pbs
    Job_Name = train-gpu
    job_state = F
    queue = gpuvolta-exec
    resources_used.walltime = 00:05:16
    obittime = Mon Jul 20 10:15:50 2026
    Exit_status = 0
";

    #[test]
    fn parses_real_gadi_record() {
        let recs = parse_usage(RECORD);
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.id, "174170283");
        assert_eq!(r.name, "train-gpu");
        assert_eq!(r.queue, "gpuvolta"); // -exec suffix trimmed
        assert_eq!(r.state, 'R');
        assert!(r.is_running());
        assert_eq!(r.nodes, "gpu-v100-0100"); // gadi- prefix trimmed
        assert_eq!(r.hosts, vec!["gadi-gpu-v100-0100"]);
        assert_eq!(r.ncpus, 12);
        assert_eq!(r.ngpus, 1);
        assert_eq!(r.cpupercent, Some(9));
        assert_eq!(r.cput_sec, Some(18));
        assert_eq!(r.used_wall_sec, Some(316));
        assert_eq!(r.req_wall_sec, Some(3600));
        // Resource_List.mem is bytes: 96636764160b = 94371840kb = 90GB.
        assert_eq!(r.req_mem_kb, Some(94371840.0));
        assert_eq!(r.used_mem_kb, Some(2323996.0));
        // jobfs: 107374182400b = 100GB requested, 1484b used.
        assert_eq!(r.req_jobfs_kb, Some(104857600.0));
        assert!(r.used_jobfs_kb.unwrap() < 2.0);
        assert_eq!(r.gpu_util, Some(0));
    }

    #[test]
    fn parses_queued_diagnosis() {
        let recs = parse_usage(QUEUED);
        let r = &recs[0];
        assert_eq!(r.state, 'Q');
        assert!(r.is_waiting());
        assert!(r.comment.as_deref().unwrap().starts_with("Not Running"));
        assert_eq!(fmt_est(r.est_start.as_deref().unwrap()), "Tue 03:00");
    }

    #[test]
    fn unwraps_multinode_exec_host() {
        let recs = parse_usage(MULTI);
        assert_eq!(recs.len(), 1);
        // 4 hosts, the 3rd wrapped across a tab continuation: first + 3 others.
        assert_eq!(recs[0].nodes, "cpu-clx-0001+3");
        assert_eq!(recs[0].hosts.len(), 4);
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
    fn renders_summary_rows_and_queued_block() {
        let jobs = parse_qstat(
            "174170283.gadi-pbs bh5941 gpuvolta-exec train-gpu 123 1 12 90gb 01:00 R 00:05 gadi-gpu-v100-0100/0*12\n\
             174187629.gadi-pbs bh5941 normal-exec bigjob -- 1 16 64gb 48:00 Q -- --\n",
        );
        let detail = format!("{RECORD}\n{QUEUED}");
        let out = render_full(&jobs, &detail, 0, None, None);
        assert!(out.contains("1 running"));
        assert!(out.contains("(12 cores)"));
        assert!(out.contains("1 queued/held"));
        assert!(out.contains("(16 cores)"));
        assert!(out.contains("174170283")); // id, server-stripped
        assert!(!out.contains("gadi-pbs")); // …completely
        assert!(out.contains("2.2G/90.0G")); // used/req mem
        assert!(out.contains("JOBFS")); // 100GB jobfs requested -> column shown
        assert!(out.contains("100.0G")); // requested jobfs rendered
        assert!(out.contains("00:05/01:00")); // used/req walltime
        assert!(out.contains("GPU%")); // gpu column present for a gpu job
        assert!(out.contains("12c+1g"));
        assert!(out.contains('▕')); // walltime bar drawn
        // Queued diagnosis block.
        assert!(out.contains("QUEUED"));
        assert!(out.contains("est Tue 03:00"));
        assert!(out.contains("Not Running: Insufficient amount of resource"));
    }

    #[test]
    fn jobfs_column_hidden_for_token_requests() {
        let jobs = parse_qstat(
            "174200000.gadi-pbs bh5941 normal-exec big-mpi 1 4 96 190gb 10:00 R 05:30 gadi-cpu-clx-0001/0*48\n",
        );
        // MULTI requests no jobfs at all -> no column.
        let out = render_full(&jobs, MULTI, 0, None, None);
        assert!(!out.contains("JOBFS"));
    }

    #[test]
    fn renders_no_running_and_truncation() {
        let jobs = parse_qstat(
            "174187629.gadi-pbs bh5941 normal-exec bigjob -- 1 16 64gb 48:00 Q -- --\n",
        );
        let out = render_full(&jobs, "", 0, None, None);
        assert!(out.contains("0 running"));
        assert!(out.contains("No running jobs."));

        let out2 = render_full(&jobs, MULTI, 7, None, None);
        assert!(out2.contains("7 more running jobs not shown"));
        assert!(!out2.contains("GPU%")); // no gpu jobs -> no gpu column
    }

    // Abridged from a real `pbsnodes gadi-dm-01` record.
    const NODES: &str = "\
gadi-dm-01
     Mom = gadi-dm-01.gadi.nci.org.au
     ntype = PBS
     state = free
     pcpus = 48
     jobs = 174171907.gadi-pbs/0, 174190352.gadi-pbs/1, 174171907.gadi-pbs/5
     resources_available.mem = 201326592kb
     resources_available.ncpus = 48
     resources_available.ngpus = 0
     resources_assigned.mem = 167772160kb
     resources_assigned.ncpus = 29
     queue = copyq-exec

gadi-gpu-v100-0100
     state = job-busy
     jobs = 174170283.gadi-pbs/0
     resources_available.mem = 402653184kb
     resources_available.ncpus = 48
     resources_available.ngpus = 4
     resources_assigned.mem = 94371840kb
     resources_assigned.ncpus = 12
";

    #[test]
    fn parses_and_renders_nodes() {
        let nodes = parse_nodes(NODES);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].name, "gadi-dm-01");
        assert_eq!(nodes[0].state, "free");
        // duplicate job entries deduped: 2 distinct jobs on dm-01
        assert_eq!(nodes[0].jobs, vec!["174171907", "174190352"]);
        assert_eq!(nodes[0].avail_cpus, 48);
        assert_eq!(nodes[0].used_cpus, 29);
        assert_eq!(nodes[1].avail_gpus, 4);

        let out = nodes_block(&nodes, &["174190352".to_string()]);
        assert!(out.contains("dm-01")); // gadi- prefix trimmed
        assert!(out.contains("29/48c"));
        assert!(out.contains("160.0G/192.0G"));
        assert!(out.contains("2 jobs · 1 mine"));
        assert!(out.contains("4g")); // gpu node shows its gpus
        assert!(out.contains("free") && out.contains("job-busy"));
    }

    #[test]
    fn queue_pressure_from_qstat_q() {
        let jobs = parse_qstat(
            "174190352.gadi-pbs bh5941 copyq-exec montest 1 1 1 1gb 00:08 R 00:01 gadi-dm-01/0\n",
        );
        let q = "\
Queue              Max   Tot Ena Str   Que   Run   Hld   Wat   Trn   Ext Type
---------------- ----- ----- --- --- ----- ----- ----- ----- ----- ----- ----
normal-exec          0  3762 yes yes   659  2867   220     0     0     6 Exe*
copyq-exec           0    71 yes yes     2    67     2     0     0     0 Exe*
";
        let line = queues_line(&jobs, q).unwrap();
        assert!(line.contains("copyq 67R/2Q")); // -exec trimmed, Run/Que shown
        assert!(!line.contains("normal")); // not one of my queues
        assert_eq!(queues_line(&[], q), None);
    }

    #[test]
    fn renders_recent_sorted_and_coloured() {
        let out = render_recent(FINISHED).unwrap();
        let first = out.lines().next().unwrap();
        // montest finished later (13:29 vs 10:15) -> listed first.
        assert!(first.contains("174190352"));
        assert!(out.contains("sig 15")); // 271 = 256 + SIGTERM
        assert!(out.contains("ok")); // exit 0
        assert!(out.contains("Jul 20 13:29"));
        assert!(out.contains("00:01")); // walltime used
        assert_eq!(render_recent(RECORD), None); // running record -> no panel
        assert_eq!(render_recent(""), None);
    }

    // Real `nci_account -P xr78` output shape.
    const ACCOUNT: &str = "
Usage Report: Project=xr78 Period=2026.q3
=============================================================
    Grant:   299.00 KSU
     Used:    10.31 SU
 Reserved:     0.00 SU
    Avail:   298.99 KSU

Storage Usage Report: Project=xr78
=============================================================
Filesystem        Used     iUsed    Allocation    iAllocation
gdata6       237.79 MiB    9.31 K    10.00 GiB        36.00 K
scratch1       8.61 GiB   50.14 K     1.00 TiB       202.00 K
=============================================================
";

    // Real `quota` output shape on Gadi: long fs name on its own line, values
    // on the next, inode quota at the ~2^32 "unlimited" sentinel.
    const QUOTA: &str = "\
Disk quotas for user bh5941 (uid 27177):
     Filesystem  blocks   quota   limit   grace   files   quota   limit   grace
gadi-home-fas.gadi.nci.org.au:/home
                7302144  10485760 10485760           90648  4294967294 4294967294
";

    #[test]
    fn parses_home_quota_and_renders_row() {
        let h = parse_home_quota(QUOTA).unwrap();
        assert_eq!(h.used_kb, 7302144.0);
        assert_eq!(h.quota_kb, 10485760.0);
        assert_eq!(h.files, 90648.0);
        assert_eq!(h.files_quota, None); // 2^32 sentinel -> unlimited

        let out = render_account(ACCOUNT, Some(QUOTA)).unwrap();
        assert_eq!(out.lines().count(), 4); // SU + home + 2 project rows
        let home_row = out.lines().nth(1).unwrap(); // home listed first
        assert!(home_row.contains("home"));
        assert!(home_row.contains("7.0G/10.0G"));
        assert!(home_row.contains("70%"));
        assert!(home_row.contains("90.6K"));
        // Unlimited inode quota: count only, no second meter on the row.
        assert_eq!(home_row.matches('▕').count(), 1);

        // quota alone (nci_account down) still charts the home row.
        let solo = render_account("", Some(QUOTA)).unwrap();
        assert!(solo.contains("home"));
        assert!(!solo.contains("SU ("));
        assert_eq!(parse_home_quota("no such row"), None);
    }

    #[test]
    fn home_quota_over_limit_and_finite_inode_quota() {
        // Short fs name (values on the same line), over quota with '*' markers
        // and a grace column, and a real (finite) inode quota.
        let raw = "\
Disk quotas for user u (uid 1):
     Filesystem  blocks   quota   limit   grace   files   quota   limit   grace
/home/u        10485770* 10485760 10485760 6days  90648  100000 100000
";
        let h = parse_home_quota(raw).unwrap();
        assert_eq!(h.used_kb, 10485770.0);
        assert_eq!(h.files_quota, Some(100000.0));
        let out = render_account("", Some(raw)).unwrap();
        assert!(out.contains("100%"));
        assert!(out.contains(RED));
        // Both the space and the files meters are drawn.
        assert_eq!(out.lines().next().unwrap().matches('▕').count(), 2);
    }

    #[test]
    fn renders_account_su_and_storage() {
        let out = render_account(ACCOUNT, None).unwrap();
        assert!(out.contains("SU (2026.q3)"));
        assert!(out.contains('▕') && out.contains('▏')); // meters drawn
        assert!(out.contains("299.0K of 299.0K avail"));
        // Storage rows normalised (gdata6 -> gdata) and humanised, one meter
        // row per filesystem plus an inode meter.
        assert!(out.contains("gdata"));
        assert!(out.contains("237.8M/10.0G"));
        assert!(out.contains("scratch"));
        assert!(out.contains("8.6G/1.0T"));
        assert!(out.contains("files"));
        assert!(out.contains("50.1K/202.0K"));
        assert_eq!(out.lines().count(), 3); // SU row + 2 filesystem rows
        assert_eq!(render_account("garbage", None), None);
    }

    #[test]
    fn account_meters_colour_and_sliver() {
        // Nearly exhausted SU and nearly full scratch go red.
        let hot = "
Usage Report: Project=xx00 Period=2026.q3
    Grant:   100.00 KSU
     Used:    96.00 KSU
    Avail:     4.00 KSU

Filesystem        Used     iUsed    Allocation    iAllocation
scratch2      950.00 GiB   10.00 K     1.00 TiB       202.00 K
";
        let out = render_account(hot, None).unwrap();
        assert!(out.contains(RED));
        assert!(out.contains("96%"));
        assert!(out.contains("93%")); // 950GiB of 1TiB
        // Barely-used meters still show a sliver rather than an empty bar
        // (ACCOUNT's SU usage is ~0.003%).
        let cool = render_account(ACCOUNT, None).unwrap();
        let su_row = cool.lines().next().unwrap();
        assert!(su_row.contains('▁'));
        assert!(su_row.contains("0%"));
    }

    #[test]
    fn renders_expiry_only_when_rows() {
        assert_eq!(render_expiry("EXPIRES AT           GROUP     SIZE  PATH\n"), None);
        assert_eq!(render_expiry(""), None);
        let out = render_expiry(
            "EXPIRES AT           GROUP     SIZE  PATH\n2026-08-01 00:00:00  xr78      1.2G  /scratch/xr78/bh5941/old\n",
        )
        .unwrap();
        assert!(out.contains("1 scratch path "));
        assert!(out.contains("nci-file-expiry"));
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
        assert_eq!(fmt_obit("Mon Jul 20 13:29:34 2026"), "Jul 20 13:29");
        assert!(date_key("Mon Jul 20 13:29:34 2026") > date_key("Mon Jul 20 10:15:50 2026"));
        assert!(date_key("Fri Jan 2 00:00:00 2027") > date_key("Thu Dec 31 23:59:59 2026"));
        assert_eq!(fmt_su(298990.0), "299.0K");
        assert_eq!(fmt_bytes(1024.0f64.powi(3) * 8.61), "8.6G");
        assert_eq!(fmt_count(50140.0), "50.1K");
    }

    #[test]
    fn colours_by_utilisation() {
        assert_eq!(util_color(95), GREEN);
        assert_eq!(util_color(50), YELLOW);
        assert_eq!(util_color(10), RED);
        assert_eq!(mem_color(95.0), RED); // about to hit the request → killed
        assert_eq!(mem_color(60.0), GREEN);
        assert_eq!(mem_color(10.0), YELLOW); // over-requested → wasted SUs
        assert_eq!(jobfs_color(95.0), RED);
        assert_eq!(jobfs_color(10.0), GREEN); // low jobfs use is normal, not shamed
        assert_eq!(time_color(95.0), RED);
    }

    #[test]
    fn hosts_label_variants() {
        let l = |s: &str| hosts_label(&hosts_of(s));
        assert_eq!(l("gadi-cpu-clx-0001/0*48"), "cpu-clx-0001");
        assert_eq!(
            l("gadi-cpu-clx-0001/0*24+gadi-cpu-clx-0001/1*24+gadi-cpu-clx-0002/0*48"),
            "cpu-clx-0001+1" // duplicate host deduped
        );
        assert_eq!(l(""), "--");
    }
}
