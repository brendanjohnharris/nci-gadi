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

/// Background worker for the Processes tab: fetches `qps` (or `qps_gpu` for
/// GPU jobs) for the requested job and emits `Update::Procs`. Same coalescing
/// shape as the details fetcher — qps can take seconds (it runs `ps` on the
/// job's compute nodes), so only the latest request is served.
pub fn spawn_procs_fetcher(updates: Sender<Update>) -> Sender<(String, bool)> {
    let (tx, rx) = channel::<(String, bool)>();
    std::thread::spawn(move || {
        loop {
            let mut latest = match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            };
            while let Ok(next) = rx.try_recv() {
                latest = next;
            }
            let (jobid, gpu) = latest;
            let text = match pbs::fetch_procs(&jobid, gpu) {
                Ok(out) if out.trim().is_empty() => {
                    format!("qps returned nothing for {jobid} (the job may be between phases).")
                }
                Ok(out) => out,
                Err(e) => format!(
                    "Unable to list processes for {jobid}.\n\n{e}\n\nqps only works on \
                     your own running jobs; try Ctrl-R once the job is running."
                ),
            };
            let _ = updates.send(Update::Procs { job: jobid, text });
        }
    });
    tx
}
