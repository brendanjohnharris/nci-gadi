use crate::app::Update;
use crate::pbs::job_number;
use notify::{Event, RecursiveMode, Watcher};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// Make a raw log line safe to render in a ratatui `Paragraph`.
///
/// ratatui measures each character with `unicode-width` and prints the byte
/// verbatim, but it does NOT emulate control characters: a `\t` is width 0 to
/// ratatui yet jumps the real cursor to the next tab stop, and a stray ESC/`\r`
/// makes the terminal move or repaint on its own. Either way ratatui's cell
/// buffer desyncs from the terminal and the frame diff can never repair it ---
/// the garbled, "sticky" log text. We therefore expand tabs to 8-column stops
/// and drop every other control character (whole ANSI escape sequences included)
/// at ingestion, so what reaches the renderer advances the cursor exactly as
/// ratatui expects.
pub fn sanitize_log_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => {
                let spaces = 8 - (col % 8);
                out.extend(std::iter::repeat(' ').take(spaces));
                col += spaces;
            }
            '\u{1b}' => {
                // Drop an ANSI escape sequence. CSI ("ESC [") runs until a final
                // byte in 0x40..=0x7e; a lone ESC just gets dropped.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
            }
            // Drop other control characters (CR, BEL, BS, vertical tab, ...).
            c if c.is_control() => {}
            c => {
                out.push(c);
                // Tab-stop accounting; one column per char is close enough for the
                // ASCII-dominated logs we render (only affects tab alignment, never
                // correctness, since no control bytes survive to desync the cursor).
                col += 1;
            }
        }
    }
    out
}

/// Resolve the log file for a job id under the `~/.jobs` convention (a job
/// script that tees its output to `~/.jobs/$PBS_JOBID.log`).
pub fn resolve_log_path(jobs_dir: &Path, job_id: &str) -> Option<PathBuf> {
    // Array task: "1234[5].server" -> dir "1234[]*", file "<dir>/5.log".
    if let (Some(lb), Some(rb)) = (job_id.find('['), job_id.find(']')) {
        if rb > lb + 1 {
            let task_id = &job_id[lb + 1..rb];
            let num = job_number(job_id);
            let prefix = format!("{}[]", num);
            if let Ok(entries) = fs::read_dir(jobs_dir) {
                for e in entries.flatten() {
                    if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with(&prefix) {
                        let candidate = e.path().join(format!("{}.log", task_id));
                        if candidate.is_file() {
                            return Some(candidate);
                        }
                    }
                }
            }
        }
    }

    // Regular job: glob "<jobnum>*.log".
    let num = job_number(job_id);
    if let Ok(entries) = fs::read_dir(jobs_dir) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(num) && name.ends_with(".log") {
                let p = e.path();
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }

    // Fallback: exact "<full-id>.log".
    let fallback = jobs_dir.join(format!("{}.log", job_id));
    if fallback.is_file() {
        Some(fallback)
    } else {
        None
    }
}

/// Read up to `max_bytes` from the end of the file. If the file is larger, the
/// first (partial) line is dropped so output starts on a line boundary.
/// Returns (text, file_len) where file_len is the byte offset read up to.
pub fn read_tail(path: &Path, max_bytes: u64) -> std::io::Result<(String, u64)> {
    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;
    if start > 0 {
        if let Some(nl) = buf.find('\n') {
            buf = buf[nl + 1..].to_string();
        }
    }
    Ok((buf, len))
}

/// Read bytes appended after `offset`. Returns (text, new_len).
pub fn read_since(path: &Path, offset: u64) -> std::io::Result<(String, u64)> {
    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    if len < offset {
        // File was truncated/rotated: re-read from the start.
        return read_tail(path, TAIL_BYTES);
    }
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;
    Ok((buf, len))
}

// Initial read budget: large enough to load the whole log for any real job, so the
// entire file is navigable (Home/End reach its true top/bottom); only a pathologically
// huge log is tailed to the last 64 MiB.
const TAIL_BYTES: u64 = 64 * 1024 * 1024;

/// qcat re-dumps the whole spool file every poll, so keep only its tail.
const QCAT_TAIL_BYTES: usize = 16 * 1024 * 1024;

/// What the log preview should show for the selected job. Resolved by the main
/// loop (see `main.rs::resolve_target`) in priority order: a tailable file
/// (`~/.jobs` convention, or the PBS `Output_Path` once it exists), else Gadi's
/// `qcat` for the spooled output of a running job, else nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogTarget {
    None,
    File(PathBuf),
    Qcat { jobid: String },
}

