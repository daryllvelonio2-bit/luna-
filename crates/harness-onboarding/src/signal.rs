//! Cursor restoration on an early exit.
//!
//! Onboarding hides the cursor and streams for a few seconds. If the user hits Ctrl+C in the middle
//! of that, the shell must not be left with an invisible cursor — hence two independent nets:
//!
//! - [`CursorGuard`] is a drop guard, so every early return *and* every `?` in the flow restores the
//!   cursor.
//! - [`install`] registers SIGINT/SIGTERM handlers, because a signal kills the process without
//!   unwinding and no drop guard would run. The handler writes the restore sequence with a raw
//!   `write(2)` (the only async-signal-safe option), then re-raises with the default disposition so
//!   the exit status is still the signal's, as the shell and any supervisor expect.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::cursor::Show;
use crossterm::style::ResetColor;
use crossterm::QueueableCommand;

use harness_core::error::HarnessError;

/// Sequences written from the signal handler: show cursor, reset attributes, newline.
///
/// Only the Unix signal handler writes it at runtime; the tests assert its exact bytes on every
/// platform, so a non-test Windows build has no reader for it.
#[cfg_attr(not(unix), allow(dead_code))]
const RESTORE: &[u8] = b"\x1b[?25h\x1b[0m\n";

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// The signal handler. Async-signal-safe by construction: one `write`, no allocation, no locking.
#[cfg(unix)]
extern "C" fn restore_and_die(signal: libc::c_int) {
    // SAFETY: `RESTORE` is a `'static` buffer and `write` is async-signal-safe. The signal number is
    // restored to its default disposition and re-raised so the process exits with the right status
    // instead of being swallowed by the handler.
    unsafe {
        libc::write(
            libc::STDOUT_FILENO,
            RESTORE.as_ptr() as *const libc::c_void,
            RESTORE.len(),
        );
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// Install the SIGINT/SIGTERM handlers once per process. Idempotent: a second call is a no-op, so
/// re-entering the flow (a test, a restarted session) cannot stack handlers.
#[cfg(unix)]
pub fn install() -> Result<(), HarnessError> {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    for signal in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: `restore_and_die` is a valid `extern "C"` handler that performs one async-safe
        // write, and the `sigaction` struct is fully initialised before the call.
        let installed = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            let handler: extern "C" fn(libc::c_int) = restore_and_die;
            action.sa_sigaction = handler as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigaction(signal, &action, std::ptr::null_mut())
        };
        if installed != 0 {
            // Un-install the state flag so a later attempt can retry the failing signal.
            INSTALLED.store(false, Ordering::SeqCst);
            return Err(HarnessError::Io(format!(
                "could not install the SIGINT/SIGTERM cursor guard: {}",
                io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

/// On Windows the console has no POSIX signal disposition to restore; the cursor guard's `Drop`
/// still runs on every ordinary exit, and Ctrl+C is delivered as a key by the console.
#[cfg(not(unix))]
pub fn install() -> Result<(), HarnessError> {
    INSTALLED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Hides the cursor on construction and restores it on drop, whatever path leaves the flow.
pub struct CursorGuard;

impl CursorGuard {
    /// Hide the cursor (ignoring a failure: a redirected stdout has no cursor to hide) and return
    /// the guard that will bring it back.
    pub fn new() -> Self {
        let _ = hide();
        Self
    }
}

impl Default for CursorGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CursorGuard {
    fn drop(&mut self) {
        let _ = show();
    }
}

/// Hide the cursor on stdout.
pub fn hide() -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.queue(crossterm::cursor::Hide)?;
    stdout.queue(ResetColor)?;
    stdout.flush()
}

/// Restore the cursor on stdout.
pub fn show() -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.queue(Show)?;
    stdout.queue(ResetColor)?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installing_twice_is_a_no_op() {
        install().expect("first install succeeds");
        install().expect("second install is a no-op");
        assert!(INSTALLED.load(Ordering::SeqCst));
    }

    #[test]
    fn the_restore_sequence_is_exactly_what_a_shell_needs() {
        assert_eq!(RESTORE, b"\x1b[?25h\x1b[0m\n");
        let text = String::from_utf8_lossy(RESTORE);
        assert!(text.starts_with("\u{1b}[?25h"));
    }

    #[test]
    fn the_guard_can_be_created_and_dropped_without_a_terminal() {
        let guard = CursorGuard::new();
        drop(guard);
    }
}
