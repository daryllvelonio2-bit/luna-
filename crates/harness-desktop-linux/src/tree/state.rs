/// The AT-SPI state names, by their bit number.
///
/// AT-SPI reports a state set as a list of bit numbers. The numbers are the
/// ones `atspi-constants` defines, and they are stable: a toolkit that
/// supports a state reports that number.
const STATE_NAMES: &[&str] = &[
    "invalid",
    "active",
    "armed",
    "busy",
    "checked",
    "collapsed",
    "defunct",
    "editable",
    "enabled",
    "expandable",
    "expanded",
    "focusable",
    "focused",
    "has-tooltip",
    "horizontal",
    "iconified",
    "modal",
    "multi-line",
    "multiselectable",
    "opaque",
    "pressed",
    "resizable",
    "selectable",
    "selected",
    "sensitive",
    "showing",
    "single-line",
    "stale",
    "transient",
    "vertical",
    "visible",
    "manages-descendants",
    "indeterminate",
    "required",
    "truncated",
    "animated",
    "invalid-entry",
    "supports-autocompletion",
    "selectable-text",
    "is-default",
    "visited",
    "checkable",
    "has-popup",
    "read-only",
];

/// The names of the states a node reports.
pub fn state_names(bits: &[u32]) -> Vec<String> {
    bits.iter()
        .filter_map(|bit| {
            STATE_NAMES
                .get(*bit as usize)
                .map(|name| (*name).to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_bit_becomes_its_name() {
        assert_eq!(state_names(&[8]), vec!["enabled".to_string()]);
        assert_eq!(state_names(&[12]), vec!["focused".to_string()]);
        assert_eq!(state_names(&[4]), vec!["checked".to_string()]);
    }

    #[test]
    fn several_bits_keep_their_order() {
        let names = state_names(&[8, 12, 23]);
        assert_eq!(names, vec!["enabled", "focused", "selected"]);
    }

    #[test]
    fn an_unknown_bit_is_skipped_rather_than_guessed() {
        assert!(state_names(&[9_999]).is_empty());
    }

    #[test]
    fn an_empty_state_set_is_empty() {
        assert!(state_names(&[]).is_empty());
    }
}
