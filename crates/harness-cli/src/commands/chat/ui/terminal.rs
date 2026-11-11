use dialoguer::console::Term;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Holds the terminal in raw mode for the whole interactive session, restoring the original
/// settings when dropped.
#[cfg(unix)]
pub struct RawTerminal {
    fd: libc::c_int,
    original: libc::termios,
}

/// Holds the terminal in raw mode for the whole interactive session, restoring the original
/// settings when dropped. The Windows console has no termios; crossterm's console API owns it.
#[cfg(windows)]
pub struct RawTerminal;

#[cfg(unix)]
impl RawTerminal {
    /// Returns `None` when stdin is not a terminal, or when termios is unavailable.
    pub fn acquire() -> Option<Self> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        let fd = libc::STDIN_FILENO;
        let mut current = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(fd, current.as_mut_ptr()) } != 0 {
            return None;
        }
        let original = unsafe { current.assume_init() };
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        // Keep output post-processing so `\n` still returns the carriage, and keep ISIG so
        // Ctrl-C stays a signal — the CLI routes it into the key channel itself.
        raw.c_oflag = original.c_oflag;
        raw.c_lflag |= libc::ISIG;
        if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw) } != 0 {
            return None;
        }
        Some(Self { fd, original })
    }
}

#[cfg(windows)]
impl RawTerminal {
    /// Returns `None` when stdin is not a terminal, or when the console cannot enter raw mode.
    pub fn acquire() -> Option<Self> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        crossterm::terminal::enable_raw_mode().ok().map(|_| Self)
    }
}

#[cfg(unix)]
impl Drop for RawTerminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.original);
        }
    }
}

#[cfg(windows)]
impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

pub fn get_terminal_width() -> usize {
    Term::stdout()
        .size_checked()
        .map(|(_, w)| w as usize)
        .filter(|&w| w > 0)
        .unwrap_or(80)
}

pub fn get_terminal_height() -> usize {
    Term::stdout()
        .size_checked()
        .map(|(h, _)| h as usize)
        .filter(|&h| h > 0)
        .unwrap_or(24)
}

/// Counts one paint as running until it is dropped.
pub(super) struct PaintInFlight(pub(super) Arc<AtomicUsize>);

impl Drop for PaintInFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
