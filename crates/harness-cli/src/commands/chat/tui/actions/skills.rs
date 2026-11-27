//! The `/skills` list popup: pin an installed skill for this session or globally.
//!
//! The popup only records intent. Pinning is applied by [`apply_skill_pin`], which writes the same
//! two places `/skills pin <name> [global|session]` writes: the config file for a global pin, the
//! session store for a session pin, then refreshes the agent's pinned instructions.

use super::super::state::SkillListEntry;
use super::*;

/// Open the `/skills` popup with one row per installed skill.
pub async fn open_skills_list(env: &Env<'_>, st: &mut TuiState) {
    let skills: &[harness_plugins::Skill] =
        env.skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);
    if skills.is_empty() {
        env.ui.emit_above(&format!(
            "• No skills installed — drop SKILL.md directories into {} and restart",
            env.config.paths.skills_dir.display()
        ));
        return;
    }
    // The store is the truth for session pins; the panel's copy may be from an earlier tab.
    let session_pinned = env
        .store
        .get_pinned_skills(env.session_id)
        .await
        .unwrap_or_default();
    st.session_pinned_skills = session_pinned.clone();
    let entries = skills
        .iter()
        .map(|skill| SkillListEntry {
            name: skill.name.clone(),
            global: env
                .config
                .agent
                .pinned_skills
                .iter()
                .any(|p| harness_plugins::names_match(p, &skill.name)),
            session: session_pinned
                .iter()
                .any(|p| harness_plugins::names_match(p, &skill.name)),
        })
        .collect();
    st.modal = ActiveModal::SkillsList {
        entries,
        selected: 0,
    };
}

/// Apply one row's state: global pins go to the config file, session pins to the store. Both are
/// then merged into the agent, so the change takes effect on the next turn without a restart.
pub async fn apply_skill_pin(env: &mut Env<'_>, st: &mut TuiState, index: usize) {
    // Read the row's target state from the modal before touching `env`, so no borrow outlives it.
    let Some((name, global, session)) = (match &st.modal {
        ActiveModal::SkillsList { entries, .. } => entries
            .get(index)
            .map(|e| (e.name.clone(), e.global, e.session)),
        _ => None,
    }) else {
        return;
    };

    // Global: keep `config.agent.pinned_skills` and the config file in step.
    let has_global = env
        .config
        .agent
        .pinned_skills
        .iter()
        .any(|p| harness_plugins::names_match(p, &name));
    if global != has_global {
        if global {
            env.config.agent.pinned_skills.push(name.clone());
        } else {
            env.config
                .agent
                .pinned_skills
                .retain(|p| !harness_plugins::names_match(p, &name));
        }
        let path = harness_core::config::config_file_path();
        if let Ok(yaml) = serde_yaml::to_string(env.config) {
            let _ = std::fs::write(path, yaml);
        }
    }

    // Session: keep the store and the panel's copy in step.
    let mut session_pinned = env
        .store
        .get_pinned_skills(env.session_id)
        .await
        .unwrap_or_default();
    let has_session = session_pinned
        .iter()
        .any(|p| harness_plugins::names_match(p, &name));
    if session != has_session {
        if session {
            session_pinned.push(name.clone());
        } else {
            session_pinned.retain(|p| !harness_plugins::names_match(p, &name));
        }
        let _ = env
            .store
            .set_pinned_skills(env.session_id, &session_pinned)
            .await;
    }
    st.session_pinned_skills = session_pinned;

    refresh_agent_pins(env, st).await;
}

/// Merge global and session pins into the agent's pinned instruction set.
async fn refresh_agent_pins(env: &mut Env<'_>, st: &TuiState) {
    let mut combined = st.session_pinned_skills.clone();
    for g in &env.config.agent.pinned_skills {
        if !combined.iter().any(|s| harness_plugins::names_match(s, g)) {
            combined.push(g.clone());
        }
    }
    let skills_slice: &[harness_plugins::Skill] =
        env.skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);
    let resolved = harness_plugins::resolve_pinned_instructions(
        &combined,
        skills_slice,
        Some(&env.config.paths.skills_dir),
    );
    // `try_lock`: a running turn holds the agent for its whole duration. The pin is already saved,
    // so it takes effect on the next turn instead of freezing the screen for this one.
    match env.agent.try_lock() {
        Ok(mut ag) => {
            ag.set_pinned_skills(combined);
            ag.set_pinned_skill_instructions(resolved);
        }
        Err(_) => env.ui.set_transient_error(Some(
            "Skill saved — it applies when the current turn finishes".to_string(),
        )),
    }
}
