//! OS-level input and capture for a Wayland desktop.
//!
//! The accessibility bus is the first path, but a rich web editor can ignore
//! `EditableText.SetTextContents` and expose no usable action. This module is
//! the fallback: it focuses a window, moves a real pointer, sends key chords,
//! pastes through the clipboard and captures the screen.
//!
//! The implementations are the ones a Hyprland session already has:
//!
//! * `hyprctl` for window focus and geometry (including the Lua dispatcher
//!   that Hyprland 0.5x uses),
//! * `ydotool` for pointer and keyboard events through `/dev/uinput`,
//! * `wl-copy` (or `xclip`) for the clipboard,
//! * `grim` for Wayland screenshots.
//!
//! Each is optional. A missing one is reported as `PLATFORM_NOT_SUPPORTED`
//! with the package to install, never as a silent success.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use harness_desktop::adapter::input::{ScreenshotInfo, WindowRef};
use harness_desktop::error::{DesktopError, ErrorCode};
use harness_desktop::tree::rect::Rect;
use serde::Deserialize;

/// A window as `hyprctl clients -j` reports it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HyprClient {
    pub address: String,
    #[serde(default = "yes")]
    pub mapped: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub at: [i32; 2],
    #[serde(default)]
    pub size: [i32; 2],
    #[serde(default)]
    pub class: String,
    #[serde(default, rename = "initialClass")]
    pub initial_class: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "focusHistoryID")]
    pub focus_history: i32,
}

fn yes() -> bool {
    true
}

impl HyprClient {
    fn bounds(&self) -> Rect {
        Rect::new(self.at[0], self.at[1], self.size[0], self.size[1])
    }

    fn to_ref(&self, active: bool) -> WindowRef {
        WindowRef {
            id: self.address.clone(),
            app: if self.class.is_empty() {
                self.initial_class.clone()
            } else {
                self.class.clone()
            },
            title: Some(self.title.clone()).filter(|title| !title.is_empty()),
            bounds: Some(self.bounds()),
            active,
        }
    }
}

/// Run a program and return its standard output.
pub(crate) fn run(program: &str, args: &[&str]) -> Result<String, DesktopError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| {
            DesktopError::new(
                ErrorCode::PlatformNotSupported,
                format!("'{program}' could not be run: {error}"),
            )
            .with_suggestion(format!("Install '{program}' and try again"))
            .not_delivered()
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(
            DesktopError::new(
                ErrorCode::ActionFailed,
                format!("'{program}' failed: {stderr}"),
            )
            .with_platform_detail(stderr)
            .not_delivered(),
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Read one `hyprctl` JSON answer.
fn hyprctl_json<T: serde::de::DeserializeOwned>(args: &[&str]) -> Result<T, DesktopError> {
    let text = run("hyprctl", args)?;
    serde_json::from_str(&text).map_err(|error| {
        DesktopError::new(
            ErrorCode::Internal,
            format!("hyprctl returned JSON this adapter cannot read: {error}"),
        )
        .with_platform_detail(text)
    })
}

/// The windows Hyprland knows about.
fn clients() -> Result<Vec<HyprClient>, DesktopError> {
    hyprctl_json(&["clients", "-j"])
}

/// The window that holds focus, if any.
pub(crate) fn active_window() -> Result<Option<WindowRef>, DesktopError> {
    let text = run("hyprctl", &["activewindow", "-j"])?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        DesktopError::new(
            ErrorCode::Internal,
            format!("hyprctl returned JSON this adapter cannot read: {error}"),
        )
        .with_platform_detail(text)
    })?;
    if value.as_object().is_none_or(|object| object.is_empty()) {
        return Ok(None);
    }
    let client: HyprClient = serde_json::from_value(value).map_err(|error| {
        DesktopError::new(
            ErrorCode::Internal,
            format!("the active window is not a window this adapter understands: {error}"),
        )
    })?;
    Ok(Some(client.to_ref(true)))
}

