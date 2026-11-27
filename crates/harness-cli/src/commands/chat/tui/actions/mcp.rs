//! The `/mcp` list popup: turn MCP servers on for this session or globally.
//!
//! Nothing is spawned here. The popup only reads the config and records intent; a server that is
//! switched on gets registered lazily by the modal's space handler.

use super::super::state::McpListEntry;
use super::*;

/// Open the interactive MCP list. `global` comes from the config; `session` starts off.
pub fn open_mcp_list(env: &Env<'_>, st: &mut TuiState) {
    let entries: Vec<McpListEntry> = env
        .config
        .mcp
        .servers
        .iter()
        .map(|server| McpListEntry {
            name: server.name.clone(),
            global: server.enabled,
            session: false,
        })
        .collect();
    st.modal = ActiveModal::McpList {
        entries,
        selected: 0,
    };
}
