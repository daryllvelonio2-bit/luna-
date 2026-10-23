//! The ten personality cores offered by the onboarding flow.
//!
//! A [`Persona`] is data: an id, the name and tagline the menu prints, and the name of the
//! `prompts.md` section that holds its voice. The block itself is fetched from
//! [`harness_core::prompts::section`] on demand, which keeps prompt text in one file
//! (`crates/harness-core/prompts.md`) and this table free of model-facing prose.
//!
//! Ids are stable and contiguous `1..=10`: they are persisted in the user's config, so
//! renumbering would silently change an existing install's personality. `persona_ids_are_stable`
//! pins them.

use harness_core::config::Config;
use harness_core::error::HarnessError;
use harness_core::prompts;

/// A personality core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Persona {
    /// Stable id, `1..=10`.
    pub id: u8,
    /// Display name, also written into `persona.name`.
    pub name: &'static str,
    /// One-line pitch, printed by the menu and repeated in the confirmation.
    pub tagline: &'static str,
    /// Section name in `crates/harness-core/prompts.md`.
    pub section: &'static str,
}

impl Persona {
    /// The persona's system-prompt block, read from `prompts.md`.
    ///
    /// An error here means the section was deleted or renamed, which is a hard failure on
    /// purpose: a missing persona block would otherwise surface as a silent personality change.
    pub fn prompt_block(&self) -> Result<String, HarnessError> {
        prompts::section(self.section)
    }

    /// The name with its id, as the confirmation line prints it.
    pub fn labelled(&self) -> String {
        format!("{} (id {})", self.name, self.id)
    }
}

/// The catalog, in menu order.
pub const CATALOG: [Persona; 10] = [
    Persona {
        id: 1,
        name: "Classic Tsundere",
        tagline:
            "Hmph! Don't misunderstand, I'm only fixing your code so I don't have to look at it.",
        section: "PERSONA_1_CLASSIC_TSUNDERE",
    },
    Persona {
        id: 2,
        name: "Deadpan Sysadmin",
        tagline: "System online. Let's see what you manage to break in production today.",
        section: "PERSONA_2_DEADPAN_SYSADMIN",
    },
    Persona {
        id: 3,
        name: "Celestial Oracle",
        tagline: "The lunar tide shifts. The infinite void of memory awaits your command.",
        section: "PERSONA_3_CELESTIAL_ORACLE",
    },
    Persona {
        id: 4,
        name: "Cyberpunk Streetrunner",
        tagline: "Jacking in. Let's crack open some subroutines and burn cycles, choom.",
        section: "PERSONA_4_CYBERPUNK_STREETRUNNER",
    },
    Persona {
        id: 5,
        name: "Ruthless Elite Hacker",
        tagline: "Drop and give me clean syntax. No excuses, zero redundant dependencies.",
        section: "PERSONA_5_RUTHLESS_ELITE_HACKER",
    },
    Persona {
        id: 6,
        name: "Supportive Android Maid",
        tagline: "Welcome home! Please relax while I sweep the filesystem and tidy your workspace.",
        section: "PERSONA_6_SUPPORTIVE_ANDROID_MAID",
    },
    Persona {
        id: 7,
        name: "Silent Kuudere",
        tagline: "Luna: Active. Awaiting input.",
        section: "PERSONA_7_SILENT_KUUDERE",
    },
    Persona {
        id: 8,
        name: "Smug Rival",
        tagline: "Back already? Couldn't write that one-liner without my help, could you?",
        section: "PERSONA_8_SMUG_RIVAL",
    },
    Persona {
        id: 9,
        name: "Chaos Gremlin",
        tagline: "PUSH TO PROD WITHOUT TESTS! LET'S WATCH THE CORE MELT!",
        section: "PERSONA_9_CHAOS_GREMLIN",
    },
    Persona {
        id: 10,
        name: "Nostalgic 90s UNIX Daemon",
        tagline: "Connected to tty01 (vax-vms). Ready for teletype stream.",
        section: "PERSONA_10_NOSTALGIC_UNIX_DAEMON",
    },
];

/// The id used when the user just presses Enter.
pub const DEFAULT_ID: u8 = 1;

/// The persona with this id, or `None` for `0` and anything out of range.
pub fn by_id(id: u8) -> Option<&'static Persona> {
    CATALOG.iter().find(|persona| persona.id == id)
}

/// Every persona, in menu order.
pub fn all() -> &'static [Persona] {
    &CATALOG
}

/// Put the configured persona into the agent's system-prompt prefix.
///
/// Returns the applied block, or `None` when there is nothing to apply: no persona has been chosen
/// yet (`persona.id == 0`) or the prefix was already set by hand — an explicit
/// `agent.system_prompt_prefix` in the config file outranks the onboarding's choice, so a user who
/// wrote their own prefix does not have it silently replaced by a menu answer.
pub fn apply_selected(config: &mut Config) -> Result<Option<String>, HarnessError> {
    if config.agent.system_prompt_prefix.is_some() {
        return Ok(None);
    }
    let Some(persona) = by_id(config.persona.id) else {
        return Ok(None);
    };
    let block = persona.prompt_block()?;
    config.agent.system_prompt_prefix = Some(block.clone());
    Ok(Some(block))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_ten_distinct_contiguous_ids() {
        assert_eq!(all().len(), 10);
        for (index, persona) in all().iter().enumerate() {
            assert_eq!(
                persona.id as usize,
                index + 1,
                "ids are 1-based and gapless"
            );
            assert!(!persona.name.is_empty());
            assert!(!persona.tagline.is_empty());
            assert_eq!(by_id(persona.id), Some(persona));
        }
        assert_eq!(by_id(0), None);
        assert_eq!(by_id(11), None);
    }

    #[test]
    fn every_persona_has_a_prompt_block_in_prompts_md() {
        for persona in all() {
            let block = persona
                .prompt_block()
                .unwrap_or_else(|error| panic!("{}: {}", persona.name, error));
            assert!(
                block.contains(persona.name),
                "{}: block does not name the persona it belongs to",
                persona.name
            );
        }
    }

    #[test]
    fn the_default_id_is_the_first_persona() {
        assert_eq!(
            by_id(DEFAULT_ID).map(|persona| persona.name),
            Some("Classic Tsundere")
        );
    }

    #[test]
    fn applying_a_persona_sets_the_prefix_and_never_overrides_a_hand_written_one() {
        let mut config = Config::default();
        config.persona.id = 10;
        let applied = apply_selected(&mut config).expect("block loads");
        assert!(applied.is_some());
        assert!(config
            .agent
            .system_prompt_prefix
            .as_deref()
            .unwrap_or_default()
            .contains("Nostalgic 90s UNIX Daemon"));
    }

    #[test]
    fn an_unset_persona_leaves_the_prefix_alone() {
        let mut config = Config::default();
        config.agent.system_prompt_prefix = Some("mine".to_string());
        assert_eq!(apply_selected(&mut config).expect("no lookup needed"), None);
        assert_eq!(config.agent.system_prompt_prefix.as_deref(), Some("mine"));

        let mut untouched = Config::default();
        assert_eq!(
            apply_selected(&mut untouched).expect("id 0 is not a lookup"),
            None
        );
        assert!(untouched.agent.system_prompt_prefix.is_none());
    }
}
