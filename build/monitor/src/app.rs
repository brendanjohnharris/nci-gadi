use crate::pbs::Job;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopTab {
    Usage,
    LogPreview,
    Details,
    Processes,
}

#[derive(Debug, Clone)]
pub enum Update {
    Jobs(Vec<Job>),
    Usage(String),
    Users(Vec<String>), // fuzzy candidates for the `u` switch (project-group members)
    Array(Option<String>),
    Recent(Option<String>),  // finished-jobs panel (None = no history)
    Account(String),         // SU + storage header lines
    Expiry(Option<String>),  // scratch-expiry warning line (None = nothing expiring)
    Procs { job: String, text: String }, // qps listing for the Processes tab
    Log { lines: Vec<String>, replace: bool },
    Error { source: String, message: String },
    Details { job: String, text: String, output_path: Option<PathBuf> },
}

pub struct App {
    pub jobs: Vec<Job>,
    pub running: Vec<usize>,          // indices into `jobs` with a running state
    pub selected: usize,              // index into `running`
    pub selected_job_id: Option<String>,
    pub usage: String,                // ANSI per-job usage panel (usage::render_full)
    pub usage_at: Option<Instant>,
    pub array: Option<String>,
    pub recent: Option<String>,       // ANSI finished-jobs panel
    pub account: String,              // ANSI SU + storage header (empty until fetched)
    pub expiry: Option<String>,       // ANSI scratch-expiry warning line
    pub jobs_at: Option<Instant>,
    pub log: Vec<String>,
    pub log_scroll: usize,
    pub log_follow: bool,
    pub usage_follow: bool,
    pub details_follow: bool,
    pub last_error: Option<(String, Instant)>,
    pub should_quit: bool,
    pub start: Instant,
    pub top_tab: TopTab,
    pub usage_scroll: usize,
    pub show_array: bool,
    pub show_queued: bool,
    pub show_running: bool,
    pub show_recent: bool,
    pub details: String,
    pub details_job: Option<String>,
    pub details_scroll: usize,
    pub procs: String,                // qps output for the Processes tab
    pub procs_job: Option<String>,    // job the procs pane is showing/fetching
    pub procs_follow: bool,
    pub procs_scroll: usize,
    pub top_inner_h: usize,
    pub user: String,             // currently monitored user
    pub project: String,          // $PROJECT, shown in the status line
    pub input: Option<String>,    // Some(buffer) while the `u` username box is open
    pub known_users: Vec<String>, // fuzzy-match candidates for the box
    pub compact: bool,            // effective array-compaction state (cached by draw)
    pub compact_override: Option<bool>, // Some => user forced e/c; None => auto by fit
    pub output_path: Option<PathBuf>,   // selected job's PBS Output_Path (log fallback)
}

impl App {
    pub fn new() -> App {
        App {
            jobs: Vec::new(),
            running: Vec::new(),
            selected: 0,
            selected_job_id: None,
            usage: String::new(),
            usage_at: None,
            array: None,
            recent: None,
            account: String::new(),
            expiry: None,
            jobs_at: None,
            log: Vec::new(),
            log_scroll: 0,
            log_follow: true,
            usage_follow: true,
            details_follow: true,
            last_error: None,
            should_quit: false,
            start: Instant::now(),
            top_tab: TopTab::Usage,
            usage_scroll: 0,
            show_array: true,
            show_queued: true,
            show_running: true,
            show_recent: true,
            details: String::new(),
            details_job: None,
            details_scroll: 0,
            procs: String::new(),
            procs_job: None,
            procs_follow: true,
            procs_scroll: 0,
            top_inner_h: 0,
            user: String::new(),
            project: String::new(),
            input: None,
            known_users: Vec::new(),
            compact: false,
            compact_override: None,
            output_path: None,
        }
    }

