//! Live checks for the OS-level input and capture path.
//!
//! These need a running Wayland session with `grim` (and `hyprctl` for the
//! window queries). They are ignored by default so a headless build stays
//! green; run them with:
//!
//! ```text
//! cargo test -p harness-desktop-linux --test live_desktop -- --ignored
//! ```

use harness_desktop::adapter::input::InputOps;
use harness_desktop::adapter::observation::ObservationOps;
use harness_desktop::deadline::Deadline;
use harness_desktop::tree::options::TreeOptions;
use harness_desktop_linux::LinuxDesktopAdapter;

#[test]
#[ignore = "needs a live Wayland session"]
fn the_active_window_can_be_read() {
    let adapter = LinuxDesktopAdapter::new();
    let window = adapter
        .active_window(Deadline::none())
        .expect("the window manager answers");
    if let Some(window) = window {
        assert!(!window.id.is_empty());
        assert!(!window.app.is_empty());
    }
}

#[test]
#[ignore = "needs a live Wayland session"]
fn a_screenshot_is_a_measurable_png() {
    let adapter = LinuxDesktopAdapter::new();
    let path = std::env::temp_dir().join("luna-live-desktop-test.png");
    let path_text = path.display().to_string();
    let info = adapter
        .screenshot(None, &path_text, Deadline::none())
        .expect("the screen can be captured");
    assert!(info.width > 0);
    assert!(info.height > 0);
    assert_eq!(info.path, std::fs::canonicalize(&path).expect("the file exists").display().to_string());
    let bytes = std::fs::read(&path).expect("the file is readable");
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    let _ = std::fs::remove_file(path);
}

#[test]
#[ignore = "needs a live Wayland session with Firefox"]
fn a_firefox_snapshot_finishes_without_hanging() {
    // The accessibility bus now bounds each call. A busy window must produce a
    // timeout error, not a turn that never returns.
    let adapter = LinuxDesktopAdapter::new();
    let started = std::time::Instant::now();
    let tree = adapter
        .observe("Firefox", &TreeOptions::default(), Deadline::none())
        .expect("the tree is readable");
    println!(
        "snapshot: {} nodes in {:?}",
        tree.node_count(),
        started.elapsed()
    );
    assert!(tree.node_count() > 0);
}

#[test]
#[ignore = "moves the real pointer and clicks"]
fn a_pointer_click_lands_where_it_was_asked() {
    // The calibration is the part that is easy to get wrong: ydotool's
    // absolute coordinates can be scaled by the device's range. Clicking the
    // Firefox title bar is safe and also proves focus follows the pointer.
    let adapter = LinuxDesktopAdapter::new();
    let before = cursor();

    adapter
        .focus("Firefox", Deadline::none())
        .expect("the window can be focused");

    adapter
        .click_at(400, 560, 0, Deadline::none())
        .expect("the click is delivered");
    let landed = cursor();
    println!("requested (400, 560), landed at {landed:?}, started at {before:?}");
    assert!(
        (landed.0 - 400).abs() <= 2 && (landed.1 - 560).abs() <= 2,
        "the pointer landed at {landed:?}, not (400, 560)"
    );

    let active = adapter
        .active_window(Deadline::none())
        .expect("the window manager answers")
        .expect("a window is active");
    assert!(
        active.app.to_lowercase().contains("firefox"),
        "the click should have focused Firefox, not {}",
        active.app
    );
}

fn cursor() -> (i32, i32) {
    let output = std::process::Command::new("hyprctl")
        .arg("cursorpos")
        .output()
        .expect("hyprctl answers");
    let text = String::from_utf8_lossy(&output.stdout);
    let (x, y) = text.trim().split_once(',').expect("x, y");
    (
        x.trim().parse().expect("an x"),
        y.trim().parse().expect("a y"),
    )
}

#[test]
#[ignore = "drives the focused window"]
fn a_key_chord_and_a_paste_reach_the_focused_window() {
    // The end-to-end check of the fallback path: focus the browser, open the
    // address bar with a key chord, paste a URL through the clipboard and
    // press Enter. This is the sequence a web editor needs.
    let adapter = LinuxDesktopAdapter::new();
    let window = adapter
        .focus("Firefox", Deadline::none())
        .expect("the browser can be focused");
    assert!(window.active, "Firefox did not take focus");

    adapter
        .keys("ctrl+l", Deadline::none())
        .expect("the address bar shortcut is delivered");
    adapter
        .paste(
            "https://flow.google.com/project/7815a713-558e-41d7-b992-a063d04174eb",
            Deadline::none(),
        )
        .expect("the URL is pasted");
    adapter
        .keys("enter", Deadline::none())
        .expect("Enter is delivered");
    std::thread::sleep(std::time::Duration::from_secs(4));

    let active = adapter
        .active_window(Deadline::none())
        .expect("the window manager answers")
        .expect("a window is active");
    assert!(active.active);
}
