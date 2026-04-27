use std::collections::HashMap;
use std::collections::HashSet;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

pub use adam_llm_types::ModelInfoUpgrade;
pub use adam_llm_types::ModelInstructionsVariables;
pub use adam_llm_types::ModelMessages;
pub use adam_llm_types::ModelVisibility;
pub use adam_llm_types::ReasoningEffort;
pub use adam_llm_types::ReasoningEffortPreset;
pub use adam_llm_types::TruncationMode;
pub use adam_llm_types::TruncationPolicyConfig;
pub use adam_llm_types::reasoning_effort_mapping_from_presets;

#[derive(
    Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema, strum_macros::Display,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ConfigShellToolType {
    Default,
    Local,
    UnifiedExec,
    Disabled,
    ShellCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPatchToolType {
    Freeform,
    Function,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema)]
pub struct ModelInfo {
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_level: Option<ReasoningEffort>,
    pub supported_reasoning_levels: Vec<ReasoningEffortPreset>,
    pub shell_type: ConfigShellToolType,
    pub visibility: ModelVisibility,
    pub supported_in_api: bool,
    pub priority: i32,
    pub upgrade: Option<ModelInfoUpgrade>,
    pub base_instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_messages: Option<ModelMessages>,
    pub supports_reasoning_summaries: bool,
    pub support_verbosity: bool,
    pub default_verbosity: Option<adam_llm_types::Verbosity>,
    pub apply_patch_tool_type: Option<ApplyPatchToolType>,
    pub truncation_policy: TruncationPolicyConfig,
    pub supports_parallel_tool_calls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_token_limit: Option<i64>,
    #[serde(default = "default_effective_context_window_percent")]
    pub effective_context_window_percent: i64,
    #[serde(default)]
    pub experimental_supported_tools: Vec<String>,
}

const fn default_effective_context_window_percent() -> i64 {
    95
}

impl ModelInfo {
    pub fn auto_compact_token_limit(&self) -> Option<i64> {
        self.auto_compact_token_limit.or_else(|| {
            self.context_window
                .map(|context_window| (context_window * 9) / 10)
        })
    }

    pub fn supports_personality(&self) -> bool {
        self.semantic_model_info().supports_personality()
    }

    pub fn get_model_instructions(
        &self,
        personality: Option<adam_llm_types::Personality>,
    ) -> String {
        self.semantic_model_info()
            .get_model_instructions(personality)
    }

    pub fn semantic_model_info(&self) -> adam_llm_types::ModelInfo {
        adam_llm_types::ModelInfo {
            slug: self.slug.clone(),
            display_name: self.display_name.clone(),
            description: self.description.clone(),
            default_reasoning_level: self.default_reasoning_level,
            supported_reasoning_levels: self.supported_reasoning_levels.clone(),
            visibility: self.visibility,
            supported_in_api: self.supported_in_api,
            priority: self.priority,
            upgrade: self.upgrade.clone(),
            base_instructions: self.base_instructions.clone(),
            model_messages: self.model_messages.clone(),
            supports_reasoning_summaries: self.supports_reasoning_summaries,
            support_verbosity: self.support_verbosity,
            default_verbosity: self.default_verbosity,
            truncation_policy: self.truncation_policy,
            supports_parallel_tool_calls: self.supports_parallel_tool_calls,
            context_window: self.context_window,
            auto_compact_token_limit: self.auto_compact_token_limit,
            effective_context_window_percent: self.effective_context_window_percent,
        }
    }
}