    fn recompute_running(&mut self) {
        self.running = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.state.is_running())
            .map(|(i, _)| i)
            .collect();
        // Re-anchor selection on the previously selected job id if still present.
        if let Some(id) = &self.selected_job_id {
            if let Some(pos) = self.running.iter().position(|&i| &self.jobs[i].id == id) {
                self.selected = pos;
                return;
            }
        }
        if self.selected >= self.running.len() {
            self.selected = 0;
        }
        self.selected_job_id = self.selected_job().map(|j| j.id.clone());
    }

    pub fn apply(&mut self, u: Update) {
        match u {
            Update::Jobs(jobs) => {
                self.jobs = jobs;
                self.recompute_running();
                self.jobs_at = Some(Instant::now());
            }
            Update::Usage(text) => {
                self.usage = text;
                self.usage_at = Some(Instant::now());
            }
            Update::Users(users) => self.known_users = users,
            Update::Array(a) => self.array = a,
            Update::Recent(r) => self.recent = r,
            Update::Account(a) => self.account = a,
            Update::Expiry(e) => self.expiry = e,
            Update::Procs { job, text } => {
                // Guard against a stale response racing a rapid selection change:
                // only the job the pane is currently pointed at may fill it.
                if self.procs_job.as_deref() == Some(job.as_str()) {
                    self.procs = text
                        .lines()
                        .map(crate::logs::sanitize_log_line)
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
            Update::Log { lines, replace } => {
                if replace {
                    self.log = lines;
                } else {
                    self.log.extend(lines);
                }
                // Keep the whole file navigable; only bound memory for a runaway tail.
                let cap = 1_000_000;
                if self.log.len() > cap {
                    let drop = self.log.len() - cap;
                    self.log.drain(0..drop);
                }
            }
            Update::Error { source, message } => {
                self.last_error = Some((format!("{source}: {message}"), Instant::now()));
            }
            Update::Details { job, text, output_path } => {
                // `qstat -f` indents wrapped continuation lines with TABs; rendered
                // raw they desync ratatui's buffer from the terminal and strand
                // "frozen" characters. Sanitize per line like the log tab does.
                self.details = text
                    .lines()
                    .map(crate::logs::sanitize_log_line)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.details_job = Some(job);
                self.details_scroll = 0;
                self.output_path = output_path;
            }
        }
    }

    pub fn next_job(&mut self) {
        if self.running.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.running.len();
        self.on_selection_change();
    }

    pub fn prev_job(&mut self) {
        if self.running.is_empty() {
            return;
        }
        self.selected = (self.selected + self.running.len() - 1) % self.running.len();
        self.on_selection_change();
    }

    /// Shared post-switch bookkeeping: re-anchor the selected id and re-pin the log to
    /// the new job's tail. The active tab is left as-is (switching jobs no longer forces
    /// the Log Preview tab forward).
    fn on_selection_change(&mut self) {
        self.selected_job_id = self.selected_job().map(|j| j.id.clone());
        self.log_follow = true;
        self.log_scroll = 0;
    }

    pub fn selected_job(&self) -> Option<&Job> {
        self.running.get(self.selected).map(|&i| &self.jobs[i])
    }

    pub fn running_count(&self) -> usize {
        self.running.len()
    }

    pub fn selected_ordinal(&self) -> usize {
        if self.running.is_empty() {
            0
        } else {
            self.selected + 1
        }
    }

    pub fn next_tab(&mut self) {
        self.top_tab = match self.top_tab {
            TopTab::Usage => TopTab::LogPreview,
            TopTab::LogPreview => TopTab::Details,
            TopTab::Details => TopTab::Processes,
            TopTab::Processes => TopTab::Usage,
        };
    }

    pub fn prev_tab(&mut self) {
        self.top_tab = match self.top_tab {
            TopTab::Usage => TopTab::Processes,
            TopTab::LogPreview => TopTab::Usage,
            TopTab::Details => TopTab::LogPreview,
            TopTab::Processes => TopTab::Details,
        };
    }

    /// The active tab's (follow flag, scroll offset), as a disjoint mutable pair.
    pub fn active(&mut self) -> (&mut bool, &mut usize) {
        match self.top_tab {
            TopTab::Usage => (&mut self.usage_follow, &mut self.usage_scroll),
            TopTab::LogPreview => (&mut self.log_follow, &mut self.log_scroll),
            TopTab::Details => (&mut self.details_follow, &mut self.details_scroll),
            TopTab::Processes => (&mut self.procs_follow, &mut self.procs_scroll),
        }
    }

    /// Scroll the active tab up/down by `step` lines (turns off tail-follow).
    fn scroll_by(&mut self, up: bool, step: usize) {
        let (follow, scroll) = self.active();
        *follow = false;
        *scroll = if up { scroll.saturating_sub(step) } else { scroll.saturating_add(step) };
    }

    pub fn page_up(&mut self) {
        self.scroll_by(true, self.top_inner_h.max(1));
    }

    pub fn page_down(&mut self) {
        self.scroll_by(false, self.top_inner_h.max(1));
    }

    pub fn scroll_up(&mut self) {
        self.scroll_by(true, 1);
    }

    pub fn scroll_down(&mut self) {
        self.scroll_by(false, 1);
    }

    pub fn scroll_to_top(&mut self) {
        let (follow, scroll) = self.active();
        *follow = false;
        *scroll = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.top_tab == TopTab::Usage {
            // Usage "follows" its top edge (see ui::render_top), so End is a
            // plain jump to the bottom, clamped to the real extent at render.
            let (follow, scroll) = self.active();
            *follow = false;
            *scroll = usize::MAX;
        } else {
            *self.active().0 = true;
        }
    }

    /// Open the username input box with a clean (empty) buffer.
    pub fn begin_user_input(&mut self) {
        self.input = Some(String::new());
    }

    pub fn input_push(&mut self, c: char) {
        if let Some(buf) = self.input.as_mut() {
            buf.push(c);
        }
    }

    pub fn input_backspace(&mut self) {
        if let Some(buf) = self.input.as_mut() {
            buf.pop();
        }
    }

    pub fn cancel_input(&mut self) {
        self.input = None;
    }

    /// The best fuzzy match for the current buffer among `known_users`, if the
    /// buffer is non-empty and isn't already an exact known user. This is the name
    /// shown as a suggestion and adopted on Enter.
    pub fn user_suggestion(&self) -> Option<String> {
        let buf = self.input.as_deref()?.trim();
        if buf.is_empty() || self.known_users.iter().any(|u| u == buf) {
            return None;
        }
        best_user_match(buf, &self.known_users)
    }

    /// Close the box and resolve the final username: the fuzzy match if one exists,
    /// else the raw text (so a user outside your projects can still be typed in
    /// full). Adopt and return it when non-empty and different; otherwise `None`.
    pub fn confirm_input(&mut self) -> Option<String> {
        let suggestion = self.user_suggestion();
        let buf = self.input.take()?;
        let name = suggestion.unwrap_or_else(|| buf.trim().to_string());
        if name.is_empty() || name == self.user {
            return None;
        }
        self.user = name.clone();
        Some(name)
    }

    /// Toggle array-job compaction, flipping from whatever is currently shown (the
    /// effective state cached by the last render) and pinning the choice so it no
    /// longer auto-follows the fit. `e` and `c` both call this.
    pub fn toggle_compact(&mut self) {
        let new = !self.compact;
        self.compact = new;
        self.compact_override = Some(new);
    }

    pub fn toggle_array(&mut self) {
        self.show_array = !self.show_array;
    }

    pub fn toggle_queued(&mut self) {
        self.show_queued = !self.show_queued;
    }

    pub fn toggle_running(&mut self) {
        self.show_running = !self.show_running;
    }

    pub fn toggle_recent(&mut self) {
        self.show_recent = !self.show_recent;
    }
}

/// Case-insensitive fuzzy score of `query` against `cand`; higher is better.
/// `None` if `query` is not a subsequence of `cand`. Rewards a match at the start
/// of the candidate, contiguous runs, and shorter candidates.
fn fuzzy_score(query: &str, cand: &str) -> Option<i32> {
    let q = query.to_lowercase();
    let c = cand.to_lowercase();
    if q.is_empty() {
        return None;
    }
    let cb = c.as_bytes();
    let mut ci = 0usize;
    let mut score = 0i32;
    let mut last: Option<usize> = None;
    for qch in q.bytes() {
        while ci < cb.len() && cb[ci] != qch {
            ci += 1;
        }
        if ci == cb.len() {
            return None; // query is not a subsequence of the candidate
        }
        if ci == 0 {
            score += 10; // matched at the very start
        }
        if last == Some(ci.wrapping_sub(1)) {
            score += 5; // contiguous with the previous matched char
        }
        last = Some(ci);
        ci += 1;
    }
    score -= (c.len() as i32) - (q.len() as i32); // prefer shorter candidates
    if c.starts_with(&q) {
        score += 15; // whole query is a prefix
    }
    Some(score)
}

/// Highest-scoring fuzzy match for `query` among `users` (shorter wins ties).
fn best_user_match(query: &str, users: &[String]) -> Option<String> {
    users
        .iter()
        .filter_map(|u| fuzzy_score(query, u).map(|s| (s, u)))
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.len().cmp(&a.1.len())))
        .map(|(_, u)| u.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pbs::JobState;

    fn job(id: &str, state: JobState) -> Job {
        Job { id: id.into(), owner: "u".into(), queue: "q".into(), name: "n".into(), state, node: None, cpus: None, mem: None, req_walltime: None, elapsed: None }
    }

    #[test]
    fn tracks_running_jobs_and_wraps() {
        let mut app = App::new();
        app.apply(Update::Jobs(vec![
            job("1.gadi-pbs", JobState::Running),
            job("2.gadi-pbs", JobState::Queued),
            job("3.gadi-pbs", JobState::Running),
        ]));
        assert_eq!(app.running_count(), 2);
        assert_eq!(app.selected_job().unwrap().id, "1.gadi-pbs");
        app.next_job();
        assert_eq!(app.selected_job().unwrap().id, "3.gadi-pbs");
        app.next_job(); // wrap
        assert_eq!(app.selected_job().unwrap().id, "1.gadi-pbs");
        app.prev_job(); // wrap back
        assert_eq!(app.selected_job().unwrap().id, "3.gadi-pbs");
    }

    #[test]
    fn exiting_jobs_stay_selectable() {
        // An E-state job still occupies its node (and its spool is often still
        // readable); it must stay in the running rotation rather than vanish.
        let mut app = App::new();
        app.apply(Update::Jobs(vec![job("1.gadi-pbs", JobState::Exiting)]));
        assert_eq!(app.running_count(), 1);
        assert_eq!(app.selected_job().unwrap().id, "1.gadi-pbs");
    }

    #[test]
    fn keeps_selection_across_refresh_when_job_persists() {
        let mut app = App::new();
        app.apply(Update::Jobs(vec![job("1.gadi-pbs", JobState::Running), job("3.gadi-pbs", JobState::Running)]));
        app.next_job();
        assert_eq!(app.selected_job().unwrap().id, "3.gadi-pbs");
        // refresh: job 1 finished, 3 remains, 5 appears
        app.apply(Update::Jobs(vec![job("3.gadi-pbs", JobState::Running), job("5.gadi-pbs", JobState::Running)]));
        assert_eq!(app.selected_job().unwrap().id, "3.gadi-pbs");
    }

    #[test]
    fn switching_job_keeps_tab_and_refollows_log() {
        let mut app = App::new();
        app.apply(Update::Jobs(vec![
            job("1.gadi-pbs", JobState::Running),
            job("2.gadi-pbs", JobState::Running),
        ]));
        // Switching jobs must NOT change the active tab.
        app.top_tab = TopTab::Details;
        app.next_job();
        assert_eq!(app.top_tab, TopTab::Details);
        app.top_tab = TopTab::Usage;
        // ...but it does re-pin the log to the new job's tail.
        app.log_follow = false;
        app.log_scroll = 5;
        app.prev_job();
        assert_eq!(app.top_tab, TopTab::Usage);
        assert!(app.log_follow);
        assert_eq!(app.log_scroll, 0);
    }

    #[test]
    fn log_replace_then_append() {
        let mut app = App::new();
        app.apply(Update::Log { lines: vec!["a".into(), "b".into()], replace: true });
        app.apply(Update::Log { lines: vec!["c".into()], replace: false });
        assert_eq!(app.log, vec!["a", "b", "c"]);
    }

    #[test]
    fn tabs_cycle_four_ways() {
        let mut app = App::new();
        assert_eq!(app.top_tab, TopTab::Usage);
        app.next_tab();
        assert_eq!(app.top_tab, TopTab::LogPreview);
        app.next_tab();
        assert_eq!(app.top_tab, TopTab::Details);
        app.next_tab();
        assert_eq!(app.top_tab, TopTab::Processes);
        app.next_tab();
        assert_eq!(app.top_tab, TopTab::Usage);
        app.prev_tab();
        assert_eq!(app.top_tab, TopTab::Processes);
        app.prev_tab();
        assert_eq!(app.top_tab, TopTab::Details);
    }

    #[test]
    fn recent_account_expiry_updates() {
        let mut app = App::new();
        assert!(app.show_recent);
        app.apply(Update::Recent(Some("montest  ok".into())));
        assert_eq!(app.recent.as_deref(), Some("montest  ok"));
        app.apply(Update::Recent(None));
        assert_eq!(app.recent, None);
        app.apply(Update::Account("SU (2026.q3): …".into()));
        assert!(app.account.starts_with("SU"));
        app.apply(Update::Expiry(Some("⚠ 3 paths".into())));
        assert!(app.expiry.is_some());
        app.toggle_recent();
        assert!(!app.show_recent);
    }

    #[test]
    fn procs_update_guarded_by_selected_fetch() {
        let mut app = App::new();
        app.procs_job = Some("2.gadi-pbs".into());
        // A stale response for job 1 must not overwrite job 2's pane…
        app.apply(Update::Procs { job: "1.gadi-pbs".into(), text: "stale".into() });
        assert_eq!(app.procs, "");
        // …but the matching job fills it, control chars sanitized.
        app.apply(Update::Procs { job: "2.gadi-pbs".into(), text: "PID\tCMD\nx".into() });
        assert!(app.procs.contains("PID"));
        assert!(!app.procs.contains('\t'));
    }

    #[test]
    fn scroll_acts_on_active_tab() {
        let mut app = App::new();
        // Usage tab active: scroll affects usage_scroll and clears its follow.
        app.scroll_down();
        assert_eq!(app.usage_scroll, 1);
        assert_eq!(app.log_scroll, 0);
        assert!(!app.usage_follow);
        app.scroll_up();
        app.scroll_up(); // saturates at 0
        assert_eq!(app.usage_scroll, 0);
        // Switch to Log tab: scroll now affects log_scroll and clears log_follow.
        app.next_tab();
        app.scroll_down();
        assert_eq!(app.log_scroll, 1);
        assert!(!app.log_follow);
        assert_eq!(app.usage_scroll, 0);
        // End re-engages follow on the active tab.
        app.scroll_to_bottom();
        assert!(app.log_follow);
    }

    #[test]
    fn details_update_and_scroll() {
        let mut app = App::new();
        app.apply(Update::Details {
            job: "174170283.gadi-pbs".into(),
            text: "a\nb\nc".into(),
            output_path: None,
        });
        assert_eq!(app.details_job.as_deref(), Some("174170283.gadi-pbs"));
        assert!(app.details.contains('b'));
        app.next_tab(); // Usage -> LogPreview
        app.next_tab(); // LogPreview -> Details
        assert_eq!(app.top_tab, TopTab::Details);
        app.scroll_down();
        assert_eq!(app.details_scroll, 1);
        app.scroll_to_top();
        assert_eq!(app.details_scroll, 0);
    }

    #[test]
    fn details_are_sanitized_for_terminal_safety() {
        // `qstat -f` wraps long attribute values onto continuation lines that
        // begin with a literal TAB. A raw \t (or any stray ESC/CR) reaches the
        // renderer width-0 to ratatui yet moves the real cursor, desyncing the
        // cell buffer from the terminal so characters stay "frozen" when the
        // Details tab is switched away. Details must be sanitized at ingestion,
        // exactly as log lines are (see logs::sanitize_log_line).
        let mut app = App::new();
        app.apply(Update::Details {
            job: "174170283.gadi-pbs".into(),
            text: "Output_Path = host:/scratch/xr78/bh59\n\t41/run.o\n\u{1b}[31mx\u{1b}[0m\r".into(),
            output_path: None,
        });
        assert!(!app.details.contains('\t'), "tabs must be expanded");
        assert!(!app.details.contains('\u{1b}'), "escape sequences must be dropped");
        assert!(!app.details.contains('\r'), "carriage returns must be dropped");
        // Line structure is preserved; the tab-led continuation is indented.
        assert_eq!(app.details, "Output_Path = host:/scratch/xr78/bh59\n        41/run.o\nx");
    }

    #[test]
    fn page_scroll_uses_viewport_height() {
        let mut app = App::new();
        app.top_inner_h = 10; // as if a render set the page height
        app.page_down();
        assert_eq!(app.usage_scroll, 10);
        assert!(!app.usage_follow);
        app.page_up();
        assert_eq!(app.usage_scroll, 0);
    }

    #[test]
    fn fuzzy_user_match_and_input_flow() {
        let users: Vec<String> = ["cq5024", "pg0953", "abc123", "bh5941"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Prefix and subsequence matches resolve to the intended user.
        assert_eq!(best_user_match("cq", &users).as_deref(), Some("cq5024"));
        assert_eq!(best_user_match("bh59", &users).as_deref(), Some("bh5941"));
        assert_eq!(best_user_match("a13", &users).as_deref(), Some("abc123")); // subsequence
        // No subsequence match at all -> None (caller falls back to raw text).
        assert_eq!(best_user_match("zzz", &users), None);

        // End-to-end: open clean box, type a fragment, confirm adopts the match.
        let mut app = App::new();
        app.user = "bh5941".into();
        app.apply(Update::Users(users.clone()));
        app.begin_user_input();
        assert_eq!(app.input.as_deref(), Some("")); // starts empty
        for c in "cq".chars() {
            app.input_push(c);
        }
        assert_eq!(app.user_suggestion().as_deref(), Some("cq5024"));
        assert_eq!(app.confirm_input().as_deref(), Some("cq5024"));
        assert_eq!(app.user, "cq5024");
        assert!(app.input.is_none());

        // A full name outside your projects still switches (raw fallback).
        app.begin_user_input();
        for c in "zz9999".chars() {
            app.input_push(c);
        }
        assert_eq!(app.confirm_input().as_deref(), Some("zz9999"));
        assert_eq!(app.user, "zz9999");
    }

    #[test]
    fn usage_end_jumps_without_bottom_follow() {
        // On the Usage tab End must jump down without engaging follow — follow
        // means "pin to top" there (ui::render_top), so engaging it would
        // bounce the view straight back up.
        let mut app = App::new();
        assert_eq!(app.top_tab, TopTab::Usage);
        app.scroll_to_bottom();
        assert!(!app.usage_follow);
        assert_eq!(app.usage_scroll, usize::MAX); // clamped at render time
        // Streaming tabs keep the pin-to-bottom semantics.
        app.next_tab();
        app.scroll_to_bottom();
        assert!(app.log_follow);
    }

    #[test]
    fn section_toggles_flip_flags() {
        let mut app = App::new();
        assert!(app.show_array && app.show_queued && app.show_running);
        app.toggle_array();
        app.toggle_queued();
        app.toggle_running();
        assert!(!app.show_array && !app.show_queued && !app.show_running);
    }
}
