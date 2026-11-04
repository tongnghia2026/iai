//! One shape for work the window hands to a worker thread (QC-9): the
//! thread is named, a panic in it comes back as a failure instead of the
//! result never arriving, and the window asks `poll` once a frame.

use std::sync::mpsc::{Receiver, TryRecvError};

/// What `Job::poll` found.
pub(in crate::app) enum Polled<T> {
    /// Still running.
    Pending,
    Done(T),
    /// The worker panicked or went away without an answer.
    Failed(String),
}

/// A worker thread's one result.
pub(in crate::app) struct Job<T> {
    rx: Receiver<std::thread::Result<T>>,
    label: &'static str,
}

impl<T: Send + 'static> Job<T> {
    /// Run `work` on a thread named `iai-<label>`.
    pub(in crate::app) fn spawn(
        label: &'static str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let run = move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            let _ = tx.send(result);
        };
        if let Err(e) = std::thread::Builder::new()
            .name(format!("iai-{label}"))
            .spawn(run)
        {
            // No thread to be had: the job fails at once (its sender is gone).
            crate::diag::note("job", &format!("{label}: no thread ({e})"));
        }
        Self { rx, label }
    }

    pub(in crate::app) fn poll(&self) -> Polled<T> {
        match self.rx.try_recv() {
            Ok(Ok(value)) => Polled::Done(value),
            Ok(Err(panic)) => {
                let why = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".to_string());
                crate::diag::note("job", &format!("{} failed: {why}", self.label));
                Polled::Failed(why)
            }
            Err(TryRecvError::Empty) => Polled::Pending,
            Err(TryRecvError::Disconnected) => Polled::Failed("stopped".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait<T: Send + 'static>(job: &Job<T>) -> Polled<T> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match job.poll() {
                Polled::Pending if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                other => return other,
            }
        }
    }

    #[test]
    fn a_job_hands_back_its_value_and_a_panic_comes_back_as_a_failure() {
        let job = Job::spawn("test-ok", || 41 + 1);
        assert!(matches!(wait(&job), Polled::Done(42)));
        let job = Job::spawn("test-panic", || -> u8 { panic!("printer gone") });
        assert!(matches!(wait(&job), Polled::Failed(why) if why == "printer gone"));
    }
}