/// Focus the window of `app`.
///
/// Hyprland 0.56 evaluates dispatchers through Lua, where the classic
/// `hyprctl dispatch focuswindow class:…` is a parse error. The Lua form is
/// tried first, then the classic one for older builds.
pub(crate) fn focus(app: &str) -> Result<WindowRef, DesktopError> {
    let client = pick_window(app)?;

    // Try the Lua dispatcher first, then the classic one for older builds. A
    // dispatcher that parses is not proof that the window took focus, so each
    // attempt is followed by a read of the active window.
    let lua = format!(
        "hl.dispatch(hl.dsp.focus({{ window = \"address:{}\" }}))",
        client.address
    );
    if run("hyprctl", &["eval", &lua]).is_ok() && wait_for_focus(&client.address, 12) {
        return Ok(client.to_ref(true));
    }

    let classic = format!("class:{}", client.class);
    if run("hyprctl", &["dispatch", "focuswindow", &classic]).is_ok()
        && wait_for_focus(&client.address, 12)
    {
        return Ok(client.to_ref(true));
    }

    Ok(client.to_ref(false))
}

/// Poll the active window for a short while, because the compositor applies a
/// focus change asynchronously.
fn wait_for_focus(address: &str, attempts: u32) -> bool {
    for _ in 0..attempts {
        let is_active = active_window()
            .ok()
            .flatten()
            .is_some_and(|window| window.id == address);
        if is_active {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// The window that best matches an application name.
///
/// An exact class match wins, then an exact initial class, then a title match;
/// ties go to the most recently focused window. Matching ignores case, spaces
/// and punctuation so `Google Chrome` finds `google-chrome`.
fn pick_window(app: &str) -> Result<HyprClient, DesktopError> {
    let wanted = normalize(app);
    let candidates: Vec<HyprClient> = clients()?
        .into_iter()
        .filter(|client| client.mapped && !client.hidden)
        .collect();

    let exact_class = candidates
        .iter()
        .filter(|client| normalize(&client.class) == wanted)
        .min_by_key(|client| client.focus_history)
        .cloned();
    let exact_initial = candidates
        .iter()
        .filter(|client| normalize(&client.initial_class) == wanted)
        .min_by_key(|client| client.focus_history)
        .cloned();
    let title = candidates
        .iter()
        .filter(|client| normalize(&client.title).contains(&wanted))
        .min_by_key(|client| client.focus_history)
        .cloned();

    exact_class.or(exact_initial).or(title).ok_or_else(|| {
        let available: Vec<String> = candidates
            .iter()
            .map(|client| client.class.clone())
            .filter(|class| !class.is_empty())
            .collect();
        DesktopError::app_not_found(app).with_details(serde_json::json!({
            "available_windows": available,
            "hint": "window classes reported by hyprctl",
        }))
    })
}

/// A comparison key that ignores case, spaces and punctuation.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Send one or more key chords to the focused window.
pub(crate) fn keys(input: &str) -> Result<(), DesktopError> {
    let chords = parse_keys(input)?;
    let mut events: Vec<String> = Vec::new();
    for chord in &chords {
        for code in chord {
            events.push(format!("{code}:1"));
        }
        for code in chord.iter().rev() {
            events.push(format!("{code}:0"));
        }
    }
    let mut args: Vec<&str> = vec!["key", "-d", "12"];
    args.extend(events.iter().map(String::as_str));
    ydotool(&args).map(|_| ())
}

/// Put `text` on the clipboard and paste it into the focused window.
pub(crate) fn paste(text: &str) -> Result<(), DesktopError> {
    set_clipboard(text)?;
    // The clipboard owner needs a moment to publish the selection before the
    // paste request arrives.
    std::thread::sleep(Duration::from_millis(120));
    keys("ctrl+v")
}

/// Write text to the Wayland clipboard, falling back to X11.
fn set_clipboard(text: &str) -> Result<(), DesktopError> {
    let wayland = write_stdin("wl-copy", &[], text);
    if wayland.is_ok() {
        return Ok(());
    }
    let x11 = write_stdin("xclip", &["-selection", "clipboard"], text);
    x11.map_err(|_| {
        DesktopError::new(
            ErrorCode::PlatformNotSupported,
            "No clipboard tool is available",
        )
        .with_suggestion("Install wl-clipboard (Wayland) or xclip (X11)")
        .not_delivered()
    })
}

fn write_stdin(program: &str, args: &[&str], text: &str) -> Result<(), DesktopError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            DesktopError::new(
                ErrorCode::PlatformNotSupported,
                format!("'{program}' could not be run: {error}"),
            )
            .not_delivered()
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).map_err(|error| {
            DesktopError::new(
                ErrorCode::ActionFailed,
                format!("the clipboard did not accept the text: {error}"),
            )
            .not_delivered()
        })?;
    }
    // wl-copy and xclip fork into a small owner that keeps serving the
    // selection. Waiting for that owner would hang the tool, so give the
    // process a moment to finish and then let it keep running.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(DesktopError::new(
                    ErrorCode::ActionFailed,
                    format!("'{program}' exited with {status}"),
                )
                .not_delivered())
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => return Ok(()),
            Err(error) => {
                return Err(DesktopError::new(
                    ErrorCode::ActionFailed,
                    format!("'{program}' did not finish: {error}"),
                )
                .not_delivered())
            }
        }
    }
}