impl From<adam_llm_types::ModelInfo> for ModelInfo {
    fn from(value: adam_llm_types::ModelInfo) -> Self {
        Self {
            slug: value.slug,
            display_name: value.display_name,
            description: value.description,
            default_reasoning_level: value.default_reasoning_level,
            supported_reasoning_levels: value.supported_reasoning_levels,
            shell_type: ConfigShellToolType::Default,
            visibility: value.visibility,
            supported_in_api: value.supported_in_api,
            priority: value.priority,
            upgrade: value.upgrade,
            base_instructions: value.base_instructions,
            model_messages: value.model_messages,
            supports_reasoning_summaries: value.supports_reasoning_summaries,
            support_verbosity: value.support_verbosity,
            default_verbosity: value.default_verbosity,
            apply_patch_tool_type: None,
            truncation_policy: value.truncation_policy,
            supports_parallel_tool_calls: value.supports_parallel_tool_calls,
            context_window: value.context_window,
            auto_compact_token_limit: value.auto_compact_token_limit,
            effective_context_window_percent: value.effective_context_window_percent,
            experimental_supported_tools: Vec::new(),
        }
    }
}

impl From<ModelInfo> for adam_llm_types::ModelInfo {
    fn from(value: ModelInfo) -> Self {
        value.semantic_model_info()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS, JsonSchema, Default)]
pub struct ModelsResponse {
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema, PartialEq)]
pub struct ModelUpgrade {
    pub id: String,
    pub reasoning_effort_mapping: Option<HashMap<ReasoningEffort, ReasoningEffort>>,
    pub migration_config_key: String,
    pub model_link: Option<String>,
    pub upgrade_copy: Option<String>,
    pub migration_markdown: Option<String>,
}

/// Metadata describing a Codex-supported model.
#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema, PartialEq)]
pub struct ModelPreset {
    /// Stable identifier for the preset.
    pub id: String,
    /// Model slug (e.g., "gpt-5").
    pub model: String,
    /// Optional provider identifier for provider-scoped custom models.
    #[serde(default)]
    pub model_provider_id: Option<String>,
    /// Display name shown in UIs.
    pub display_name: String,
    /// Short human description shown in UIs.
    pub description: String,
    /// Reasoning effort applied when none is explicitly chosen.
    pub default_reasoning_effort: ReasoningEffort,
    /// Supported reasoning effort options.
    pub supported_reasoning_efforts: Vec<ReasoningEffortPreset>,
    /// Whether this model supports personality-specific instructions.
    #[serde(default)]
    pub supports_personality: bool,
    /// Whether this is the default model for new users.
    pub is_default: bool,
    /// recommended upgrade model
    pub upgrade: Option<ModelUpgrade>,
    /// Whether this preset should appear in the picker UI.
    pub show_in_picker: bool,
    /// whether this model is supported in the api
    pub supported_in_api: bool,
}

/// Semantic version triple encoded as an array in JSON (e.g. [0, 62, 0]).
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS, JsonSchema)]
pub struct ClientVersion(pub i32, pub i32, pub i32);

impl From<&ModelUpgrade> for ModelInfoUpgrade {
    fn from(upgrade: &ModelUpgrade) -> Self {
        ModelInfoUpgrade {
            model: upgrade.id.clone(),
            migration_markdown: upgrade.migration_markdown.clone().unwrap_or_default(),
        }
    }
}

impl From<ModelInfo> for ModelPreset {
    fn from(info: ModelInfo) -> Self {
        let supports_personality = info.supports_personality();
        ModelPreset {
            id: info.slug.clone(),
            model: info.slug.clone(),
            model_provider_id: None,
            display_name: info.display_name,
            description: info.description.unwrap_or_default(),
            default_reasoning_effort: info
                .default_reasoning_level
                .unwrap_or(ReasoningEffort::None),
            supported_reasoning_efforts: info.supported_reasoning_levels.clone(),
            supports_personality,
            is_default: false,
            upgrade: info.upgrade.as_ref().map(|upgrade| ModelUpgrade {
                id: upgrade.model.clone(),
                reasoning_effort_mapping: reasoning_effort_mapping_from_presets(
                    &info.supported_reasoning_levels,
                ),
                migration_config_key: info.slug.clone(),
                model_link: None,
                upgrade_copy: None,
                migration_markdown: Some(upgrade.migration_markdown.clone()),
            }),
            show_in_picker: info.visibility == ModelVisibility::List,
            supported_in_api: info.supported_in_api,
        }
    }
}

