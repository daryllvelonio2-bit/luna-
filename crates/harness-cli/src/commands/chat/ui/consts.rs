/// Fewest rows the live region gives the thought block. On a screen with no room left the block
/// still shows this much: below it the region stops saying anything at all.
pub const MIN_LIVE_REASONING_ROWS: usize = 4;

/// How often the live region advances a spinner frame and refreshes its elapsed time.
pub const ANIMATION_INTERVAL: std::time::Duration = std::time::Duration::from_millis(80);

/// How often the landing shower advances a frame. Faster than [`ANIMATION_INTERVAL`] because the
/// stars move across the mark: a spinner reads fine at twelve frames a second, a meteor does not.
pub const LANDING_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);

/// What the empty composer says when nothing is in flight: the one line that teaches the way in.
pub(super) const IDLE_HINT: &str = "Luna is inactive — type /help for commands";

/// What the empty composer says while a turn is in flight. The commands that only work mid-turn
/// are named here, because a running turn is exactly when they are worth knowing about. Typing a
/// line now redirects the turn (stopping the current reply but keeping what was already thought),
/// so the hint leads with that rather than with `/interrupt`.
pub(super) const BUSY_HINT: &str =
    "Luna is thinking — type to redirect · /steer to guide · /queue to wait";

/// Columns of room the prompt row must have before a hint is drawn at all. Below this the hint
/// would be a clipped fragment that teaches nothing, so the prompt is drawn bare instead.
pub(super) const HINT_MIN_ROOM: usize = 12;
