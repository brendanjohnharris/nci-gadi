use crate::app::Update;
use crate::pbs;
use std::sync::mpsc::{channel, Sender};

/// Background worker that fetches `qstat -f <jobid>` (falling back to `-xf`
/// history, via pbs::fetch_job_detail) once per message -- i.e. once per
/// selection change -- and emits `Update::Details`. Send `Some(jobid)` to
/// fetch; `None` to clear. The thread exits when the returned sender is
/// dropped (the `recv()` call returns `Err` on disconnect).
///
/// Rapid job-switching queues multiple requests; the worker coalesces them by
/// draining any already-queued messages with `try_recv()` before fetching, so
/// only the latest selection triggers a qstat call.
pub fn spawn_details_fetcher(updates: Sender<Update>) -> Sender<Option<String>> {
    let (tx, rx) = channel::<Option<String>>();
    std::thread::spawn(move || {
        loop {
            // Block for the next request; exit when all senders are dropped.
            let mut latest = match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            };
            // Coalesce: if more requests are already queued (rapid job-switching),
            // skip straight to the latest one -- only its details are still wanted.
            while let Ok(next) = rx.try_recv() {
                latest = next;
            }
            match latest {
                Some(jobid) => match pbs::fetch_job_detail(&jobid) {
                    Ok(text) => {
                        // Parse Output_Path here (raw text) for the log-preview fallback.
                        let output_path = pbs::output_path(&text);
                        let _ = updates.send(Update::Details { job: jobid, text, output_path });
                    }
                    Err(e) => {
                        let _ = updates.send(Update::Details {
                            job: jobid.clone(),
                            text: format!(
                                "Unable to fetch details for {jobid}.\n\n{e}\n\nTry pressing Ctrl-R to refresh."
                            ),
                            output_path: None,
                        });
                    }
                },
                None => {
                    let _ = updates.send(Update::Details {
                        job: String::new(),
                        text: String::new(),
                        output_path: None,
                    });
                }
            }
        }
    });
    tx
}
