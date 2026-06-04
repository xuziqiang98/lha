use crate::product::agent::workflow::validation;
use crate::product::protocol::protocol::RolloutItem;
use crate::product::protocol::workflow::WorkflowDefinition;
use crate::product::protocol::workflow::WorkflowEvent;
use crate::product::protocol::workflow::WorkflowRolloutItem;
use crate::product::protocol::workflow::WorkflowStatus;
use crate::product::protocol::workflow::WorkflowValidationError;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

const MAX_ARTIFACT_BYTES: usize = 256 * 1024;
const MAX_TOTAL_ARTIFACT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct WorkflowArtifact {
    pub step_id: String,
    pub value: Value,
    pub hash: String,
    pub submitted_at_turn_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowSession {
    pub definition: WorkflowDefinition,
    pub status: WorkflowStatus,
    pub current_step: Option<String>,
    pub completed_steps: Vec<String>,
    pub artifacts: BTreeMap<String, WorkflowArtifact>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowTurnContext {
    pub definition: WorkflowDefinition,
    pub status: WorkflowStatus,
    pub current_step: Option<String>,
    pub completed_steps: Vec<String>,
    pub artifacts: BTreeMap<String, WorkflowArtifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ArtifactSubmission {
    pub step_id: String,
    pub artifact: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub(crate) enum WorkflowSubmissionResult {
    Accepted {
        workflow_id: String,
        completed_step: String,
        next_step: Option<String>,
        workflow_completed: bool,
    },
    Rejected {
        workflow_id: String,
        step_id: String,
        errors: Vec<WorkflowValidationError>,
    },
}

impl WorkflowSession {
    #[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
    pub fn new(definition: WorkflowDefinition) -> Result<Self, Vec<WorkflowValidationError>> {
        validation::validate_definition(&definition)?;
        let current_step = definition.steps.first().map(|step| step.id.clone());
        Ok(Self {
            definition,
            status: WorkflowStatus::InProgress,
            current_step,
            completed_steps: Vec::new(),
            artifacts: BTreeMap::new(),
        })
    }

    // Reserved for wiring workflow state reconstruction into resume/fork.
    #[allow(dead_code)]
    pub fn from_rollout_items(
        definition: WorkflowDefinition,
        items: &[RolloutItem],
    ) -> Result<Self, Vec<WorkflowValidationError>> {
        let mut session = Self::new(definition)?;
        for item in items {
            let RolloutItem::Workflow(workflow_item) = item else {
                continue;
            };
            if workflow_item.workflow_id != session.definition.id {
                continue;
            }
            session.apply_event(workflow_item);
        }
        Ok(session)
    }

    pub fn snapshot(&self) -> WorkflowTurnContext {
        WorkflowTurnContext {
            definition: self.definition.clone(),
            status: self.status.clone(),
            current_step: self.current_step.clone(),
            completed_steps: self.completed_steps.clone(),
            artifacts: self.artifacts.clone(),
        }
    }

    #[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
    pub fn started_item(&self, turn_id: &str) -> Option<RolloutItem> {
        let current_step = self.current_step.clone()?;
        Some(self.rollout_item(turn_id, WorkflowEvent::Started { current_step }))
    }

    #[allow(dead_code)]
    fn apply_event(&mut self, item: &WorkflowRolloutItem) {
        match &item.event {
            WorkflowEvent::Started { current_step } => {
                self.status = WorkflowStatus::InProgress;
                self.current_step = Some(current_step.clone());
            }
            WorkflowEvent::ArtifactSubmitted {
                step_id,
                artifact,
                artifact_hash,
            } => {
                self.artifacts.insert(
                    step_id.clone(),
                    WorkflowArtifact {
                        step_id: step_id.clone(),
                        value: artifact.clone(),
                        hash: artifact_hash.clone(),
                        submitted_at_turn_id: item.turn_id.clone(),
                    },
                );
            }
            WorkflowEvent::StepCompleted { step_id, next_step } => {
                if !self.completed_steps.contains(step_id) {
                    self.completed_steps.push(step_id.clone());
                }
                self.current_step = next_step.clone();
            }
            WorkflowEvent::ValidationFailed { .. } => {}
            WorkflowEvent::Completed => {
                self.status = WorkflowStatus::Completed;
                self.current_step = None;
            }
            WorkflowEvent::Failed { .. } => {
                self.status = WorkflowStatus::Failed;
            }
        }
    }

    pub fn submit_artifact(
        &mut self,
        turn_id: &str,
        submission: ArtifactSubmission,
    ) -> (WorkflowSubmissionResult, Vec<RolloutItem>) {
        let workflow_id = self.definition.id.clone();
        let step_id = submission.step_id.clone();
        let errors = self.validate_submission(&submission);
        if !errors.is_empty() {
            let items = vec![self.rollout_item(
                turn_id,
                WorkflowEvent::ValidationFailed {
                    step_id: step_id.clone(),
                    errors: errors.clone(),
                },
            )];
            return (
                WorkflowSubmissionResult::Rejected {
                    workflow_id,
                    step_id,
                    errors,
                },
                items,
            );
        }

        let hash = artifact_hash(&submission.artifact);
        let artifact = WorkflowArtifact {
            step_id: step_id.clone(),
            value: submission.artifact,
            hash: hash.clone(),
            submitted_at_turn_id: turn_id.to_string(),
        };
        self.artifacts.insert(step_id.clone(), artifact.clone());
        self.completed_steps.push(step_id.clone());
        let next_step = self.next_step_after(&step_id);
        self.current_step = next_step.clone();
        let workflow_completed = next_step.is_none();
        if workflow_completed {
            self.status = WorkflowStatus::Completed;
        }

        let mut items = vec![self.rollout_item(
            turn_id,
            WorkflowEvent::ArtifactSubmitted {
                step_id: step_id.clone(),
                artifact: artifact.value,
                artifact_hash: hash,
            },
        )];
        items.push(self.rollout_item(
            turn_id,
            WorkflowEvent::StepCompleted {
                step_id: step_id.clone(),
                next_step: next_step.clone(),
            },
        ));
        if workflow_completed {
            items.push(self.rollout_item(turn_id, WorkflowEvent::Completed));
        }

        (
            WorkflowSubmissionResult::Accepted {
                workflow_id,
                completed_step: step_id,
                next_step,
                workflow_completed,
            },
            items,
        )
    }

    fn validate_submission(&self, submission: &ArtifactSubmission) -> Vec<WorkflowValidationError> {
        let mut errors = Vec::new();
        if self.status == WorkflowStatus::Completed {
            errors.push(error(
                "workflow_completed",
                "/step_id",
                "workflow is already completed",
            ));
            return errors;
        }

        if self.completed_steps.contains(&submission.step_id) {
            errors.push(error(
                "step_already_completed",
                "/step_id",
                format!("step `{}` is already completed", submission.step_id),
            ));
        }

        let Some(current_step) = self.current_step.as_ref() else {
            errors.push(error(
                "workflow_not_started",
                "/step_id",
                "workflow has no current step",
            ));
            return errors;
        };
        if current_step != &submission.step_id {
            errors.push(error(
                "step_not_current",
                "/step_id",
                format!(
                    "step `{}` cannot be submitted while current step is `{current_step}`",
                    submission.step_id
                ),
            ));
            return errors;
        }

        let Some(step) = self
            .definition
            .steps
            .iter()
            .find(|step| step.id == submission.step_id)
        else {
            errors.push(error(
                "unknown_step",
                "/step_id",
                format!("unknown workflow step `{}`", submission.step_id),
            ));
            return errors;
        };

        let completed: BTreeSet<&str> = self.completed_steps.iter().map(String::as_str).collect();
        for dependency in &step.depends_on {
            if !completed.contains(dependency.as_str()) || !self.artifacts.contains_key(dependency)
            {
                errors.push(error(
                    "dependency_missing",
                    "/step_id",
                    format!("dependency `{dependency}` has no accepted artifact"),
                ));
            }
        }

        let artifact_bytes = serde_json::to_vec(&submission.artifact)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        if artifact_bytes > MAX_ARTIFACT_BYTES {
            errors.push(error(
                "artifact_too_large",
                "/artifact",
                format!("artifact is {artifact_bytes} bytes; maximum is {MAX_ARTIFACT_BYTES}"),
            ));
        }
        let total_bytes = self.total_artifact_bytes().saturating_add(artifact_bytes);
        if total_bytes > MAX_TOTAL_ARTIFACT_BYTES {
            errors.push(error(
                "artifact_too_large",
                "/artifact",
                format!(
                    "workflow artifacts total {total_bytes} bytes; maximum is {MAX_TOTAL_ARTIFACT_BYTES}"
                ),
            ));
        }

        errors.extend(validation::validate_artifact(
            &self.definition,
            &submission.step_id,
            &submission.artifact,
            &self.artifacts,
        ));
        errors
    }

    fn next_step_after(&self, step_id: &str) -> Option<String> {
        self.definition
            .steps
            .iter()
            .position(|step| step.id == step_id)
            .and_then(|idx| self.definition.steps.get(idx + 1))
            .map(|step| step.id.clone())
    }

    fn total_artifact_bytes(&self) -> usize {
        self.artifacts
            .values()
            .map(|artifact| {
                serde_json::to_vec(&artifact.value).map_or(usize::MAX, |bytes| bytes.len())
            })
            .sum()
    }

    fn rollout_item(&self, turn_id: &str, event: WorkflowEvent) -> RolloutItem {
        RolloutItem::Workflow(WorkflowRolloutItem {
            workflow_id: self.definition.id.clone(),
            identity_id: self.definition.identity_id.clone(),
            turn_id: turn_id.to_string(),
            event,
        })
    }
}

impl WorkflowTurnContext {
    pub fn allowed_tools(&self) -> Option<BTreeSet<String>> {
        if self.status == WorkflowStatus::Completed {
            return None;
        }
        let current_step = self.current_step.as_ref()?;
        let mut allowed = self
            .definition
            .steps
            .iter()
            .find(|step| &step.id == current_step)
            .and_then(|step| step.allowed_tools.clone())
            .map(|tools| tools.into_iter().collect::<BTreeSet<_>>())?;
        allowed.insert("workflow_submit_artifact".to_string());
        Some(allowed)
    }

    pub fn developer_instructions(&self) -> Option<String> {
        if self.status == WorkflowStatus::Completed {
            return None;
        }
        let current_step_id = self.current_step.as_ref()?;
        let step = self
            .definition
            .steps
            .iter()
            .find(|step| &step.id == current_step_id)?;
        let mut message = format!(
            "<workflow>\nWorkflow: {}\nCurrent step: {} ({})\nCompleted steps: {}\n\n{}\n\nYou must submit the artifact for the current step by calling workflow_submit_artifact. Do not submit artifacts for later steps.\n",
            self.definition.id,
            step.id,
            step.label,
            self.completed_steps.join(", "),
            step.prompt
        );
        if !self.artifacts.is_empty() {
            message.push_str("\nAccepted artifacts:\n");
            for (step_id, artifact) in &self.artifacts {
                message.push_str(&format!(
                    "- {step_id} (artifact step {}, hash {}, turn {}): {}\n",
                    artifact.step_id, artifact.hash, artifact.submitted_at_turn_id, artifact.value
                ));
            }
        }
        message.push_str("</workflow>");
        Some(message)
    }
}

fn artifact_hash(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let digest = sha2::Sha256::digest(bytes);
    format!("{digest:x}")
}

fn error(
    code: impl Into<String>,
    path: impl Into<String>,
    message: impl Into<String>,
) -> WorkflowValidationError {
    WorkflowValidationError::new(code, path, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::protocol::workflow::WorkflowMode;
    use crate::product::protocol::workflow::WorkflowStepDefinition;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn definition() -> WorkflowDefinition {
        WorkflowDefinition {
            id: "architect_v1".to_string(),
            identity_id: "architect".to_string(),
            mode: WorkflowMode::Sequential,
            steps: vec![
                WorkflowStepDefinition {
                    id: "requirements".to_string(),
                    label: "Requirements".to_string(),
                    prompt: "collect requirements".to_string(),
                    depends_on: Vec::new(),
                    output_schema: json!({
                        "type": "object",
                        "properties": { "requirements": { "type": "array" } },
                        "required": ["requirements"],
                        "additionalProperties": false
                    }),
                    allowed_tools: Some(vec!["read_file".to_string()]),
                    validators: Vec::new(),
                },
                WorkflowStepDefinition {
                    id: "architecture".to_string(),
                    label: "Architecture".to_string(),
                    prompt: "design architecture".to_string(),
                    depends_on: vec!["requirements".to_string()],
                    output_schema: json!({
                        "type": "object",
                        "properties": { "components": { "type": "array" } },
                        "required": ["components"],
                        "additionalProperties": false
                    }),
                    allowed_tools: None,
                    validators: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn advances_sequentially_after_valid_artifact() {
        let mut session = WorkflowSession::new(definition()).expect("valid definition");
        let (result, items) = session.submit_artifact(
            "turn-1",
            ArtifactSubmission {
                step_id: "requirements".to_string(),
                artifact: json!({ "requirements": [] }),
            },
        );

        assert!(matches!(result, WorkflowSubmissionResult::Accepted { .. }));
        assert_eq!(session.current_step.as_deref(), Some("architecture"));
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn rejects_skipped_step() {
        let mut session = WorkflowSession::new(definition()).expect("valid definition");
        let (result, _) = session.submit_artifact(
            "turn-1",
            ArtifactSubmission {
                step_id: "architecture".to_string(),
                artifact: json!({ "components": [] }),
            },
        );

        let WorkflowSubmissionResult::Rejected { errors, .. } = result else {
            panic!("expected rejection");
        };
        assert_eq!(errors[0].code, "step_not_current");
    }

    #[test]
    fn rebuilds_state_from_rollout_items() {
        let mut session = WorkflowSession::new(definition()).expect("valid definition");
        let (_, mut items) = session.submit_artifact(
            "turn-1",
            ArtifactSubmission {
                step_id: "requirements".to_string(),
                artifact: json!({ "requirements": [] }),
            },
        );
        items.insert(0, session.started_item("turn-0").expect("started item"));

        let rebuilt = WorkflowSession::from_rollout_items(definition(), &items)
            .expect("workflow should rebuild");

        assert_eq!(rebuilt.current_step.as_deref(), Some("architecture"));
        assert_eq!(rebuilt.completed_steps, vec!["requirements".to_string()]);
        assert!(rebuilt.artifacts.contains_key("requirements"));
    }
}
