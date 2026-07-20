//! In-process `qarray`: per-array-job progress bars from PBS subjob states.
//!
//! Ported from the physics-cluster monitor, unchanged in logic — Gadi runs the
//! same PBSPro subjob model (`X` finished, `R` running, `Q` queued) — but data
//! is fetched with plain local `qstat` calls (see `pbs::fetch_array_progress`)
//! rather than over SSH.
//!
//! Per array job it prints: `<base>[] [bar] NN% done (c/total)[, NN% active | ETA: …,
//! Avg: …]`. The bar uses green eighth-blocks for completed subjobs (matching the panel
//! title) and blue for running ones (matching the RUNNING JOBS header); the ETA
//! extrapolates from the mean walltime of completed subjobs.

const RESET: &str = "\x1b[0m";
const DONE: &str = "\x1b[0;32m"; // completed subjobs; green to match the panel title
const RUNNING: &str = "\x1b[0;34m"; // running subjobs; blue to match the RUNNING JOBS header
// Eighth blocks for sub-cell resolution; index 0 is a space, 7 is the fullest used.
const BLOCKS: [&str; 8] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇"];

/// True if `id` contains a numeric array index, e.g. `174190001[7].gadi-pbs` (but not
/// the bare master `174190001[].gadi-pbs`).
fn has_array_index(id: &str) -> bool {
    match (id.find('['), id.find(']')) {
        (Some(lb), Some(rb)) => rb > lb + 1 && id[lb + 1..rb].bytes().all(|b| b.is_ascii_digit()),
        _ => false,
    }
}

/// Array-master job ids (`<seq>[].server`) from `qstat -w [-u <user>]` output.
pub fn array_bases(qstat_w: &str) -> Vec<String> {
    let mut bases: Vec<String> = qstat_w
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|id| id.starts_with(|c: char| c.is_ascii_digit()) && id.contains("[]"))
        .map(|id| id.to_string())
        .collect();
    bases.sort();
    bases.dedup();
    bases
}

/// "HH:MM:SS" to seconds; None for "0", "", or anything else (matching the script,
/// which only accumulates real per-subjob walltimes).
fn parse_hms(s: &str) -> Option<i64> {
    let mut it = s.split(':');
    let h: i64 = it.next()?.trim().parse().ok()?;
    let m: i64 = it.next()?.trim().parse().ok()?;
    let sec: i64 = it.next()?.trim().parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(h * 3600 + m * 60 + sec)
}

#[derive(Default)]
struct Stats {
    total: i64,
    completed: i64,
    running: i64,
    queued: i64,
    total_time: i64, // summed walltime of completed subjobs
    time_count: i64, // how many completed subjobs contributed a walltime
}

/// Tally subjob states from `qstat [-w] -t <base>` output. Columns (default format):
/// 1=id, 2=name, 3=user, 4=Time Use, 5=state, 6=queue.
fn tally(detail: &str, base_pattern: &str) -> Stats {
    let mut s = Stats::default();
    for line in detail.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 5 || !f[0].starts_with(base_pattern) || !has_array_index(f[0]) {
            continue;
        }
        s.total += 1;
        match f[4] {
            "X" => {
                s.completed += 1;
                if let Some(t) = parse_hms(f[3]) {
                    s.total_time += t;
                    s.time_count += 1;
                }
            }
            "R" => s.running += 1,
            "Q" => s.queued += 1,
            _ => {}
        }
    }
    s
}

/// Green/blue eighth-block progress bar (width 30) for completed/running subjobs,
/// followed by the done/active percentages.
fn progress_bar(completed: i64, running: i64, total: i64) -> String {
    const WIDTH: i64 = 30;
    const N: i64 = 8; // blocks per cell
    const MAX: i64 = 7; // fullest block index
    let (done_pct, active_pct, filled, mut running_parts) = if total > 0 {
        let active = (completed + running).min(total);
        (
            (completed * 100 + total / 2) / total,
            (active * 100 + total / 2) / total,
            WIDTH * N * completed / total,
            WIDTH * N * running / total,
        )
    } else {
        (0, 0, 0, 0)
    };
    if running > 0 && running_parts == 0 {
        running_parts = 1; // never round a nonzero running count away
    }

    let mut bar = String::from("[");
    for i in 0..WIDTH {
        let pos = i * N;
        let next = (i + 1) * N;
        if next > filled && pos < filled + running_parts {
            let idx = if pos < filled + running_parts - MAX { MAX } else { filled + running_parts - pos };
            bar.push_str(&format!("{RUNNING}{}{RESET}", BLOCKS[idx as usize]));
        } else if pos < filled - MAX {
            bar.push_str(&format!("{DONE}{}{RESET}", BLOCKS[MAX as usize]));
        } else if pos < filled {
            bar.push_str(&format!("{DONE}{}{RESET}", BLOCKS[(filled - pos) as usize]));
        } else {
            bar.push(' ');
        }
    }
    if running > 0 {
        bar.push_str(&format!("] {done_pct:>3}% done ({completed}/{total}), {active_pct:>3}% active"));
    } else {
        bar.push_str(&format!("] {done_pct:>3}% done ({completed}/{total})"));
    }
    bar
}

