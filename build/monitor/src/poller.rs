use crate::app::Update;
use crate::pbs::{self, Job};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub struct PollIntervals {
    pub jobs: Duration,
    pub usage: Duration,
}

/// Commands the main thread sends the poller.
pub enum PollCmd {
    /// Force an immediate refresh of all PBS sources now.
    Refresh,
    /// Switch the monitored user and refresh immediately.
    SetUser(String),
}

/// One jobs poll: fetch, publish, and return the list so the (slower) usage
/// poll can reuse it instead of re-running `qstat -u`.
fn poll_jobs(user: &str, updates: &Sender<Update>) -> Option<Vec<Job>> {
    match pbs::fetch_user_jobs(user) {
        Ok(jobs) => {
            let has_arrays = pbs::has_array_jobs(&jobs);
            updates.send(Update::Jobs(jobs.clone())).ok();
            if has_arrays {
                match pbs::fetch_array_progress(user) {
                    Ok(text) => {
                        updates.send(Update::Array(Some(text))).ok();
                    }
                    Err(e) => {
                        updates
                            .send(Update::Error { source: "qarray".into(), message: e.to_string() })
                            .ok();
                    }
                }
            } else {
                updates.send(Update::Array(None)).ok();
            }
            Some(jobs)
        }
        Err(e) => {
            updates
                .send(Update::Error { source: "qstat".into(), message: e.to_string() })
                .ok();
            None
        }
    }
}

fn poll_usage(jobs: &[Job], updates: &Sender<Update>) {
    match pbs::fetch_usage(jobs) {
        Ok(text) => {
            updates.send(Update::Usage(text)).ok();
        }
        Err(e) => {
            updates
                .send(Update::Error { source: "qusage".into(), message: e.to_string() })
                .ok();
        }
    }
}

pub fn spawn_poller(
    user: String,
    intervals: PollIntervals,
    updates: Sender<Update>,
) -> Sender<PollCmd> {
    let (cmd_tx, cmd_rx): (Sender<PollCmd>, Receiver<PollCmd>) = channel();

    std::thread::spawn(move || {
        let mut user = user; // mutable so `u`/-u can switch the monitored user live
        // Poll everything once at startup.
        let mut last_jobs: Vec<Job> = poll_jobs(&user, &updates).unwrap_or_default();
        poll_usage(&last_jobs, &updates);
        let mut next_jobs = Instant::now() + intervals.jobs;
        let mut next_usage = Instant::now() + intervals.usage;

        loop {
            let now = Instant::now();
            let soonest = next_jobs.min(next_usage);
            let wait = soonest.saturating_duration_since(now);

            match cmd_rx.recv_timeout(wait) {
                Ok(cmd) => {
                    // Refresh or user-switch: poll everything now, reset both deadlines.
                    if let PollCmd::SetUser(u) = &cmd {
                        user = u.clone();
                        last_jobs.clear(); // stale user's jobs must not feed usage
                    }
                    if let Some(jobs) = poll_jobs(&user, &updates) {
                        last_jobs = jobs;
                    }
                    poll_usage(&last_jobs, &updates);
                    next_jobs = Instant::now() + intervals.jobs;
                    next_usage = Instant::now() + intervals.usage;
                }
                Err(RecvTimeoutError::Timeout) => {
                    let now = Instant::now();
                    if now >= next_jobs {
                        if let Some(jobs) = poll_jobs(&user, &updates) {
                            last_jobs = jobs;
                        }
                        next_jobs = now + intervals.jobs;
                    }
                    if now >= next_usage {
                        poll_usage(&last_jobs, &updates);
                        next_usage = now + intervals.usage;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    cmd_tx
}
