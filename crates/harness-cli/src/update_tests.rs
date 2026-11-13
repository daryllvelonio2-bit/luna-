#[cfg(test)]
mod update_tests {
    use crate::commands::update::cached_build_is_newer_than_running;

    #[test]
    fn installed_build_and_cached_short_prefix_are_not_offered_as_updates() {
        let installed = "678037df1d9d29c3f7b311b953e68a1cb4cf8095";
        let older_run = "c6243d6c1834dda3640e0e00c9c13500b55dc351";
        assert!(!cached_build_is_newer_than_running(installed, installed));
        assert!(cached_build_is_newer_than_running(installed, older_run));
        assert!(!cached_build_is_newer_than_running(installed, "678037df"));
        assert!(cached_build_is_newer_than_running("c2d3ecdd", installed));
        assert!(!cached_build_is_newer_than_running("", installed));
        assert!(!cached_build_is_newer_than_running(installed, ""));
    }
}
