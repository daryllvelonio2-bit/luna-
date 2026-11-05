use crate::error::{DesktopError, ErrorCode};

/// The separator between a snapshot id and an element number.
const REF_SEPARATOR: &str = ":e";

/// The qualified form of a ref: `@<snapshot_id>:e<N>`.
///
/// The snapshot id is in the ref on purpose. A bare `@e8` names nothing: it
/// says which element of *some* snapshot, and the screen has changed since.
pub fn qualify_ref_id(snapshot_id: &str, local_ref: &str) -> String {
    format!(
        "@{snapshot_id}:{}",
        local_ref.strip_prefix('@').unwrap_or(local_ref)
    )
}

/// Split a qualified ref into its snapshot id and its local element ref.
pub fn resolve_ref_target(ref_id: &str) -> Result<(String, String), DesktopError> {
    let Some(without_at) = ref_id.strip_prefix('@') else {
        return Err(invalid_ref(ref_id));
    };
    let Some((snapshot_id, element_number)) = without_at.rsplit_once(REF_SEPARATOR) else {
        return Err(invalid_ref(ref_id));
    };
    if snapshot_id.is_empty() || !is_snapshot_id(snapshot_id) {
        return Err(invalid_ref(ref_id));
    }
    let local_ref = format!("@e{element_number}");
    if !is_local_ref(&local_ref) {
        return Err(invalid_ref(ref_id));
    }
    Ok((snapshot_id.to_string(), local_ref))
}

/// Whether a string is a well-formed ref.
pub fn validate_ref_token(ref_id: &str) -> Result<(), DesktopError> {
    resolve_ref_target(ref_id).map(|_| ())
}

/// A snapshot id is a short run of lowercase alphanumerics.
fn is_snapshot_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && value
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
}

fn is_local_ref(ref_id: &str) -> bool {
    ref_id.strip_prefix("@e").is_some_and(|digits| {
        !digits.is_empty()
            && digits.len() <= 10
            && digits.chars().all(|character| character.is_ascii_digit())
            && digits.parse::<u32>().is_ok_and(|number| number > 0)
    })
}

fn invalid_ref(ref_id: &str) -> DesktopError {
    DesktopError::new(
        ErrorCode::InvalidArgs,
        format!("Invalid ref '{ref_id}': expected @<snapshot_id>:e<N>"),
    )
    .with_suggestion(
        "Use the ref a snapshot printed, for example @s8f3k2p9:e1. A bare @e1 is not accepted.",
    )
    .not_delivered()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_qualified_ref_splits_into_its_parts() {
        let (snapshot, local) = resolve_ref_target("@s8f3k2p9:e12").expect("a valid ref");
        assert_eq!(snapshot, "s8f3k2p9");
        assert_eq!(local, "@e12");
    }

    #[test]
    fn a_bare_element_ref_is_rejected() {
        let error = resolve_ref_target("@e12").expect_err("a bare ref is not a target");
        assert_eq!(error.code, ErrorCode::InvalidArgs);
    }

    #[test]
    fn a_ref_without_the_at_sign_is_rejected() {
        assert!(resolve_ref_target("s8f3k2p9:e12").is_err());
    }

    #[test]
    fn a_ref_with_a_zero_element_number_is_rejected() {
        assert!(resolve_ref_target("@s8f3k2p9:e0").is_err());
    }

    #[test]
    fn a_ref_with_an_empty_snapshot_is_rejected() {
        assert!(resolve_ref_target("@:e12").is_err());
    }

    #[test]
    fn a_ref_with_a_long_snapshot_is_rejected() {
        assert!(resolve_ref_target("@abcdefghijklmnopqrst:e12").is_err());
    }

    #[test]
    fn qualification_is_idempotent_over_the_local_part() {
        assert_eq!(qualify_ref_id("s1", "@e3"), "@s1:e3");
        assert_eq!(qualify_ref_id("s1", "e3"), "@s1:e3");
    }
}