/// Shown when there is no selected running job.
fn no_job_message() -> Vec<String> {
    vec!["No running job selected.".into()]
}

/// Shown when a resolved log file cannot be read.
fn unreadable_message(path: &Path) -> Vec<String> {
    vec![
        format!("Could not read {}.", path.display()),
        String::new(),
        "The file may have been removed, or is not readable from this node.".into(),
    ]
}

/// Shown while qcat is failing (job mid-start, mid-exit, or spool unreadable).
fn qcat_error_message(jobid: &str, err: &str) -> Vec<String> {
    let first = err.lines().last().unwrap_or(err).trim().to_string();
    vec![
        format!("qcat could not read the spooled output of {jobid}:"),
        format!("  {first}"),
        String::new(),
        "If the job only just started, the spool may not exist yet.".into(),
        "If the job just finished, PBS is copying the output back — the".into(),
        "preview switches to the .o file automatically once it appears.".into(),
        String::new(),
        "For live output that doesn't depend on qcat, submit with".into(),
        "'#PBS -k oed' (streams the .o/.e files as the job runs) or tee".into(),
        "your output to ~/.jobs/$PBS_JOBID.log in the job script.".into(),
    ]
}

fn pump_log_delta(path: &Path, offset: &mut u64, updates: &Sender<Update>) {
    let old_off = *offset;
    let len = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return,
    };

    // Fast path: no visible growth/truncation; avoid reopening the file.
    if len == old_off {
        return;
    }

    if let Ok((text, new_off)) = read_since(path, old_off) {
        *offset = new_off;

        // On truncate/rotation, replace the preview so old lines do not linger.
        let replace = len < old_off;
        if !text.is_empty() || replace {
            let _ = updates.send(Update::Log {
                lines: text.lines().map(sanitize_log_line).collect(),
                replace,
            });
        }
    }
}

/// Watcher-side state for the active target.
enum Mode {
    Idle,
    File { path: PathBuf, offset: u64 },
    Qcat { jobid: String, last_poll: Option<Instant>, last_text: String },
}