/// Click a screen coordinate with a real pointer.
///
/// ydotool's absolute coordinates are not always in the compositor's logical
/// space: on a HiDPI setup they can be scaled by the device's absolute range
/// (this machine reports `2 * requested + 1`). The mapping is measured once
/// and cached, and the final position is verified and corrected, so a click
/// lands where the caller asked.
pub(crate) fn click_at(x: i32, y: i32, button: u8) -> Result<(), DesktopError> {
    let code = match button {
        0 => "0x00",
        1 => "0x01",
        2 => "0x02",
        other => {
            return Err(DesktopError::invalid_args(format!(
                "button {other} is not one of 0 (left), 1 (right) or 2 (middle)"
            )))
        }
    };
    move_pointer(x, y)?;
    std::thread::sleep(Duration::from_millis(40));
    ydotool(&["click", code]).map(|_| ())
}

/// Move the pointer to a logical screen coordinate and verify it arrived.
fn move_pointer(x: i32, y: i32) -> Result<(), DesktopError> {
    let (scale_x, scale_y) = pointer_scale();
    let mut request_x = request_for(x, scale_x);
    let mut request_y = request_for(y, scale_y);

    for _ in 0..3 {
        ydotool(&[
            "mousemove",
            "-a",
            "--",
            &request_x.to_string(),
            &request_y.to_string(),
        ])?;
        std::thread::sleep(Duration::from_millis(30));

        let Some((actual_x, actual_y)) = cursor_position() else {
            return Ok(());
        };
        let error_x = x - actual_x;
        let error_y = y - actual_y;
        if error_x.abs() <= 2 && error_y.abs() <= 2 {
            return Ok(());
        }
        request_x += request_for(error_x, scale_x);
        request_y += request_for(error_y, scale_y);
    }
    Ok(())
}

/// The raw request that should land on `target` under `scale`.
fn request_for(target: i32, scale: f64) -> i32 {
    (f64::from(target) / scale).round() as i32
}

