//! The `/skills` readout: an index of what is installed, not a copy of the procedures.
//!
//! A procedure is read with `skill_view`; `/skills` is what a user scans to remember what exists, so
//! each entry is a name and nothing else. A skill's description is written for the model — scope,
//! exclusions, alternatives, frequently a paragraph — and printing it turns a scannable index into a
//! wall of prose that has to be read to find a name. The descriptions still reach the model through
//! `agent.skills_summary`; this module only shapes what the terminal prints.

use harness_plugins::Skill;

/// One `- name` line per skill, in load order.
///
/// `None` when nothing is installed, so a caller can tell "no skills" from "an empty list".
pub fn readout(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let lines: Vec<String> = skills
        .iter()
        .map(|skill| format!("- {}", skill.name))
        .collect();
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            instructions: String::new(),
            file_path: PathBuf::from(format!("{name}/SKILL.md")),
            command: None,
            timeout_secs: None,
            parameters: None,
        }
    }

    #[test]
    fn the_readout_is_one_line_of_names_and_nothing_else() {
        // The first description is the shape skills actually ship in: a paragraph written for a
        // model picking a tool. None of it belongs in a list a human scans for a name.
        let skills = vec![
            skill(
                "blender-design",
                "Art direction for building GOOD Blender projects: composition, camera and lens \
                 choice, lighting rigs, PBR material discipline, colour management, scene hygiene, \
                 and export deliverables. Use when designing, blocking out, lighting, or critiquing \
                 a 3D scene. Not for bpy API mechanics: use `blender` for that.",
            ),
            skill("browser", "Browse websites and search the web."),
        ];

        let readout = readout(&skills).expect("two skills");
        assert_eq!(readout, "- blender-design\n- browser");
        assert_eq!(readout.lines().count(), 2, "one line per skill: {readout}");
    }

    #[test]
    fn no_skills_is_not_an_empty_list() {
        assert_eq!(readout(&[]), None);
        assert_eq!(
            readout(&[skill("tiny", "Does one thing.")]).as_deref(),
            Some("- tiny")
        );
    }
}
