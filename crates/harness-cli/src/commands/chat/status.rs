//! The status line's provider-specific half: who the account is, and what is left of the quota.
use std::sync::Arc;

use super::ui::ChatUi;
use harness_providers::CredentialManager;

pub(crate) async fn refresh_antigravity_status_if_needed(
    ui: &ChatUi,
    current_provider: &str,
    current_model: &str,
    cred_manager: &Arc<CredentialManager>,
) {
    if current_provider.eq_ignore_ascii_case("antigravity")
        || current_provider.eq_ignore_ascii_case("agy")
    {
        let creds = cred_manager.get_credentials("antigravity").await;
        if let Some(primary) = creds
            .iter()
            .find(|c| c.is_priority && !c.is_temporarily_disabled)
            .or_else(|| creds.iter().find(|c| !c.is_temporarily_disabled))
        {
            let client = match harness_providers::build_http_client(30) {
                Ok(c) => c,
                Err(_) => return,
            };

            let strict_email =
                harness_providers::resolve_antigravity_email_strict(&client, &primary.api_key)
                    .await;
            let account_name = strict_email.clone().unwrap_or_else(|| {
                if !primary.label.is_empty() {
                    primary.label.clone()
                } else {
                    "antigravity".to_string()
                }
            });
            ui.set_account_name(Some(account_name));

            if let Ok(summary) = harness_providers::retrieve_antigravity_quota(
                &client,
                &primary.api_key,
                Some(cred_manager),
            )
            .await
            {
                if let Some(display) = summary.format_quota_display(current_model) {
                    ui.set_quota_display(Some(display));
                }
            }
        } else {
            ui.set_account_name(None);
            ui.set_quota_display(None);
        }
    } else {
        ui.set_account_name(None);
        ui.set_quota_display(None);
    }
}