/// The cursor's logical position, as the compositor reports it.
fn cursor_position() -> Option<(i32, i32)> {
    let text = run("hyprctl", &["cursorpos"]).ok()?;
    let (x, y) = text.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// The factor between a requested absolute move and where the pointer lands.
///
/// Measured once per process with two known requests. A session that already
/// maps 1:1 measures as `(1, 1)`, so the same code works on either kind of
/// machine. The two requests stay small enough that a scaled move still lands
/// on screen: a measurement that runs off the edge would be clamped by the
/// compositor and read as a smaller scale than it is.
fn pointer_scale() -> (f64, f64) {
    static SCALE: OnceLock<(f64, f64)> = OnceLock::new();
    *SCALE.get_or_init(|| {
        const NEAR: i32 = 100;
        const FAR: i32 = 300;
        let Some(origin) = cursor_position() else {
            return (1.0, 1.0);
        };
        let Some(first) = measure_move(NEAR) else {
            return (1.0, 1.0);
        };
        let Some(second) = measure_move(FAR) else {
            let _ = move_absolute_raw(origin.0, origin.1);
            return (1.0, 1.0);
        };
        let _ = move_absolute_raw(origin.0, origin.1);

        let span = f64::from(FAR - NEAR);
        scale_between(first, second, span)
    })
}

/// The per-axis scale from two measured landings `span` raw units apart.
///
/// A measurement that is not a plausible scale (a clamped move, a compositor
/// that ignores absolute moves) falls back to 1:1 rather than producing a
/// nonsense correction.
fn scale_between(first: (i32, i32), second: (i32, i32), span: f64) -> (f64, f64) {
    let scale_x = f64::from(second.0 - first.0) / span;
    let scale_y = f64::from(second.1 - first.1) / span;
    let sane = |scale: f64| scale.is_finite() && (0.25..=16.0).contains(&scale);
    if sane(scale_x) && sane(scale_y) {
        (scale_x, scale_y)
    } else {
        (1.0, 1.0)
    }
}

/// Move to `request` on both axes and read where the pointer landed.
fn measure_move(request: i32) -> Option<(i32, i32)> {
    move_absolute_raw(request, request)?;
    std::thread::sleep(Duration::from_millis(30));
    cursor_position()
}

/// One raw absolute move, in ydotool's own coordinate space.
fn move_absolute_raw(x: i32, y: i32) -> Option<()> {
    ydotool(&[
        "mousemove",
        "-a",
        "--",
        &x.to_string(),
        &y.to_string(),
    ])
    .ok()
    .map(|_| ())
}

/// The ydotool daemon socket, if the session has one.
fn ydotool_socket() -> PathBuf {
    if let Ok(socket) = std::env::var("YDOTOOL_SOCKET") {
        return PathBuf::from(socket);
    }
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join(".ydotool_socket");
    }
    PathBuf::from("/tmp/.ydotool_socket")
}

/// Run ydotool, starting its daemon once if the socket is missing.
fn ydotool(args: &[&str]) -> Result<String, DesktopError> {
    if ydotool_socket().exists() {
        return run("ydotool", args);
    }
    start_ydotoold()?;
    std::thread::sleep(Duration::from_millis(300));
    run("ydotool", args)
}

/// Start the ydotool daemon in the background.
fn start_ydotoold() -> Result<(), DesktopError> {
    Command::new("ydotoold")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            DesktopError::new(
                ErrorCode::PlatformNotSupported,
                format!("the ydotool daemon could not be started: {error}"),
            )
            .with_suggestion(
                "Install ydotool and make sure /dev/uinput is writable, then run `ydotoold &`",
            )
            .not_delivered()
        })
}

/// Parse `ctrl+shift+v` and `ctrl+a ctrl+v` into keycodes.
fn parse_keys(input: &str) -> Result<Vec<Vec<u16>>, DesktopError> {
    let mut chords = Vec::new();
    for token in input.split_whitespace() {
        let mut codes = Vec::new();
        for part in token.split('+') {
            let name = part.trim();
            if name.is_empty() {
                continue;
            }
            let code = keycode(name).ok_or_else(|| {
                DesktopError::invalid_args(format!("'{part}' is not a key this adapter knows"))
                    .with_suggestion(
                        "Use ctrl, shift, alt, super, enter, tab, escape, space, backspace, delete, \
                         home, end, pageup, pagedown, left, right, up, down, f1-f12, a-z or 0-9",
                    )
            })?;
            codes.push(code);
        }
        if codes.is_empty() {
            return Err(DesktopError::invalid_args("an empty key chord was given"));
        }
        chords.push(codes);
    }
    if chords.is_empty() {
        return Err(DesktopError::invalid_args("no keys were given"));
    }
    Ok(chords)
}