/// Human duration: `Ns` / `Nm Ns` / `Nh Nm` / `Nd Nh`.
fn format_time(sec: i64) -> String {
    if sec < 60 {
        format!("{sec}s")
    } else if sec < 3600 {
        format!("{}m {}s", sec / 60, sec % 60)
    } else if sec < 86400 {
        format!("{}h {}m", sec / 3600, (sec % 3600) / 60)
    } else {
        format!("{}d {}h", sec / 86400, (sec % 86400) / 3600)
    }
}

/// Render one array job's progress line from its `qstat -t` detail, or None if the
/// detail has no array subjobs (e.g. just the parent).
pub fn render_array(base: &str, detail: &str) -> Option<String> {
    let base_pattern = crate::pbs::job_number(base); // digits before '[' / '.'
    let s = tally(detail, base_pattern);
    if s.total == 0 {
        return None;
    }
    // Show the base without the `.gadi-pbs` suffix; the [] marks it as an array.
    let display = base.split('.').next().unwrap_or(base);
    let mut line = format!("{:<10} {}", display, progress_bar(s.completed, s.running, s.total));

    // ETA from the mean completed-subjob walltime, extrapolated over remaining subjobs
    // (running batch first, then queued/held in batches of `running`).
    if s.completed > 0 && s.running + s.queued > 0 && s.time_count > 0 && s.total_time > 0 {
        let avg = s.total_time / s.time_count;
        let remaining = s.total - s.completed - s.running;
        let est = if s.running > 0 {
            if remaining > 0 {
                let batches = (remaining + s.running - 1) / s.running;
                avg + batches * avg
            } else {
                avg
            }
        } else {
            avg * remaining
        };
        line.push_str(&format!(" | ETA: {}, Avg: {}", format_time(est), format_time(avg)));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_array_bases() {
        let w = "\
Job id                         Name    User      Time Use S Queue
------------------------------ ------  --------  -------- - -----
174170283.gadi-pbs             vasp    bh5941    8875:31* R normal-exec
174190001[].gadi-pbs           seq     bh5941           0 B normal-exec
174190002[].gadi-pbs           seq     bh5941           0 B normal-exec
";
        assert_eq!(
            array_bases(w),
            vec!["174190001[].gadi-pbs", "174190002[].gadi-pbs"]
        );
    }

    const DETAIL: &str = "\
Job id                Name  User    Time Use S Queue
--------------------  ----  ------  -------- - -----
174190001[].gadi-pbs  seq   bh5941         0 B normal-exec
174190001[0].gadi-pbs seq   bh5941  00:10:00 X normal-exec
174190001[1].gadi-pbs seq   bh5941  00:20:00 X normal-exec
174190001[2].gadi-pbs seq   bh5941  00:05:00 R normal-exec
174190001[3].gadi-pbs seq   bh5941         0 Q normal-exec
";

    #[test]
    fn tallies_states_and_times() {
        let s = tally(DETAIL, "174190001");
        assert_eq!(s.total, 4); // [0..3]; the [] master is skipped
        assert_eq!(s.completed, 2);
        assert_eq!(s.running, 1);
        assert_eq!(s.queued, 1);
        assert_eq!(s.total_time, 1800); // 600 + 1200
        assert_eq!(s.time_count, 2);
    }

    #[test]
    fn renders_bar_percentages_and_eta() {
        let out = render_array("174190001[].gadi-pbs", DETAIL).unwrap();
        assert!(out.starts_with("174190001[] ")); // base with server suffix stripped
        assert!(out.contains("50% done (2/4)"));
        assert!(out.contains("75% active"));
        // avg = 1800/2 = 900s = 15m; remaining = 1, running 1 -> 1 batch -> est 1800s = 30m.
        assert!(out.contains("ETA: 30m 0s"));
        assert!(out.contains("Avg: 15m 0s"));
    }

    #[test]
    fn no_subjobs_returns_none() {
        let only_master = "174190001[].gadi-pbs  seq  bh5941  0 B normal-exec\n";
        assert_eq!(render_array("174190001[].gadi-pbs", only_master), None);
    }

    #[test]
    fn all_done_has_no_eta() {
        let d = "\
174190001[0].gadi-pbs  s  u  0 X q
174190001[1].gadi-pbs  s  u  0 X q
";
        let out = render_array("174190001[].gadi-pbs", d).unwrap();
        assert!(out.contains("100% done (2/2)"));
        assert!(!out.contains("ETA"));
    }
}