pub fn spawn_log_watcher(updates: Sender<Update>, qcat_interval: Duration) -> Sender<LogTarget> {
    let (target_tx, target_rx): (Sender<LogTarget>, Receiver<LogTarget>) = channel();

    std::thread::spawn(move || {
        // notify delivers filesystem events into ev_rx.
        let (ev_tx, ev_rx) = channel::<notify::Result<Event>>();
        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = ev_tx.send(res);
        }) {
            Ok(w) => w,
            Err(e) => {
                let _ = updates.send(Update::Error {
                    source: "log".into(),
                    message: format!("watcher init failed: {e}"),
                });
                return;
            }
        };

        let mut mode = Mode::Idle;

        loop {
            // 1. Drain any pending target switches (the last one wins).
            loop {
                match target_rx.try_recv() {
                    Ok(target) => {
                        if let Mode::File { path, .. } = &mode {
                            let _ = watcher.unwatch(path);
                        }
                        match target {
                            LogTarget::File(p) => match read_tail(&p, TAIL_BYTES) {
                                Ok((text, off)) => {
                                    let _ = watcher.watch(&p, RecursiveMode::NonRecursive);
                                    let _ = updates.send(Update::Log {
                                        lines: text.lines().map(sanitize_log_line).collect(),
                                        replace: true,
                                    });
                                    mode = Mode::File { path: p, offset: off };
                                }
                                Err(_) => {
                                    let _ = updates.send(Update::Log {
                                        lines: unreadable_message(&p),
                                        replace: true,
                                    });
                                    mode = Mode::Idle;
                                }
                            },
                            LogTarget::Qcat { jobid } => {
                                // Same job again (e.g. its fallback path changed):
                                // keep the shown content and the poll timer.
                                let same = matches!(&mode, Mode::Qcat { jobid: j, .. } if *j == jobid);
                                if !same {
                                    let _ = updates.send(Update::Log {
                                        lines: vec![format!(
                                            "Fetching spooled output of {jobid} via qcat…"
                                        )],
                                        replace: true,
                                    });
                                    mode = Mode::Qcat { jobid, last_poll: None, last_text: String::new() };
                                }
                            }
                            LogTarget::None => {
                                let _ = updates.send(Update::Log {
                                    lines: no_job_message(),
                                    replace: true,
                                });
                                mode = Mode::Idle;
                            }
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                }
            }

            // 2. Wait for a filesystem event (or wake every 250ms to recheck).
            let woke_event = match ev_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Ok(_event)) => true,
                Ok(Err(_)) => false,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            };

            match &mut mode {
                Mode::Idle => {}
                // On an event, and also on the timeout fallback (in case notify
                // misses an event or loses a watch), advance from the length delta.
                Mode::File { path, offset } => {
                    let _ = woke_event; // both paths pump; the delta check is cheap
                    let p = path.clone();
                    pump_log_delta(&p, offset, &updates);
                }
                Mode::Qcat { jobid, last_poll, last_text } => {
                    let due = last_poll.map(|t| t.elapsed() >= qcat_interval).unwrap_or(true);
                    if due {
                        *last_poll = Some(Instant::now());
                        match crate::pbs::qcat_stdout(jobid) {
                            Ok(mut text) => {
                                // Keep only the tail of very large spools, on a
                                // line boundary.
                                if text.len() > QCAT_TAIL_BYTES {
                                    let cut = text.len() - QCAT_TAIL_BYTES;
                                    let cut = text[cut..]
                                        .find('\n')
                                        .map(|nl| cut + nl + 1)
                                        .unwrap_or(cut);
                                    text = text.split_off(cut);
                                }
                                if text != *last_text {
                                    let _ = updates.send(Update::Log {
                                        lines: text.lines().map(sanitize_log_line).collect(),
                                        replace: true,
                                    });
                                    *last_text = text;
                                }
                            }
                            Err(e) => {
                                let msg = e.to_string();
                                if msg != *last_text {
                                    let _ = updates.send(Update::Log {
                                        lines: qcat_error_message(jobid, &msg),
                                        replace: true,
                                    });
                                    *last_text = msg;
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    target_tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(tag: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("monitor_logtest_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn resolves_regular_job_by_glob() {
        let dir = tmpdir("reg");
        fs::write(dir.join("174170283.gadi-pbs.log"), "hello").unwrap();
        let p = resolve_log_path(&dir, "174170283.gadi-pbs").unwrap();
        assert_eq!(p, dir.join("174170283.gadi-pbs.log"));
    }

    #[test]
    fn resolves_array_task_in_bracket_dir() {
        let dir = tmpdir("arr");
        let jobdir = dir.join("174190001[].gadi-pbs");
        fs::create_dir_all(&jobdir).unwrap();
        fs::write(jobdir.join("3.log"), "task3").unwrap();
        let p = resolve_log_path(&dir, "174190001[3].gadi-pbs").unwrap();
        assert_eq!(p, jobdir.join("3.log"));
    }

    #[test]
    fn returns_none_when_absent() {
        let dir = tmpdir("none");
        assert_eq!(resolve_log_path(&dir, "999999999.gadi-pbs"), None);
    }

    #[test]
    fn sanitize_expands_tabs_to_eight_col_stops() {
        // "From worker 3:" is 14 chars; the tab fills to column 16.
        let line = "From worker 3:\t\u{250c} Info: hi";
        let out = sanitize_log_line(line);
        assert_eq!(out, "From worker 3:  \u{250c} Info: hi");
        // No tab or other control byte survives.
        assert!(!out.contains('\t'));
        // Box-drawing characters are not controls and must be preserved.
        assert!(out.contains('\u{250c}'));
    }

    #[test]
    fn sanitize_strips_ansi_and_control_bytes() {
        let line = "\u{1b}[31mERROR\u{1b}[0m boom\r\u{7}";
        let out = sanitize_log_line(line);
        assert_eq!(out, "ERROR boom");
        assert!(!out.contains('\u{1b}'));
        assert!(!out.contains('\r'));
    }

    #[test]
    fn sanitize_tab_stop_resets_each_call() {
        // A leading tab from column 0 expands to a full 8 spaces.
        assert_eq!(sanitize_log_line("\tx"), "        x");
        // Plain text is untouched.
        assert_eq!(sanitize_log_line("plain line"), "plain line");
    }

    #[test]
    fn reads_tail_within_byte_budget() {
        let dir = tmpdir("tail");
        let f = dir.join("a.log");
        let body: String = (0..100).map(|i| format!("line{}\n", i)).collect();
        fs::write(&f, &body).unwrap();
        let (text, off) = read_tail(&f, 40).unwrap();
        assert!(text.ends_with("line99\n"));
        assert!(text.len() <= 40 + 16); // tail of file, partial first line trimmed
        assert_eq!(off, body.len() as u64);
    }

    #[test]
    fn qcat_error_message_names_job_and_hints() {
        let lines = qcat_error_message("174170283.gadi-pbs", "qcat: Job is not running\n");
        assert!(lines[0].contains("174170283.gadi-pbs"));
        assert!(lines.iter().any(|l| l.contains("Job is not running")));
        assert!(lines.iter().any(|l| l.contains("-k oed")));
    }
}