/// The Linux input keycode for a key name.
fn keycode(name: &str) -> Option<u16> {
    let name = name.to_ascii_lowercase();
    let code = match name.as_str() {
        "ctrl" | "control" => 29,
        "shift" => 42,
        "alt" => 56,
        "super" | "meta" | "win" | "windows" | "cmd" => 125,
        "enter" | "return" => 28,
        "tab" => 15,
        "esc" | "escape" => 1,
        "space" => 57,
        "backspace" => 14,
        "delete" | "del" => 111,
        "insert" | "ins" => 110,
        "home" => 102,
        "end" => 107,
        "pageup" | "pgup" => 104,
        "pagedown" | "pgdn" => 109,
        "up" => 103,
        "down" => 108,
        "left" => 105,
        "right" => 106,
        "minus" | "-" => 12,
        "equal" | "=" => 13,
        "comma" | "," => 51,
        "dot" | "period" | "." => 52,
        "slash" | "/" => 53,
        "semicolon" | ";" => 39,
        "apostrophe" | "'" => 40,
        "grave" | "backtick" | "`" => 41,
        "leftbrace" | "[" => 26,
        "rightbrace" | "]" => 27,
        "backslash" | "\\" => 43,
        "f1" => 59,
        "f2" => 60,
        "f3" => 61,
        "f4" => 62,
        "f5" => 63,
        "f6" => 64,
        "f7" => 65,
        "f8" => 66,
        "f9" => 67,
        "f10" => 68,
        "f11" => 87,
        "f12" => 88,
        _ => {
            if name.len() == 1 {
                let character = name.as_bytes()[0];
                return match character {
                    b'a'..=b'z' => Some(letter(character)),
                    b'1'..=b'9' => Some(1 + u16::from(character - b'0')),
                    b'0' => Some(11),
                    _ => None,
                };
            }
            return None;
        }
    };
    Some(code)
}

fn letter(character: u8) -> u16 {
    // The three keyboard rows are contiguous in keycode order, not in
    // alphabetical order, so each row is spelled out.
    match character {
        b'q' => 16,
        b'w' => 17,
        b'e' => 18,
        b'r' => 19,
        b't' => 20,
        b'y' => 21,
        b'u' => 22,
        b'i' => 23,
        b'o' => 24,
        b'p' => 25,
        b'a' => 30,
        b's' => 31,
        b'd' => 32,
        b'f' => 33,
        b'g' => 34,
        b'h' => 35,
        b'j' => 36,
        b'k' => 37,
        b'l' => 38,
        b'z' => 44,
        b'x' => 45,
        b'c' => 46,
        b'v' => 47,
        b'b' => 48,
        b'n' => 49,
        b'm' => 50,
        _ => 0,
    }
}

/// Capture the screen, or one application's window, to a PNG file.
pub(crate) fn screenshot(app: Option<&str>, path: &str) -> Result<ScreenshotInfo, DesktopError> {
    let target = match app {
        Some(app) => {
            let client = pick_window(app)?;
            let bounds = client.bounds();
            if !bounds.has_area() {
                return Err(DesktopError::new(
                    ErrorCode::ActionFailed,
                    "The window has no area to capture",
                )
                .not_delivered());
            }
            let geometry = format!(
                "{},{} {}x{}",
                bounds.x, bounds.y, bounds.width, bounds.height
            );
            run("grim", &["-g", &geometry, path])?;
            client.address
        }
        None => {
            run("grim", &[path])?;
            "screen".to_string()
        }
    };

    let bytes = std::fs::read(path).map_err(|error| {
        DesktopError::new(
            ErrorCode::ActionFailed,
            format!("the screenshot was not written to '{path}': {error}"),
        )
        .not_delivered()
    })?;
    let (width, height) = png_dimensions(&bytes).ok_or_else(|| {
        DesktopError::new(
            ErrorCode::ActionFailed,
            "the captured file is not a PNG this adapter can measure",
        )
    })?;
    let absolute = std::fs::canonicalize(path)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| path.to_string());
    Ok(ScreenshotInfo {
        path: absolute,
        width,
        height,
        target,
    })
}

