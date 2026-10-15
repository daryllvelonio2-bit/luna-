use harness_core::HarnessError;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Runs a command in an isolated PTY, sends optional input, and captures the output.
pub fn run_in_pty(
    binary_path: &Path,
    args: &[&str],
    workdir: &Path,
    input_text: Option<&str>,
    timeout: Duration,
) -> Result<(i32, String), HarnessError> {
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

    // Close slave in parent so EOF can be detected
    // Note: slave_stdin/stdout/stderr will close slave fd copies when dropped

    let mut master_file = unsafe { File::from_raw_fd(master) };
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];

    let start = Instant::now();
    let mut input_sent = input_text.is_none();

    while start.elapsed() < timeout {
        // Feed input after child has a moment to start
        if !input_sent && start.elapsed() >= Duration::from_millis(500) {
            if let Some(txt) = input_text {
                let _ = master_file.write_all(txt.as_bytes());
                let _ = master_file.flush();
            }
            input_sent = true;
        }

        match master_file.read(&mut buf) {
            Ok(n) if n > 0 => {
                output.extend_from_slice(&buf[..n]);
            }
            Ok(_) => {}
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }

        if let Ok(Some(status)) = child.try_wait() {
            // Drain remaining output
            loop {
                match master_file.read(&mut buf) {
                    Ok(n) if n > 0 => output.extend_from_slice(&buf[..n]),
                    _ => break,
                }
            }
            let code = status.code().unwrap_or(-1);
            let out_str = String::from_utf8_lossy(&output).to_string();
            return Ok((code, out_str));
        }
    }

    // Timed out: kill child
    let _ = child.kill();
    let _ = child.wait();
    let out_str = String::from_utf8_lossy(&output).to_string();
    Err(HarnessError::EvolverError(format!(
        "PTY execution timed out after {:?}. Partial output:\n{}",
        timeout, out_str
    )))
}
