pub mod approval;
pub mod auth;
pub mod chat;
pub mod clarify;
pub mod clipboard_image;
pub mod config;
pub mod context_window;
pub mod desktop;
pub mod diff;
pub mod exec;
pub mod gateway;
pub mod heartbeat;
pub mod hooks;
pub mod mcp;
pub mod models;
pub mod profile;
pub mod prompt_images;
pub mod sessions;
pub mod skill;
pub mod skills;
pub mod supervisor;
pub mod update;
pub mod usage;
pub mod voice;

/// Clear the visible terminal and its scrollback, leaving the cursor at home.
/// A no-op when stdout is not an attended terminal (piped or redirected output must
/// stay clean text). A failed write is inconsequential — clearing is cosmetic and
/// must never abort a session over it — so the errors are ignored deliberately.
pub fn clear_screen() {
    use crossterm::cursor::MoveTo;
    use crossterm::execute;
    use crossterm::terminal::{self, ClearType};

    let term = dialoguer::console::Term::stdout();
    if !term.features().is_attended() {
        return;
    }
    // crossterm emits the right sequence per platform (including enabling VT processing on
    // Windows), rather than a hardcoded Unix ANSI string.
    let _ = execute!(
        std::io::stdout(),
        terminal::Clear(ClearType::Purge),
        terminal::Clear(ClearType::All),
        MoveTo(0, 0)
    );
}
