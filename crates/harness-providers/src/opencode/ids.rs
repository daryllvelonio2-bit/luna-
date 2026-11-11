use rand::Rng;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const BASE62_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const ID_CEILING: u64 = 1 << 48;

static CACHED_SESSION_ID: OnceLock<String> = OnceLock::new();
static COUNTER: AtomicUsize = AtomicUsize::new(1);

/// Mint a SessionID in the exact shape OpenCode v2's id generator emits.
///
/// OpenCode Zen's free-tier gate decodes the 12-hex prefix of `x-opencode-session`
/// and 403s anything that is not that encoding. Mirror the generator:
/// `~((ms << 12) | counter)` truncated to 48 bits, then 14 random Base62 characters.
pub fn new_opencode_session_id() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cnt = COUNTER.fetch_add(1, Ordering::Relaxed) as u64;
    let value = (ID_CEILING - 1) - ((now_ms * 0x1000 + cnt) % ID_CEILING);

    let mut rng = rand::thread_rng();
    let mut tail = String::with_capacity(14);
    for _ in 0..14 {
        let idx = rng.gen_range(0..62);
        tail.push(BASE62_CHARS[idx] as char);
    }
    format!("ses_{:012x}{}", value, tail)
}

/// Retrieve or initialize a stable SessionID for OpenCode affinity headers.
pub fn get_or_create_opencode_session_id() -> String {
    CACHED_SESSION_ID
        .get_or_init(new_opencode_session_id)
        .clone()
}

/// Mint a RequestID in the exact shape OpenCode v2's id generator emits.
pub fn new_opencode_request_id() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cnt = COUNTER.fetch_add(1, Ordering::Relaxed) as u64;
    let current = (now_ms * 0x1000 + cnt) % ID_CEILING;

    let mut rng = rand::thread_rng();
    let mut tail = String::with_capacity(14);
    for _ in 0..14 {
        let idx = rng.gen_range(0..62);
        tail.push(BASE62_CHARS[idx] as char);
    }
    format!("msg_{:012x}{}", current, tail)
}