/// The width and height from a PNG header.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chord_becomes_press_and_release_in_order() {
        let chords = parse_keys("ctrl+v").expect("a chord");
        assert_eq!(chords, vec![vec![29, 47]]);
    }

    #[test]
    fn several_chords_are_separate_sequences() {
        let chords = parse_keys("ctrl+a ctrl+v").expect("chords");
        assert_eq!(chords, vec![vec![29, 30], vec![29, 47]]);
    }

    #[test]
    fn letters_are_contiguous_runs() {
        assert_eq!(keycode("a"), Some(30));
        assert_eq!(keycode("l"), Some(38));
        assert_eq!(keycode("m"), Some(50));
        assert_eq!(keycode("q"), Some(16));
        assert_eq!(keycode("z"), Some(44));
        assert_eq!(keycode("0"), Some(11));
        assert_eq!(keycode("9"), Some(10));
    }

    #[test]
    fn an_unknown_key_is_refused_with_a_suggestion() {
        let error = parse_keys("ctrl+nosuchkey").expect_err("unknown");
        assert_eq!(error.code, ErrorCode::InvalidArgs);
        assert!(error.suggestion.is_some());
    }

    #[test]
    fn an_empty_chord_is_refused() {
        assert!(parse_keys("   ").is_err());
    }

    #[test]
    fn window_matching_ignores_case_and_punctuation() {
        assert_eq!(normalize("Google Chrome"), normalize("google-chrome"));
        assert_eq!(normalize("Firefox"), normalize("firefox"));
        assert_ne!(normalize("Firefox"), normalize("Chromium"));
    }

    #[test]
    fn a_scaled_pointer_request_lands_within_two_pixels() {
        // This machine reports `2 * request + 1`; the inverse must be close
        // enough that the correction loop stops.
        for target in [11, 200, 485, 600, 958] {
            let actual = 2 * request_for(target, 2.0) + 1;
            assert!(
                (actual - target).abs() <= 2,
                "target {target} landed at {actual}"
            );
        }
    }

    #[test]
    fn an_unscaled_pointer_request_is_the_target() {
        assert_eq!(request_for(485, 1.0), 485);
    }

    #[test]
    fn a_two_times_scale_is_measured_from_two_landings() {
        // Raw 100 lands at 201 and raw 300 lands at 601.
        assert_eq!(scale_between((201, 201), (601, 601), 200.0), (2.0, 2.0));
    }

    #[test]
    fn an_unscaled_move_measures_one_to_one() {
        assert_eq!(scale_between((100, 100), (300, 300), 200.0), (1.0, 1.0));
    }

    #[test]
    fn a_clamped_or_ignored_move_falls_back_to_one_to_one() {
        // A move the compositor clamped: the second landing is barely past the
        // first, which is not a scale this adapter should trust.
        assert_eq!(scale_between((100, 100), (101, 101), 200.0), (1.0, 1.0));
        // A compositor that ignores absolute moves lands in the same place.
        assert_eq!(scale_between((400, 400), (400, 400), 200.0), (1.0, 1.0));
    }

    #[test]
    fn a_png_header_is_measured() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes.extend_from_slice(&[0, 0, 0, 13]);
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&1920u32.to_be_bytes());
        bytes.extend_from_slice(&1080u32.to_be_bytes());
        assert_eq!(png_dimensions(&bytes), Some((1920, 1080)));
    }

    #[test]
    fn a_file_that_is_not_a_png_is_not_measured() {
        assert_eq!(png_dimensions(b"not a png"), None);
    }
}
