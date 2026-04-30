use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

/// Declarative workflow definition attached to an identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "workflow/")]
pub struct WorkflowDefinition {
    pub id: String,
    #[serde(default)]
    pub identity_id: String,
    #[serde(default)]
    pub mode: WorkflowMode,
    pub steps: Vec<WorkflowStepDefinition>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS, Default)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "workflow/")]
pub enum WorkflowMode {
    #[default]
    Sequential,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "workflow/")]
pub struct WorkflowStepDefinition {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub output_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<WorkflowValidatorDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export_to = "workflow/")]
pub enum WorkflowValidatorDefinition {
    UniqueIds {
        path: String,
    },
    ReferencesExistingIds {
        from: String,
        target: String,
        target_path: String,
    },
    ReferencesAllIds {
        from: String,
        target: String,
        target_path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "workflow/")]
pub enum WorkflowStatus {
    NotStarted,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "workflow/")]
pub struct WorkflowValidationError {
    pub code: String,
    pub path: String,
    pub message: String,
}

impl WorkflowValidationError {
    pub fn new(
        code: impl Into<String>,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "workflow/")]
pub struct WorkflowRolloutItem {
    pub workflow_id: String,
    pub identity_id: String,
    pub turn_id: String,
    pub event: WorkflowEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export_to = "workflow/")]
pub enum WorkflowEvent {
    Started {
        current_step: String,
    },
    ArtifactSubmitted {
        step_id: String,
        artifact: Value,
        artifact_hash: String,
    },
    StepCompleted {
        step_id: String,
        next_step: Option<String>,
    },
    ValidationFailed {
        step_id: String,
        errors: Vec<WorkflowValidationError>,
    },
    Completed,
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "workflow/")]
pub struct WorkflowUpdateEvent {
    pub workflow_id: String,
    pub status: WorkflowStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_step: Option<String>,
    pub completed_steps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<WorkflowValidationError>,
}
