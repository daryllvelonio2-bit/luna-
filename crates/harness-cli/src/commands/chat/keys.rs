//! Terminal key decoding for the interactive chat.
//!
//! The console crate's reader cannot tell Ctrl+O from Ctrl+Shift+O: a terminal without
//! the kitty keyboard protocol sends both as the single byte `0x0F`. The chat therefore
//! asks the terminal for that protocol (`CSI > 1 u`), which reports the ambiguous
//! combinations in CSI-u form — `CSI 111;6u` for ctrl+shift+o — and decodes the replies
//! here instead of delegating to `console`.
//!
//! The decoder is split in two: [`decode`] is pure (bytes in, key out) so the mapping can
//! be asserted in tests, and [`read_key`] does the blocking read.

// The byte decoder exists for terminals that answer with CSI-u sequences. Windows uses
// crossterm's console events instead, so the decoder is only exercised by the tests there.
#![cfg_attr(windows, allow(dead_code))]

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::io::RawFd;

/// How long the reader waits for the rest of an escape sequence before deciding the
/// Escape key was pressed on its own. Terminals write a sequence in one go, so this only
/// covers the rare case of the bytes arriving in separate reads.
#[cfg(unix)]
const ESC_PEEK_TIMEOUT: Duration = Duration::from_millis(30);

/// How long a blocking read waits before re-checking [`input_paused`]. Short enough that a
/// pause takes effect before a keystroke is lost, long enough to stay idle at rest.
const READ_SLICE: Duration = Duration::from_millis(50);

/// Non-zero while another part of the process owns stdin (`/profile create` runs `dialoguer`).
///
/// The chat reads keys on its own thread. A prompt that reads stdin directly would race that
/// thread for every keystroke, which shows up as keys that need pressing twice. A command that
/// needs stdin pauses the reader for the duration instead.
static INPUT_PAUSED: AtomicUsize = AtomicUsize::new(0);
/// Readers currently inside a read call, so a pause can wait for the fd to be released.
static ACTIVE_READERS: AtomicUsize = AtomicUsize::new(0);

/// Whether the chat's key reader is currently paused.
pub fn input_paused() -> bool {
    INPUT_PAUSED.load(Ordering::SeqCst) > 0
}

/// A guard that lends stdin to another reader and gives it back on drop.
pub struct InputPauseGuard;

