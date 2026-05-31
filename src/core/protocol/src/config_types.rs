use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;
use ts_rs::TS;

use crate::openai_models::ReasoningEffort;
pub use lha_llm_types::Personality;
pub use lha_llm_types::ReasoningSummary;
pub use lha_llm_types::Verbosity;
pub use lha_llm_types::WebSearchMode;

#[derive(
    Deserialize, Debug, Clone, Copy, PartialEq, Default, Serialize, Display, JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum SandboxMode {
    #[serde(rename = "read-only")]
    #[default]
    ReadOnly,

    #[serde(rename = "workspace-write")]
    WorkspaceWrite,

    #[serde(rename = "danger-full-access")]
    DangerFullAccess,
}

#[derive(
    Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Display, JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum WindowsSandboxLevel {
    #[default]
    Disabled,
    RestrictedToken,
    Elevated,
}

/// Represents the trust level for a project directory.
/// This determines the approval policy and sandbox mode applied.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Display, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum TrustLevel {
    Trusted,
    Untrusted,
}

/// Built-in LHA identity kind.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, JsonSchema, TS, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    #[default]
    Nobody,
    Planner,
    Programmer,
    Explorer,
    Reviewer,
}

/// Identity for an LHA session.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub struct Identity {
    pub kind: IdentityKind,
    pub settings: Settings,
}

impl Identity {
    /// Returns a reference to the settings.
    fn settings_ref(&self) -> &Settings {
        &self.settings
    }

    pub fn model(&self) -> &str {
        self.settings_ref().model.as_str()
    }

    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.settings_ref().reasoning_effort
    }

    /// Updates the identity with new model and/or effort values.
    ///
    /// - `model`: `Some(s)` to update the model, `None` to keep the current model
    /// - `effort`: `Some(Some(e))` to set effort to `e`, `Some(None)` to clear effort, `None` to keep current effort
    /// - `developer_instructions`: `Some(Some(s))` to set instructions, `Some(None)` to clear them, `None` to keep current
    ///
    /// Returns a new `Identity` with updated values, preserving the identity kind.
    pub fn with_updates(
        &self,
        model: Option<String>,
        effort: Option<Option<ReasoningEffort>>,
        developer_instructions: Option<Option<String>>,
    ) -> Self {
        let settings = self.settings_ref();
        let updated_settings = Settings {
            model: model.unwrap_or_else(|| settings.model.clone()),
            reasoning_effort: effort.unwrap_or(settings.reasoning_effort),
            developer_instructions: developer_instructions
                .unwrap_or_else(|| settings.developer_instructions.clone()),
        };

        Identity {
            kind: self.kind,
            settings: updated_settings,
        }
    }

    /// Applies a mask to this identity, returning a new identity
    /// with the mask values applied. Fields in the mask that are `Some` will override
    /// the corresponding fields, while `None` values will preserve the original values.
    ///
    /// The `name` and `capabilities` fields in the mask are ignored as metadata for the mask itself.
    pub fn apply_mask(&self, mask: &IdentityMask) -> Self {
        let settings = self.settings_ref();
        Identity {
            kind: mask.kind.unwrap_or(self.kind),
            settings: Settings {
                model: mask.model.clone().unwrap_or_else(|| settings.model.clone()),
                reasoning_effort: mask.reasoning_effort.unwrap_or(settings.reasoning_effort),
                developer_instructions: mask
                    .developer_instructions
                    .clone()
                    .unwrap_or_else(|| settings.developer_instructions.clone()),
            },
        }
    }
}

/// Settings for an identity.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, JsonSchema, TS)]
pub struct Settings {
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub developer_instructions: Option<String>,
}

/// Capabilities associated with an identity.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, JsonSchema, TS)]
pub struct IdentityCapabilities {
    /// Whether this identity is intended to have write-capable tools.
    /// This is currently metadata only; tool exposure is not filtered from this field yet.
    pub write_tools: bool,
}

/// A mask for identity settings, allowing partial updates.
/// All fields except `name` are optional, enabling selective updates.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, JsonSchema, TS)]
pub struct IdentityMask {
    pub name: String,
    pub kind: Option<IdentityKind>,
    pub model: Option<String>,
    pub reasoning_effort: Option<Option<ReasoningEffort>>,
    pub developer_instructions: Option<Option<String>>,
    pub capabilities: IdentityCapabilities,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn apply_mask_can_clear_optional_fields() {
        let identity = Identity {
            kind: IdentityKind::Programmer,
            settings: Settings {
                model: "gpt-5.2-codex".to_string(),
                reasoning_effort: Some(ReasoningEffort::High),
                developer_instructions: Some("stay focused".to_string()),
            },
        };
        let mask = IdentityMask {
            name: "Clear".to_string(),
            kind: None,
            model: None,
            reasoning_effort: Some(None),
            developer_instructions: Some(None),
            capabilities: IdentityCapabilities { write_tools: true },
        };

        let expected = Identity {
            kind: IdentityKind::Programmer,
            settings: Settings {
                model: "gpt-5.2-codex".to_string(),
                reasoning_effort: None,
                developer_instructions: None,
            },
        };
        assert_eq!(expected, identity.apply_mask(&mask));
    }
}
