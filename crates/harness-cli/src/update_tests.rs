#[cfg(test)]
#[allow(clippy::module_inception)]
mod update_tests {
    use serde_json::json;

    use crate::commands::update::{
        cache_entry_is_fresh, cached_build_is_newer_than_running, compare_status_means_newer,
        select_latest_successful_run, CacheEntry,
    };

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

    #[test]
    fn cache_entry_is_invalidated_after_upgrading_to_new_build() {
        let legacy = CacheEntry {
            checked_at: 1_000,
            checked_for: String::new(),
            latest: "c6243d6c1834dda3640e0e00c9c13500b55dc351".to_string(),
            ..Default::default()
        };
        assert!(!cache_entry_is_fresh(
            &legacy,
            1_100,
            21_600,
            "37d73152d6156281d6f3623629748a1033ddcf66"
        ));

        let same_build = CacheEntry {
            checked_at: 1_000,
            checked_for: "37d73152d6156281d6f3623629748a1033ddcf66".to_string(),
            latest: "37d73152d6156281d6f3623629748a1033ddcf66".to_string(),
            ..Default::default()
        };
        assert!(cache_entry_is_fresh(
            &same_build,
            1_100,
            21_600,
            "37d73152d6156281d6f3623629748a1033ddcf66"
        ));
        assert!(!cache_entry_is_fresh(
            &same_build,
            25_000,
            21_600,
            "37d73152d6156281d6f3623629748a1033ddcf66"
        ));
        assert!(!cache_entry_is_fresh(
            &same_build,
            1_100,
            21_600,
            "9999999999999999999999999999999999999999"
        ));
    }

    #[test]
    fn selects_highest_successful_run_without_falling_back_to_older_runs() {
        let payload = json!({
            "workflow_runs": [
                {
                    "id": 66,
                    "run_number": 66,
                    "status": "completed",
                    "conclusion": "failure",
                    "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                {
                    "id": 65,
                    "run_number": 65,
                    "status": "completed",
                    "conclusion": "success",
                    "head_sha": "37d73152d6156281d6f3623629748a1033ddcf66",
                    "head_commit": { "timestamp": "2026-11-13T01:59:46Z" }
                },
                {
                    "id": 61,
                    "run_number": 61,
                    "status": "completed",
                    "conclusion": "success",
                    "head_sha": "c6243d6c1834dda3640e0e00c9c13500b55dc351",
                    "head_commit": { "timestamp": "2026-11-12T19:17:45Z" }
                }
            ]
        });

        let selected = select_latest_successful_run(&payload).expect("run selected");
        assert_eq!(selected.run_id, 65);
        assert_eq!(selected.sha, "37d73152d6156281d6f3623629748a1033ddcf66");
        assert!(selected.commit_ts.is_some());
    }

    #[test]
    fn compare_status_never_offers_behind_or_identical_builds() {
        assert!(compare_status_means_newer("ahead", Some(100), Some(90)));
        assert!(!compare_status_means_newer("behind", Some(100), Some(200)));
        assert!(!compare_status_means_newer("identical", Some(100), Some(200)));
        assert!(compare_status_means_newer("diverged", Some(100), Some(200)));
        assert!(!compare_status_means_newer("diverged", Some(200), Some(100)));
        assert!(!compare_status_means_newer("diverged", None, Some(200)));
    }
}