impl Drop for InputPauseGuard {
    fn drop(&mut self) {
        INPUT_PAUSED.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Wait for the key reader to leave stdin, then keep it out until the guard drops.
///
/// An in-flight read returns within [`READ_SLICE`], so this wait is bounded even while the
/// terminal is silent.
pub fn pause_input() -> InputPauseGuard {
    INPUT_PAUSED.fetch_add(1, Ordering::SeqCst);
    while ACTIVE_READERS.load(Ordering::SeqCst) > 0 {
        std::thread::sleep(Duration::from_millis(2));
    }
    InputPauseGuard
}

/// Marks a read as active, unless a pause arrived first.
struct ActiveReader;

impl ActiveReader {
    fn acquire() -> Option<Self> {
        ACTIVE_READERS.fetch_add(1, Ordering::SeqCst);
        if input_paused() {
            ACTIVE_READERS.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self)
    }
}

impl Drop for ActiveReader {
    fn drop(&mut self) {
        ACTIVE_READERS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A key press, as the chat understands it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppKey {
    /// A printable character, already decoded from UTF-8.
    Char(char),
    Enter,
    Backspace,
    Tab,
    /// Shift+Tab, which some terminals send as `CSI Z`.
    BackTab,
    Escape,
    CtrlC,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    /// A function key, F1 through F12.
    F(u8),
    /// Ctrl+Shift+O — the key that folds the todo panel.
    CtrlShiftO,
    /// Ctrl+Shift+B — open the live background-task output viewer.
    CtrlShiftB,
    /// Any other control combination, as the letter it modifies.
    Ctrl(char),
    /// A sequence this decoder does not recognise; it is consumed and ignored.
    Unknown,
    /// The terminal window changed size. Windows reports it as a console event rather than a
    /// POSIX `SIGWINCH`, so it arrives through the key channel and the event loop redraws.
    #[cfg_attr(not(windows), allow(dead_code))]
    Resize,
}

/// Ask the terminal for the kitty keyboard protocol, so ctrl+shift+o arrives as a
/// distinguishable key instead of the same byte as ctrl+o.
///
/// Returns whether the request was written. A terminal that does not know the sequence
/// ignores it, which costs nothing: the affected keys simply keep their legacy encoding.
pub fn enable_kitty_keyboard() -> bool {
    #[cfg(unix)]
    {
        write_control("\x1b[>1u")
    }
    #[cfg(not(unix))]
    {
        // The Windows console reports modifiers itself through crossterm; the kitty sequence
        // would just be printed as text by a console that does not understand it.
        false
    }
}

/// Put the terminal's key reporting back the way it was.
pub fn disable_kitty_keyboard() {
    #[cfg(unix)]
    write_control("\x1b[<u");
}

#[cfg(unix)]
fn write_control(sequence: &str) -> bool {
    use std::io::Write;
    let mut stdout = io::stdout();
    match stdout
        .write_all(sequence.as_bytes())
        .and_then(|_| stdout.flush())
    {
        Ok(()) => true,
        Err(_) => false,
    }
}

/// Read one key press from the terminal.
#[cfg(unix)]
pub fn read_key() -> io::Result<AppKey> {
    read_key_fd(libc::STDIN_FILENO)
}

/// Read one key press from the terminal.
#[cfg(windows)]
pub fn read_key() -> io::Result<AppKey> {
    use crossterm::event::{self, Event, KeyEventKind};
    let _active = match ActiveReader::acquire() {
        Some(active) => active,
        None => {
            std::thread::sleep(Duration::from_millis(10));
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
    };
    loop {
        if !event::poll(READ_SLICE)? {
            if input_paused() {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                return Ok(from_crossterm(key));
            }
            // The console reports a resize as an event, not a signal. Hand it to the event
            // loop, which reflows the transcript the same way SIGWINCH does on Unix.
            Event::Resize(_, _) => return Ok(AppKey::Resize),
            // Mouse and focus events are not key presses; the chat ignores them here.
            _ => continue,
        }
    }
}

/// Map crossterm's Windows console key event onto the chat's key vocabulary.
#[cfg(windows)]
fn from_crossterm(key: crossterm::event::KeyEvent) -> AppKey {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char(c) if ctrl && shift && c.eq_ignore_ascii_case(&'o') => AppKey::CtrlShiftO,
        KeyCode::Char(c) if ctrl && shift && c.eq_ignore_ascii_case(&'b') => AppKey::CtrlShiftB,
        KeyCode::Char('c') | KeyCode::Char('C') if ctrl => AppKey::CtrlC,
        KeyCode::Char(c) if ctrl => AppKey::Ctrl(c.to_ascii_lowercase()),
        KeyCode::Char(c) => AppKey::Char(c),
        KeyCode::Enter => AppKey::Enter,
        KeyCode::Backspace => AppKey::Backspace,
        KeyCode::Tab => AppKey::Tab,
        KeyCode::BackTab => AppKey::BackTab,
        KeyCode::Esc => AppKey::Escape,
        KeyCode::Up => AppKey::Up,
        KeyCode::Down => AppKey::Down,
        KeyCode::Left => AppKey::Left,
        KeyCode::Right => AppKey::Right,
        KeyCode::Home => AppKey::Home,
        KeyCode::End => AppKey::End,
        KeyCode::PageUp => AppKey::PageUp,
        KeyCode::PageDown => AppKey::PageDown,
        KeyCode::Delete => AppKey::Delete,
        KeyCode::Insert => AppKey::Insert,
        KeyCode::F(n) => AppKey::F(n),
        _ => AppKey::Unknown,
    }
}

#[cfg(unix)]
fn read_key_fd(fd: RawFd) -> io::Result<AppKey> {
    let _active = match ActiveReader::acquire() {
        Some(active) => active,
        None => {
            // A pause owns stdin; report it the way an interrupted syscall is reported so the
            // caller can wait without inventing a key.
            std::thread::sleep(Duration::from_millis(10));
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
    };

    let mut buf: Vec<u8> = Vec::with_capacity(16);
    loop {
        let byte = match read_byte(fd, Some(READ_SLICE))? {
            ByteRead::Byte(byte) => byte,
            ByteRead::Eof => return Ok(AppKey::Unknown),
            ByteRead::Timeout => {
                if input_paused() {
                    return Err(io::Error::from(io::ErrorKind::Interrupted));
                }
                continue;
            }
        };
        buf.push(byte);

        if byte == 0x1b {
            // The rest of an escape sequence may still be in flight. If nothing follows
            // within the peek window, the Escape key was pressed on its own.
            match read_byte(fd, Some(ESC_PEEK_TIMEOUT))? {
                ByteRead::Byte(next) => buf.push(next),
                ByteRead::Timeout | ByteRead::Eof => return Ok(AppKey::Escape),
            }
        }

        if let Some((key, consumed)) = decode(&buf) {
            buf.drain(..consumed);
            return Ok(key);
        }
        // More bytes are needed for this key; keep reading.
    }
}

/// Decode one key event from the front of `buf`.
///
/// Returns the key and how many bytes it used, or `None` when the buffer holds only part
/// of a key event and more bytes are needed.
pub fn decode(buf: &[u8]) -> Option<(AppKey, usize)> {
    let first = *buf.first()?;
    match first {
        0x1b => decode_escape(buf),
        b'\r' | b'\n' => Some((AppKey::Enter, 1)),
        b'\t' => Some((AppKey::Tab, 1)),
        0x7f | 0x08 => Some((AppKey::Backspace, 1)),
        0x03 => Some((AppKey::CtrlC, 1)),
        0x01 => Some((AppKey::Home, 1)),
        0x05 => Some((AppKey::End, 1)),
        0x00..=0x1f => Some((AppKey::Ctrl(control_letter(first)), 1)),
        _ => decode_utf8(buf),
    }
}

/// The letter a control byte stands for: ctrl+a is 0x01, ctrl+z is 0x1a.
fn control_letter(byte: u8) -> char {
    match byte {
        0x01..=0x1a => (b'a' + byte - 1) as char,
        _ => '?',
    }
}

fn decode_escape(buf: &[u8]) -> Option<(AppKey, usize)> {
    match buf.get(1) {
        None => None,
        Some(b'[') => decode_csi(buf),
        Some(b'O') => {
            // SS3: the function keys and the arrows in their "application" form.
            let final_byte = *buf.get(2)?;
            let key = match final_byte {
                b'A' => AppKey::Up,
                b'B' => AppKey::Down,
                b'C' => AppKey::Right,
                b'D' => AppKey::Left,
                b'H' => AppKey::Home,
                b'F' => AppKey::End,
                b'P' => AppKey::F(1),
                b'Q' => AppKey::F(2),
                b'R' => AppKey::F(3),
                b'S' => AppKey::F(4),
                _ => AppKey::Unknown,
            };
            Some((key, 3))
        }
        // ESC followed by anything else is a sequence this decoder does not model.
        Some(_) => Some((AppKey::Unknown, 2)),
    }
}

/// Decode a `CSI` sequence: `ESC [ <parameters> <final byte>`.
fn decode_csi(buf: &[u8]) -> Option<(AppKey, usize)> {
    let mut index = 2;
    while index < buf.len() {
        let byte = buf[index];
        if (0x40..=0x7e).contains(&byte) {
            let params = &buf[2..index];
            return Some((csi_key(params, byte), index + 1));
        }
        if !(0x20..=0x3f).contains(&byte) {
            // Not a parameter byte: the sequence is malformed, so drop what we have.
            return Some((AppKey::Unknown, index + 1));
        }
        index += 1;
    }
    // No final byte yet.
    None
}

fn csi_key(params: &[u8], final_byte: u8) -> AppKey {
    let text = String::from_utf8_lossy(params);
    let parts: Vec<&str> = text.split(';').collect();

    match final_byte {
        b'u' => kitty_csi_u(&parts),
        b'~' => {
            // xterm's modifyOtherKeys reports a modified key as `CSI 27 ; modifiers ; key ~`.
            if parts.len() >= 3 && parts[0] == "27" {
                return modified_key(parts[2], parts[1]);
            }
            match parts.first().and_then(|p| p.parse::<u32>().ok()) {
                Some(1) | Some(7) => AppKey::Home,
                Some(2) => AppKey::Insert,
                Some(3) => AppKey::Delete,
                Some(4) | Some(8) => AppKey::End,
                Some(5) => AppKey::PageUp,
                Some(6) => AppKey::PageDown,
                Some(11) => AppKey::F(1),
                Some(12) => AppKey::F(2),
                Some(13) => AppKey::F(3),
                Some(14) => AppKey::F(4),
                Some(15) => AppKey::F(5),
                Some(17) => AppKey::F(6),
                Some(18) => AppKey::F(7),
                Some(19) => AppKey::F(8),
                Some(20) => AppKey::F(9),
                Some(21) => AppKey::F(10),
                Some(23) => AppKey::F(11),
                Some(24) => AppKey::F(12),
                _ => AppKey::Unknown,
            }
        }
        b'A' => AppKey::Up,
        b'B' => AppKey::Down,
        b'C' => AppKey::Right,
        b'D' => AppKey::Left,
        b'H' => AppKey::Home,
        b'F' => AppKey::End,
        b'Z' => AppKey::BackTab,
        b'P' => AppKey::F(1),
        b'Q' => AppKey::F(2),
        b'R' => AppKey::F(3),
        b'S' => AppKey::F(4),
        _ => AppKey::Unknown,
    }
}

/// The kitty protocol's `CSI <key> ; <modifiers> u` form.
///
/// The modifier count is `1 +` the modifier bits: shift is 1, alt is 2, ctrl is 4. So
/// ctrl+shift+o is `111;6u` — the count carries the offset, and the bits are one less.
///
/// Kitty's "disambiguate escape codes" mode — which this app asks for — moves the whole
/// ctrl family out of the legacy control bytes and into this form: ctrl+c arrives as
/// `CSI 99;5u` and no longer raises SIGINT, and shift+tab arrives as `CSI 9;2u`. Keys
/// that keep their legacy bytes (enter, tab, backspace) are matched on their code
/// alone; the ones that carry a modifier are matched on both.
fn kitty_csi_u(parts: &[&str]) -> AppKey {
    let code = parts
        .first()
        .and_then(|p| p.split(':').next())
        .and_then(|p| p.parse::<u32>().ok());
    let modifiers = parts
        .get(1)
        .and_then(|p| p.split(':').next())
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(1);
    let bits = modifiers.saturating_sub(1);
    let shift = bits & 1 != 0;
    let ctrl = bits & 4 != 0;
    match code {
        Some(27) => AppKey::Escape,
        Some(13) => AppKey::Enter,
        // The tab key itself keeps its legacy byte, but the shifted tab does not: it is
        // a different key event, and it is how a terminal says "back tab".
        Some(9) => {
            if shift {
                AppKey::BackTab
            } else {
                AppKey::Tab
            }
        }
        Some(127) => AppKey::Backspace,
        // Ctrl+C. With the kitty protocol on, the terminal stops sending the 0x03 byte
        // — and the SIGINT it used to raise — so this is the only way Ctrl+C arrives.
        Some(99) if ctrl => AppKey::CtrlC,
        // The functional keys live in the Unicode private use area.
        Some(57348) => AppKey::Insert,
        Some(57349) => AppKey::Delete,
        Some(57350) => AppKey::Left,
        Some(57351) => AppKey::Right,
        Some(57352) => AppKey::Up,
        Some(57353) => AppKey::Down,
        Some(57354) => AppKey::PageUp,
        Some(57355) => AppKey::PageDown,
        Some(57356) => AppKey::Home,
        Some(57357) => AppKey::End,
        Some(code @ 57364..=57375) => AppKey::F((code - 57364 + 1) as u8),
        Some(code) => match char::from_u32(code) {
            Some('o') | Some('O') if ctrl && shift => AppKey::CtrlShiftO,
            Some('b') | Some('B') if ctrl && shift => AppKey::CtrlShiftB,
            // Ctrl+letter: the letter, with the control bit. Kitty reports these in
            // CSI-u form, and losing the control bit would type the letter instead.
            Some(ch) if ctrl => {
                let letter = ch.to_ascii_lowercase();
                if letter.is_ascii_alphabetic() {
                    AppKey::Ctrl(letter)
                } else if ch == ' ' {
                    AppKey::Char(' ')
                } else {
                    AppKey::Unknown
                }
            }
            Some('O') => AppKey::Char('O'),
            Some(ch) if shift => AppKey::Char(ch.to_ascii_uppercase()),
            Some(ch) => AppKey::Char(ch),
            None => AppKey::Unknown,
        },
        None => AppKey::Unknown,
    }
}

/// xterm's modifyOtherKeys form: a key code plus a modifier count.
fn modified_key(code: &str, modifiers: &str) -> AppKey {
    let code = code.parse::<u32>().unwrap_or(0);
    let bits = modifiers.parse::<u32>().unwrap_or(1).saturating_sub(1);
    let shift = bits & 1 != 0;
    let ctrl = bits & 4 != 0;
    match code {
        27 => AppKey::Escape,
        13 => AppKey::Enter,
        9 => {
            if shift {
                AppKey::BackTab
            } else {
                AppKey::Tab
            }
        }
        127 => AppKey::Backspace,
        99 if ctrl => AppKey::CtrlC,
        _ => match char::from_u32(code) {
            Some('o') | Some('O') if ctrl && shift => AppKey::CtrlShiftO,
            Some('b') | Some('B') if ctrl && shift => AppKey::CtrlShiftB,
            Some(ch) if ctrl => {
                let letter = ch.to_ascii_lowercase();
                if letter.is_ascii_alphabetic() {
                    AppKey::Ctrl(letter)
                } else {
                    AppKey::Unknown
                }
            }
            Some(ch) if shift => AppKey::Char(ch.to_ascii_uppercase()),
            Some(ch) => AppKey::Char(ch),
            None => AppKey::Unknown,
        },
    }
}

fn decode_utf8(buf: &[u8]) -> Option<(AppKey, usize)> {
    let len = utf8_len(buf[0]);
    if buf.len() < len {
        return None;
    }
    match std::str::from_utf8(&buf[..len]) {
        Ok(text) => text.chars().next().map(|c| (AppKey::Char(c), len)),
        Err(_) => Some((AppKey::Unknown, 1)),
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        // A stray continuation byte: consume it and move on.
        _ => 1,
    }
}

/// One step of a byte read: a byte, no byte yet, or the terminal is gone.
#[cfg(unix)]
enum ByteRead {
    Byte(u8),
    Timeout,
    Eof,
}

/// Read one byte, waiting up to `timeout` for it.
#[cfg(unix)]
fn read_byte(fd: RawFd, timeout: Option<Duration>) -> io::Result<ByteRead> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = match timeout {
        None => -1,
        Some(duration) => duration.as_millis().min(i32::MAX as u128) as i32,
    };
    let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(ByteRead::Timeout);
        }
        return Err(error);
    }
    if ready == 0 {
        return Ok(ByteRead::Timeout);
    }

    let mut byte = [0u8; 1];
    loop {
        let read = unsafe { libc::read(fd, byte.as_mut_ptr() as *mut libc::c_void, 1) };
        if read == 1 {
            return Ok(ByteRead::Byte(byte[0]));
        }
        if read == 0 {
            return Ok(ByteRead::Eof);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(bytes: &[u8]) -> AppKey {
        let (key, consumed) = decode(bytes).expect("a complete key event");
        assert_eq!(consumed, bytes.len(), "the whole buffer is one key event");
        key
    }

    #[test]
    fn plain_characters_decode_to_themselves() {
        assert_eq!(key(b"a"), AppKey::Char('a'));
        assert_eq!(key(b"/"), AppKey::Char('/'));
        assert_eq!(key(b" "), AppKey::Char(' '));
    }

    #[test]
    fn control_keys_decode_by_name() {
        assert_eq!(key(b"\r"), AppKey::Enter);
        assert_eq!(key(b"\n"), AppKey::Enter);
        assert_eq!(key(b"\t"), AppKey::Tab);
        assert_eq!(key(b"\x7f"), AppKey::Backspace);
        assert_eq!(key(b"\x03"), AppKey::CtrlC);
    }

    #[test]
    fn ctrl_letters_keep_their_letter() {
        assert_eq!(key(b"\x0f"), AppKey::Ctrl('o'));
        assert_eq!(key(b"\x02"), AppKey::Ctrl('b'));
        assert_eq!(key(b"\x16"), AppKey::Ctrl('v'));
    }

    #[test]
    fn arrows_and_navigation_keys_decode() {
        assert_eq!(key(b"\x1b[A"), AppKey::Up);
        assert_eq!(key(b"\x1b[B"), AppKey::Down);
        assert_eq!(key(b"\x1b[C"), AppKey::Right);
        assert_eq!(key(b"\x1b[D"), AppKey::Left);
        assert_eq!(key(b"\x1b[H"), AppKey::Home);
        assert_eq!(key(b"\x1b[F"), AppKey::End);
        assert_eq!(key(b"\x1b[Z"), AppKey::BackTab);
        assert_eq!(key(b"\x1b[5~"), AppKey::PageUp);
        assert_eq!(key(b"\x1b[6~"), AppKey::PageDown);
        assert_eq!(key(b"\x1b[3~"), AppKey::Delete);
    }

    /// The whole point of the kitty protocol: ctrl+shift+o must not be the same byte as
    /// ctrl+o, or the todo panel could never have its own key.
    #[test]
    fn kitty_reports_ctrl_shift_o_apart_from_ctrl_o() {
        // CSI 111;6u — 'o' with shift+ctrl (1 + 1 + 4).
        assert_eq!(key(b"\x1b[111;6u"), AppKey::CtrlShiftO);
        // CSI 111;5u — 'o' with ctrl only.
        assert_eq!(key(b"\x1b[111;5u"), AppKey::Ctrl('o'));
        // The shifted key on its own is just the capital letter.
        assert_eq!(key(b"\x1b[111;2u"), AppKey::Char('O'));
    }

    /// Ctrl+Shift+B opens the background output viewer; it must not be swallowed as plain Ctrl+B.
    #[test]
    fn kitty_reports_ctrl_shift_b_apart_from_ctrl_b() {
        assert_eq!(key(b"\x1b[98;6u"), AppKey::CtrlShiftB);
        assert_eq!(key(b"\x1b[27;6;98~"), AppKey::CtrlShiftB);
        assert_eq!(key(b"\x1b[98;5u"), AppKey::Ctrl('b'));
        // A terminal without the protocol still sends the legacy ctrl+b byte.
        assert_eq!(key(b"\x02"), AppKey::Ctrl('b'));
    }

    /// Kitty's "disambiguate escape codes" mode moves the ctrl family out of the legacy
    /// control bytes: ctrl+c stops being the byte 0x03 (and stops raising SIGINT) and
    /// arrives as `CSI 99;5u` instead. Decoding it as the letter 'c' would leave the
    /// user with no way out of the chat.
    #[test]
    fn kitty_reports_ctrl_c_as_a_key_event() {
        assert_eq!(key(b"\x1b[99;5u"), AppKey::CtrlC);
        // Shifted, for good measure: still a ctrl+c.
        assert_eq!(key(b"\x1b[99;6u"), AppKey::CtrlC);
    }

    /// The same key, in xterm's modifyOtherKeys form.
    #[test]
    fn xterm_reports_ctrl_c_as_a_key_event() {
        assert_eq!(key(b"\x1b[27;5;99~"), AppKey::CtrlC);
        assert_eq!(key(b"\x1b[27;6;99~"), AppKey::CtrlC);
    }

    /// Shift+tab is a different key event from tab, and kitty reports it as the tab key
    /// with the shift modifier — `CSI 9;2u`, not the legacy `CSI Z`. Reading it as a
    /// plain tab would silently turn the yolo key into a space.
    #[test]
    fn kitty_reports_shift_tab_as_a_back_tab() {
        assert_eq!(key(b"\x1b[9;2u"), AppKey::BackTab);
        // The legacy form still works on a terminal without the protocol.
        assert_eq!(key(b"\x1b[Z"), AppKey::BackTab);
        // And the modified functional-key form.
        assert_eq!(key(b"\x1b[1;2Z"), AppKey::BackTab);
        // The tab key on its own is still a tab.
        assert_eq!(key(b"\x1b[9;1u"), AppKey::Tab);
        assert_eq!(key(b"\t"), AppKey::Tab);
    }

    /// Every key the chat binds, in the form kitty sends it.
    #[test]
    fn kitty_reports_the_bound_keys() {
        assert_eq!(key(b"\x1b[13;1u"), AppKey::Enter);
        assert_eq!(key(b"\x1b[27;1u"), AppKey::Escape);
        assert_eq!(key(b"\x1b[127;1u"), AppKey::Backspace);
        assert_eq!(key(b"\x1b[1;1A"), AppKey::Up);
        assert_eq!(key(b"\x1b[1;1B"), AppKey::Down);
        assert_eq!(key(b"\x1b[1;1C"), AppKey::Right);
        assert_eq!(key(b"\x1b[1;1D"), AppKey::Left);
        assert_eq!(key(b"\x1b[1;1H"), AppKey::Home);
        assert_eq!(key(b"\x1b[1;1F"), AppKey::End);
        assert_eq!(key(b"\x1b[5;1~"), AppKey::PageUp);
        assert_eq!(key(b"\x1b[6;1~"), AppKey::PageDown);
        assert_eq!(key(b"\x1b[3;1~"), AppKey::Delete);
        assert_eq!(key(b"\x1b[2;1~"), AppKey::Insert);
        assert_eq!(key(b"\x1b[1;1P"), AppKey::F(1));
        assert_eq!(key(b"\x1b[15;1~"), AppKey::F(5));
        // A shifted arrow is still that arrow.
        assert_eq!(key(b"\x1b[1;2A"), AppKey::Up);
    }

    /// Kitty keeps the control bit on every letter, and ctrl+space is a space.
    #[test]
    fn kitty_keeps_the_control_bit_on_letters() {
        assert_eq!(key(b"\x1b[118;5u"), AppKey::Ctrl('v'));
        assert_eq!(key(b"\x1b[98;5u"), AppKey::Ctrl('b'));
        assert_eq!(key(b"\x1b[111;5u"), AppKey::Ctrl('o'));
        assert_eq!(key(b"\x1b[97;1u"), AppKey::Char('a'));
        assert_eq!(key(b"\x1b[32;5u"), AppKey::Char(' '));
    }

    #[test]
    fn xterm_modify_other_keys_is_understood_too() {
        // CSI 27;6;111~ — 'o' with shift+ctrl.
        assert_eq!(key(b"\x1b[27;6;111~"), AppKey::CtrlShiftO);
        // CSI 27;5;111~ — 'o' with ctrl.
        assert_eq!(key(b"\x1b[27;5;111~"), AppKey::Ctrl('o'));
    }

    #[test]
    fn kitty_reports_the_named_keys_as_csi_u() {
        assert_eq!(key(b"\x1b[27u"), AppKey::Escape);
        assert_eq!(key(b"\x1b[13u"), AppKey::Enter);
        assert_eq!(key(b"\x1b[9u"), AppKey::Tab);
        assert_eq!(key(b"\x1b[127u"), AppKey::Backspace);
    }

    #[test]
    fn a_partial_sequence_asks_for_more_bytes() {
        assert!(decode(b"\x1b").is_none());
        assert!(decode(b"\x1b[").is_none());
        assert!(decode(b"\x1b[1;").is_none());
    }

    #[test]
    fn multibyte_characters_decode_as_one_key() {
        assert_eq!(key("é".as_bytes()), AppKey::Char('é'));
        assert_eq!(key("☐".as_bytes()), AppKey::Char('☐'));
    }

    #[test]
    fn an_unknown_sequence_is_consumed_rather_than_typed() {
        let (key, consumed) = decode(b"\x1b[>1;2c").unwrap();
        assert_eq!(key, AppKey::Unknown);
        assert_eq!(consumed, 7);
    }
}
