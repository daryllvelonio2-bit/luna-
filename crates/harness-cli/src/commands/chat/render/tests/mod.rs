pub(crate) fn plain(s: &str) -> String {
    dialoguer::console::strip_ansi_codes(s).to_string()
}

mod landing_and_tools;
mod transcript_and_history;
