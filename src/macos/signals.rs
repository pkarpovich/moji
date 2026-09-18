//! The SIGTERM launchd sends, turned into something the run loop can read.
//!
//! A signal handler may not stop a run loop: almost nothing is safe to call from one. This handler
//! therefore only sets a flag, and the daemon polls it from a run loop timer, which is the same
//! shape the upgrade watch will take.

use std::sync::atomic::{AtomicBool, Ordering};

static TERMINATING: AtomicBool = AtomicBool::new(false);

/// Installs the SIGTERM handler that records the signal for the run loop to find.
pub fn watch_for_termination() {
    let handler = handle as *const () as libc::sighandler_t;
    unsafe { libc::signal(libc::SIGTERM, handler) };
}

/// Returns whether SIGTERM has arrived since the process started.
pub fn termination_requested() -> bool {
    TERMINATING.load(Ordering::Relaxed)
}

extern "C" fn handle(_signal: libc::c_int) {
    TERMINATING.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_that_was_not_signalled_is_not_terminating() {
        assert!(!termination_requested());
    }

    #[test]
    fn installing_the_handler_does_not_report_a_signal_by_itself() {
        watch_for_termination();

        assert!(!termination_requested());
    }
}
