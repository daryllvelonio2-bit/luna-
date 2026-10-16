use harness_core::HarnessError;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// A snapshot of the 2D terminal screen as rendered by the virtual terminal emulator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenCapture {
    pub rows: usize,
    pub cols: usize,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub lines: Vec<String>,
    pub rendered_display: String,
}

/// A timed keystroke event for interacting with an interactive CLI in a PTY.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimedInput {
    pub delay_ms: u64,
    pub input: String,
}

/// A minimal VT100 / ANSI virtual terminal emulator that maintains a 2D character grid
/// and cursor state, interpreting cursor motion and line/screen erases.
pub struct VirtualTerminal {
    pub rows: usize,
    pub cols: usize,
    pub cursor_row: usize,
    pub cursor_col: usize,
    grid: Vec<Vec<char>>,
}

impl VirtualTerminal {
    pub fn new(rows: usize, cols: usize) -> Self {
        let grid = vec![vec![' '; cols]; rows];
        Self {
            rows,
            cols,
            cursor_row: 0,
            cursor_col: 0,
            grid,
        }
    }

    pub fn put_char(&mut self, ch: char) {
        if self.cursor_row >= self.rows {
            self.cursor_row = self.rows - 1;
        }
        if self.cursor_col >= self.cols {
            self.cursor_col = self.cols - 1;
        }
        self.grid[self.cursor_row][self.cursor_col] = ch;
        self.cursor_col += 1;
    }