impl ModelPreset {
    /// Filter models based on authentication mode.
    ///
    /// In ChatGPT mode, all models are visible. Otherwise, only API-supported models are shown.
    pub fn filter_by_auth(models: Vec<ModelPreset>, chatgpt_mode: bool) -> Vec<ModelPreset> {
        models
            .into_iter()
            .filter(|model| chatgpt_mode || model.supported_in_api)
            .collect()
    }

    /// Merge remote presets with existing presets, preferring remote when slugs match.
    ///
    /// Remote presets take precedence. Existing presets not in remote are appended with `is_default` set to false.
    pub fn merge(
        remote_presets: Vec<ModelPreset>,
        existing_presets: Vec<ModelPreset>,
    ) -> Vec<ModelPreset> {
        if remote_presets.is_empty() {
            return existing_presets;
        }

        let remote_slugs: HashSet<&str> = remote_presets
            .iter()
            .map(|preset| preset.model.as_str())
            .collect();

        let mut merged_presets = remote_presets.clone();
        for mut preset in existing_presets {
            if remote_slugs.contains(preset.model.as_str()) {
                continue;
            }
            preset.is_default = false;
            merged_presets.push(preset);
        }

        merged_presets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn test_model(spec: Option<ModelMessages>) -> ModelInfo {
        ModelInfo {
            slug: "test-model".to_string(),
            display_name: "Test Model".to_string(),
            description: None,
            default_reasoning_level: None,
            supported_reasoning_levels: vec![],
            shell_type: ConfigShellToolType::ShellCommand,
            visibility: ModelVisibility::List,
            supported_in_api: true,
            priority: 1,
            upgrade: None,
            base_instructions: "base".to_string(),
            model_messages: spec,
            supports_reasoning_summaries: false,
            support_verbosity: false,
            default_verbosity: None,
            apply_patch_tool_type: None,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
            supports_parallel_tool_calls: false,
            context_window: None,
            auto_compact_token_limit: None,
            effective_context_window_percent: 95,
            experimental_supported_tools: vec![],
        }
    }

    fn personality_variables() -> ModelInstructionsVariables {
        ModelInstructionsVariables {
            personality_default: Some("default".to_string()),
            personality_friendly: Some("friendly".to_string()),
            personality_pragmatic: Some("pragmatic".to_string()),
        }
    }

    #[test]
    fn uses_base_instructions_when_no_model_messages() {
        let model = test_model(None);
        let instructions = model.get_model_instructions(None);
        assert_eq!(instructions, "base");
    }

    #[test]
    fn uses_template_with_personality_message() {
        let model = test_model(Some(ModelMessages {
            instructions_template: Some("hello {{ personality }}".to_string()),
            instructions_variables: Some(personality_variables()),
        }));

        let instructions =
            model.get_model_instructions(Some(adam_llm_types::Personality::Friendly));
        assert_eq!(instructions, "hello friendly");
    }

    #[test]
    fn defaults_template_personality_message_when_none_selected() {
        let model = test_model(Some(ModelMessages {
            instructions_template: Some("hello {{ personality }}".to_string()),
            instructions_variables: Some(personality_variables()),
        }));

        let instructions = model.get_model_instructions(None);
        assert_eq!(instructions, "hello default");
    }

    #[test]
    fn falls_back_to_base_instructions_when_template_missing() {
        let model = test_model(Some(ModelMessages {
            instructions_template: None,
            instructions_variables: Some(personality_variables()),
        }));

        let instructions =
            model.get_model_instructions(Some(adam_llm_types::Personality::Friendly));
        assert_eq!(instructions, "base");
    }

    #[test]
    fn exposes_personality_messages() {
        let variables = personality_variables();
        assert_eq!(
            variables.get_personality_message(Some(adam_llm_types::Personality::Friendly)),
            Some("friendly".to_string()),
        );
        assert_eq!(
            variables.get_personality_message(Some(adam_llm_types::Personality::Pragmatic)),
            Some("pragmatic".to_string()),
        );
        assert_eq!(
            variables.get_personality_message(None),
            Some("default".to_string()),
        );
    }
}
