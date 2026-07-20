use monitor::app::{App, Update};
use monitor::logs::{self, LogTarget};
use monitor::poller::{self, PollCmd, PollIntervals};
use monitor::{details, pbs, ui};

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "monitor", about = "PBS job monitor for NCI Gadi (gentle polling)")]
struct Cli {
    /// User to monitor (default: $USER); switch live with `u`
    #[arg(short = 'u', long)]
    username: Option<String>,
    /// Per-job usage poll interval, seconds
    #[arg(long, default_value_t = 30)]
    usage_interval: u64,
    /// Your-jobs poll interval, seconds
    #[arg(long, default_value_t = 10)]
    jobs_interval: u64,
    /// Spooled-output (qcat) poll interval, seconds
    #[arg(long, default_value_t = 15)]
    qcat_interval: u64,
}

/// Restores the terminal on drop, even on panic.
struct TermGuard;
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

/// Where the Log Preview should read from, in priority order: a tailable file
/// (the `~/.jobs` convention, else the job's `Output_Path` once it exists on
/// disk — e.g. submitted with `#PBS -k oed`, or just finished), else `qcat` for
/// the spooled output of the running job, else nothing.
fn resolve_target(app: &App, jobs_dir: &Path) -> LogTarget {
    let Some(id) = app.selected_job_id.as_deref() else {
        return LogTarget::None;
    };
    if let Some(p) = logs::resolve_log_path(jobs_dir, id) {
        return LogTarget::File(p);
    }
    if app.details_job.as_deref() == Some(id) {
        if let Some(p) = app.output_path.clone().filter(|p| p.is_file()) {
            return LogTarget::File(p);
        }
    }
    // Selection is always over running jobs, so qcat is applicable.
    LogTarget::Qcat { jobid: id.to_string() }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let user = cli
        .username
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_default();

    let jobs_dir = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".jobs")
    };

    // Channels: poller + log watcher both send Updates to the main loop.
    let (update_tx, update_rx) = channel::<Update>();
    let poll_tx = poller::spawn_poller(
        user.clone(),
        PollIntervals {
            jobs: Duration::from_secs(cli.jobs_interval),
            usage: Duration::from_secs(cli.usage_interval),
        },
        update_tx.clone(),
    );
    let log_tx = logs::spawn_log_watcher(update_tx.clone(), Duration::from_secs(cli.qcat_interval));
    let detail_tx = details::spawn_details_fetcher(update_tx.clone());

    // Fuzzy candidates for the `u` switch: project-group members, fetched once
    // in the background (getent can stall on a slow LDAP; never block the UI).
    {
        let tx = update_tx.clone();
        std::thread::spawn(move || {
            let users = pbs::fetch_known_users();
            if !users.is_empty() {
                let _ = tx.send(Update::Users(users));
            }
        });
    }

    // Install panic hook before entering raw mode so a panic at any point restores the terminal.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
    // Terminal setup with a guard so we always restore on exit/panic.
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let _guard = TermGuard;

    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = App::new();
    app.user = user;
    app.project = std::env::var("PROJECT").unwrap_or_default();

    // Log-target tracking: re-resolve on selection change immediately, and every
    // couple of seconds otherwise (details arriving, the ~/.jobs log or the
    // copied-back .o file appearing, the job leaving the running set).
    let mut current_target = LogTarget::None;
    let mut last_sel: Option<String> = None;
    let mut last_eval = Instant::now() - Duration::from_secs(10);

    loop {
        // Drain all pending updates from background threads.
        while let Ok(u) = update_rx.try_recv() {
            app.apply(u);
        }

        let sel_changed = app.selected_job_id != last_sel;
        if sel_changed || last_eval.elapsed() >= Duration::from_secs(2) {
            last_eval = Instant::now();
            if sel_changed {
                last_sel = app.selected_job_id.clone();
                let _ = detail_tx.send(app.selected_job_id.clone());
            }
            let target = resolve_target(&app, &jobs_dir);
            if target != current_target {
                current_target = target.clone();
                let _ = log_tx.send(target);
            }
        }

        terminal.draw(|f| ui::draw(f, &mut app))?;

        // Input with a short timeout so the clock/ages keep ticking.
        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                // Ctrl-C always quits, even while typing in the username box.
                if ctrl && key.code == KeyCode::Char('c') {
                    app.should_quit = true;
                } else if app.input.is_some() {
                    // Username box open: keys edit the buffer instead of driving the UI.
                    match key.code {
                        KeyCode::Enter => {
                            if let Some(new_user) = app.confirm_input() {
                                let _ = poll_tx.send(PollCmd::SetUser(new_user));
                            }
                        }
                        KeyCode::Esc => app.cancel_input(),
                        KeyCode::Backspace => app.input_backspace(),
                        KeyCode::Char(c) => app.input_push(c),
                        _ => {}
                    }
                } else {
                    match key.code {
                        // Quit (q toggles the Queued section, so quit is Esc / Ctrl-C)
                        KeyCode::Esc => app.should_quit = true,
                        // Switch the top tab (Job Usage <-> Log Preview <-> Details)
                        KeyCode::Left => app.prev_tab(),
                        KeyCode::Right => app.next_tab(),
                        // Scroll the active top tab
                        KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
                        KeyCode::Down | KeyCode::Char('j') => app.scroll_down(),
                        KeyCode::Char('g') | KeyCode::Home => app.scroll_to_top(),
                        KeyCode::Char('G') | KeyCode::End => app.scroll_to_bottom(),
                        // Switch the selected running job (drives the Log Preview tab)
                        KeyCode::PageDown => app.page_down(),
                        KeyCode::PageUp => app.page_up(),
                        KeyCode::Char('.') => app.next_job(),
                        KeyCode::Char(',') => app.prev_job(),
                        // Toggle the lower sections (running moved off 'u' to 'r')
                        KeyCode::Char('a') => app.toggle_array(),
                        KeyCode::Char('q') => app.toggle_queued(),
                        KeyCode::Char('r') if !ctrl => app.toggle_running(),
                        // Toggle array-job compaction (e/c are redundant mnemonics)
                        KeyCode::Char('e') | KeyCode::Char('c') => app.toggle_compact(),
                        // Open the username switch box
                        KeyCode::Char('u') => app.begin_user_input(),
                        // Force an immediate PBS refresh (moved off 'r' to Ctrl-R)
                        KeyCode::Char('r') if ctrl => {
                            let _ = poll_tx.send(PollCmd::Refresh);
                            let _ = detail_tx.send(app.selected_job_id.clone());
                        }
                        _ => {}
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
