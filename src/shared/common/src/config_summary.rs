use lha_agent::config::Config;
use lha_protocol::config_types::IdentityKind;

use crate::sandbox_summary::summarize_sandbox_policy;

/// Build a list of key/value pairs summarizing the effective configuration.
pub fn create_config_summary_entries(
    config: &Config,
    model: &str,
    identity_kind: IdentityKind,
) -> Vec<(&'static str, String)> {
    let mut entries = vec![
        ("workdir", config.cwd.display().to_string()),
        ("model", model.to_string()),
        ("provider", config.model_provider_id.clone()),
        ("identity", identity_kind_label(identity_kind).to_string()),
        ("approval", config.approval_policy.value().to_string()),
        (
            "sandbox",
            summarize_sandbox_policy(config.sandbox_policy.get()),
        ),
    ];
    if config.model_provider.uses_responses_api() {
        let reasoning_effort = config
            .model_reasoning_effort
            .map(|effort| effort.to_string());
        entries.push((
            "reasoning effort",
            reasoning_effort.unwrap_or_else(|| "none".to_string()),
        ));
        entries.push((
            "reasoning summaries",
            config.model_reasoning_summary.to_string(),
        ));
    }

    entries
}

fn identity_kind_label(identity_kind: IdentityKind) -> &'static str {
    match identity_kind {
        IdentityKind::Nobody => "nobody",
        IdentityKind::Planner => "planner",
        IdentityKind::Programmer => "programmer",
        IdentityKind::Explorer => "explorer",
        IdentityKind::Reviewer => "reviewer",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn identity_entry_uses_stable_lowercase_label() {
        let identity = identity_kind_label(IdentityKind::Planner);

        assert_eq!(identity, "planner");
    }
}
