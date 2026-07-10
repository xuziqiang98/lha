use crate::product::agent::config::Config;
use crate::product::protocol::config_types::IdentityKind;
use crate::product::protocol::openai_models::ReasoningEffort;

use crate::product::common::sandbox_summary::summarize_sandbox_policy;

/// Build a list of key/value pairs summarizing the effective configuration.
pub fn create_config_summary_entries(
    config: &Config,
    model: &str,
    identity_kind: IdentityKind,
    reasoning_effort: Option<ReasoningEffort>,
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
        let reasoning_effort = reasoning_effort.map(|effort| effort.to_string());
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
    use crate::product::agent::config::test_config;
    use pretty_assertions::assert_eq;

    #[test]
    fn identity_entry_uses_stable_lowercase_label() {
        let identity = identity_kind_label(IdentityKind::Planner);

        assert_eq!(identity, "planner");
    }

    #[test]
    fn reasoning_effort_entry_uses_effective_value() {
        let mut config = test_config();
        config.model_reasoning_effort = Some(ReasoningEffort::High);

        for (reasoning_effort, expected) in [(Some(ReasoningEffort::Low), "low"), (None, "none")] {
            let entries = create_config_summary_entries(
                &config,
                "test-model",
                IdentityKind::Explorer,
                reasoning_effort,
            );
            let actual = entries
                .into_iter()
                .find(|(key, _)| *key == "reasoning effort")
                .map(|(_, value)| value);

            assert_eq!(actual.as_deref(), Some(expected));
        }
    }
}
