//! One glob matcher for every pattern the harness matches against a *string* — tool names, rule
//! arguments, hook matchers.
//!
//! `*` matches any run of characters (including none), `?` matches exactly one, everything else is
//! literal. There is no escaping and no character class: these patterns come from a config file
//! written by a person, and a pattern language with more surface than that is a language with more
//! ways to be subtly wrong.
//!
//! Path matching is a different problem with its own rules (see `tools::fs`); this is deliberately
//! not shared with it.

/// Whether `pattern` matches `text` in full.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut resume) = (None::<usize>, 0usize);

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            resume = t;
            p += 1;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Whether a pattern that may be `None` matches. `None` and empty mean "everything".
pub fn matches_optional(pattern: Option<&str>, text: &str) -> bool {
    match pattern {
        None => true,
        Some(pattern) if pattern.is_empty() || pattern == "*" => true,
        Some(pattern) => glob_match(pattern, text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_wildcards() {
        assert!(glob_match("terminal", "terminal"));
        assert!(!glob_match("terminal", "terminals"));
        assert!(glob_match("write_*", "write_file"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("?ead_file", "read_file"));
        assert!(!glob_match("?ead_file", "read_files"));
        assert!(glob_match("git push*", "git push --force origin main"));
        assert!(glob_match("*force*", "git push --force"));
        assert!(!glob_match("*force*", "git push"));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
    }

    #[test]
    fn optional_patterns_match_everything() {
        assert!(matches_optional(None, "terminal"));
        assert!(matches_optional(Some(""), "terminal"));
        assert!(matches_optional(Some("*"), "terminal"));
        assert!(!matches_optional(Some("read_*"), "terminal"));
    }
}
