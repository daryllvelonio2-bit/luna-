/// Capture what something writes to stdout while it runs.
///
/// The work runs on its own thread: a fresh thread has no output capture of its own, so it
/// writes to the real stdout, which is the redirected file.
///
/// This only sees writes that go to [`std::io::stdout`] directly. `print!` and `println!` go
/// through the harness's own capture first and never reach the redirected descriptor, so a test
/// that captures a `println!` gets an empty string and asserts nothing — check the returned
/// bytes are non-empty when the assertion matters.
///
#[cfg(unix)]
pub(crate) fn capture_stdout<F: FnOnce() + Send + 'static>(f: F) -> String {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    /// Serialises the captures: every capture swaps the process's stdout, so two running at
    /// once write into each other's file and each test reads the other's bytes.
    static CAPTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    let _guard = CAPTURE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out");
    let file = std::fs::File::create(&path).unwrap();
    let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
    assert!(saved >= 0, "cannot save stdout");
    assert!(unsafe { libc::dup2(file.as_raw_fd(), libc::STDOUT_FILENO) } >= 0);

    std::thread::spawn(move || {
        f();
        let _ = std::io::stdout().flush();
    })
    .join()
    .expect("the captured work panicked");
    let _ = std::io::stdout().flush();

    unsafe { libc::dup2(saved, libc::STDOUT_FILENO) };
    unsafe { libc::close(saved) };

    let mut out = String::new();
    std::fs::File::open(&path)
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    out
}
