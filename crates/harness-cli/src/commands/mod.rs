pub mod approval;
pub mod auth;
pub mod chat;
pub mod clarify;
pub mod clipboard_image;
pub mod config;
pub mod context_window;
pub mod diff;
pub mod exec;
pub mod gateway;
pub mod heartbeat;
pub mod hooks;
pub mod mcp;
pub mod models;
pub mod prompt_images;
pub mod sessions;
pub mod skills;
pub mod supervisor;
pub mod usage;
pub mod voice;

/// Clear the visible terminal and its scrollback, leaving the cursor at home.
/// A no-op when stdout is not an attended terminal (piped or redirected output must
/// stay clean text). A failed write is inconsequential — clearing is cosmetic and
/// must never abort a session over it — so the errors are ignored deliberately.
pub fn clear_screen() {
    use std::io::Write;

    let term = dialoguer::console::Term::stdout();
    if !term.features().is_attended() {
        return;
    }
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[H\x1b[2J\x1b[3J");
    let _ = out.flush();
}