    pub fn feed(&mut self, s: &str) {
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        let n = chars.len();

        while i < n {
            let c = chars[i];
            if c == '\x1b' {
                // Escape sequence
                if i + 1 < n && chars[i + 1] == '[' {
                    // CSI sequence
                    let mut j = i + 2;
                    while j < n && !(chars[j] >= '@' && chars[j] <= '~') {
                        j += 1;
                    }
                    if j >= n {
                        break;
                    }
                    let params: String = chars[i + 2..j].iter().collect();
                    let final_ch = chars[j];
                    self.handle_csi(&params, final_ch);
                    i = j + 1;
                    continue;
                }
                i += 2;
                continue;
            }

            match c {
                '\r' => self.cursor_col = 0,
                '\n' => {
                    if self.cursor_row + 1 < self.rows {
                        self.cursor_row += 1;
                    } else {
                        // Scroll down: shift lines up by 1
                        self.grid.remove(0);
                        self.grid.push(vec![' '; self.cols]);
                    }
                }
                '\x08' => {
                    self.cursor_col = self.cursor_col.saturating_sub(1);
                }
                '\t' => {
                    let next_tab = (self.cursor_col / 8 + 1) * 8;
                    self.cursor_col = next_tab.min(self.cols.saturating_sub(1));
                }
                other if other >= ' ' => {
                    self.put_char(other);
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn handle_csi(&mut self, params: &str, final_ch: char) {
        let nums: Vec<usize> = params
            .split(';')
            .filter_map(|p| p.parse::<usize>().ok())
            .collect();

        let n = nums.first().copied().unwrap_or(1).max(1);

        match final_ch {
            'A' => {
                // Cursor Up
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            'B' => {
                // Cursor Down
                self.cursor_row = (self.cursor_row + n).min(self.rows.saturating_sub(1));
            }
            'C' => {
                // Cursor Forward
                self.cursor_col = (self.cursor_col + n).min(self.cols.saturating_sub(1));
            }
            'D' => {
                // Cursor Back
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'H' | 'f' => {
                // Cursor Position: 1-indexed row, col
                let r = nums.first().copied().unwrap_or(1).saturating_sub(1);
                let c = nums.get(1).copied().unwrap_or(1).saturating_sub(1);
                self.cursor_row = r.min(self.rows.saturating_sub(1));
                self.cursor_col = c.min(self.cols.saturating_sub(1));
            }
            'J' => {
                // Erase in Display
                let mode = nums.first().copied().unwrap_or(0);
                match mode {
                    0 => {
                        // Clear from cursor to end of screen
                        for col in self.cursor_col..self.cols {
                            self.grid[self.cursor_row][col] = ' ';
                        }
                        for row in (self.cursor_row + 1)..self.rows {
                            for col in 0..self.cols {
                                self.grid[row][col] = ' ';
                            }
                        }
                    }
                    1 => {
                        // Clear from start to cursor
                        for row in 0..self.cursor_row {
                            for col in 0..self.cols {
                                self.grid[row][col] = ' ';
                            }
                        }
                        for col in 0..=self.cursor_col.min(self.cols.saturating_sub(1)) {
                            self.grid[self.cursor_row][col] = ' ';
                        }
                    }
                    2 | 3 => {
                        // Clear entire screen
                        for row in 0..self.rows {
                            for col in 0..self.cols {
                                self.grid[row][col] = ' ';
                            }
                        }
                        if mode == 2 {
                            self.cursor_row = 0;
                            self.cursor_col = 0;
                        }
                    }
                    _ => {}
                }
            }
            'K' => {
                // Erase in Line
                let mode = nums.first().copied().unwrap_or(0);
                match mode {
                    0 => {
                        // Clear from cursor to end of line
                        for col in self.cursor_col..self.cols {
                            self.grid[self.cursor_row][col] = ' ';
                        }
                    }
                    1 => {
                        // Clear from start of line to cursor
                        for col in 0..=self.cursor_col.min(self.cols.saturating_sub(1)) {
                            self.grid[self.cursor_row][col] = ' ';
                        }
                    }
                    2 => {
                        // Clear entire line
                        for col in 0..self.cols {
                            self.grid[self.cursor_row][col] = ' ';
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    pub fn lines(&self) -> Vec<String> {
        self.grid
            .iter()
            .map(|row| {
                let s: String = row.iter().collect();
                s.trim_end().to_string()
            })
            .collect()
    }

    pub fn capture(&self) -> ScreenCapture {
        let lines = self.lines();
        let mut display = String::new();
        for (idx, line) in lines.iter().enumerate() {
            if !line.is_empty() || idx == self.cursor_row {
                let marker = if idx == self.cursor_row { "▶" } else { " " };
                display.push_str(&format!("{} [{:02}] {}\n", marker, idx, line));
            }
        }

        ScreenCapture {
            rows: self.rows,
            cols: self.cols,
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            lines,
            rendered_display: display,
        }
    }
}

/// Runs a command in an isolated PTY with window sizing, executes timed inputs,
/// and captures both raw output and a 2D screen capture.
pub fn run_in_pty_with_capture(
    binary_path: &Path,
    args: &[&str],
    workdir: &Path,
    inputs: &[TimedInput],
    rows: usize,
    cols: usize,
    timeout: Duration,
) -> Result<(i32, String, ScreenCapture), HarnessError> {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;

    let res = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };

    if res != 0 {
        return Err(HarnessError::EvolverError(format!(
            "openpty failed: errno {}",
            std::io::Error::last_os_error()
        )));
    }

    // Set terminal window size on slave PTY so TIOCGWINSZ returns rows & cols
    unsafe {
        let win = libc::winsize {
            ws_row: rows as libc::c_ushort,
            ws_col: cols as libc::c_ushort,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        libc::ioctl(slave, libc::TIOCSWINSZ, &win);
    }

    // Set non-blocking on master
    unsafe {
        let flags = libc::fcntl(master, libc::F_GETFL);
        libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }

    let slave_stdin = unsafe { File::from_raw_fd(slave) };
    let slave_stdout = match slave_stdin.try_clone() {
        Ok(f) => f,
        Err(e) => {
            unsafe {
                libc::close(master);
            }
            return Err(HarnessError::Io(format!("Failed to clone slave fd: {}", e)));
        }
    };
    let slave_stderr = match slave_stdin.try_clone() {
        Ok(f) => f,
        Err(e) => {
            unsafe {
                libc::close(master);
            }
            return Err(HarnessError::Io(format!("Failed to clone slave fd: {}", e)));
        }
    };

    let mut cmd = Command::new(binary_path);
    cmd.args(args)
        .current_dir(workdir)
        .env("TERM", "xterm-256color")
        .env("COLUMNS", cols.to_string())
        .env("LINES", rows.to_string())
        .stdin(slave_stdin)
        .stdout(slave_stdout)
        .stderr(slave_stderr);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            unsafe {
                libc::close(master);
            }
            return Err(HarnessError::Io(format!(
                "Failed to spawn child in PTY: {}",
                e
            )));
        }
    };

    let mut master_file = unsafe { File::from_raw_fd(master) };
    let mut raw_output = Vec::new();
    let mut vt = VirtualTerminal::new(rows, cols);
    let mut buf = [0u8; 4096];

    let start = Instant::now();
    let mut input_idx = 0;

    while start.elapsed() < timeout {
        // Send scheduled inputs at their specified timestamps
        while input_idx < inputs.len()
            && start.elapsed().as_millis() >= inputs[input_idx].delay_ms as u128
        {
            let _ = master_file.write_all(inputs[input_idx].input.as_bytes());
            let _ = master_file.flush();
            input_idx += 1;
        }

        match master_file.read(&mut buf) {
            Ok(n) if n > 0 => {
                raw_output.extend_from_slice(&buf[..n]);
                if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                    vt.feed(text);
                }
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(_) => break,
        }

        if let Ok(Some(status)) = child.try_wait() {
            // Drain remaining output
            loop {
                match master_file.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        raw_output.extend_from_slice(&buf[..n]);
                        if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                            vt.feed(text);
                        }
                    }
                    _ => break,
                }
            }
            let code = status.code().unwrap_or(-1);
            let out_str = String::from_utf8_lossy(&raw_output).to_string();
            let capture = vt.capture();
            return Ok((code, out_str, capture));
        }
    }

    // Timed out: kill child and grab final screen
    let _ = child.kill();
    let _ = child.wait();
    let capture = vt.capture();
    Err(HarnessError::EvolverError(format!(
        "PTY execution timed out after {:?}.\nFinal Screen Capture:\n{}",
        timeout, capture.rendered_display
    )))
}

/// Simplified wrapper for backwards compatibility
pub fn run_in_pty(
    binary_path: &Path,
    args: &[&str],
    workdir: &Path,
    input_text: Option<&str>,
    timeout: Duration,
) -> Result<(i32, String), HarnessError> {
    let inputs = if let Some(txt) = input_text {
        vec![TimedInput {
            delay_ms: 400,
            input: txt.to_string(),
        }]
    } else {
        vec![]
    };

    let (code, output, _capture) =
        run_in_pty_with_capture(binary_path, args, workdir, &inputs, 24, 80, timeout)?;
    Ok((code, output))
}
