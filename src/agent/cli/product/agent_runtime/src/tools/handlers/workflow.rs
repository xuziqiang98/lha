use async_trait::async_trait;

use crate::product::agent::function_tool::FunctionCallError;
use crate::product::agent::tools::context::ToolInvocation;
use crate::product::agent::tools::context::ToolOutput;
use crate::product::agent::tools::context::ToolPayload;
use crate::product::agent::tools::handlers::parse_arguments;
use crate::product::agent::tools::registry::ToolHandler;
use crate::product::agent::tools::registry::ToolKind;
use crate::product::agent::tools::spec::JsonSchema;
use crate::product::agent::workflow::ArtifactSubmission;
use lha_llm::FunctionToolDescriptor;
use lha_llm::ToolDescriptor;
use std::collections::BTreeMap;
use std::sync::LazyLock;

pub struct WorkflowHandler;

pub static WORKFLOW_SUBMIT_ARTIFACT_TOOL: LazyLock<ToolDescriptor> = LazyLock::new(|| {
    let properties = BTreeMap::from([
        (
            "step_id".to_string(),
            JsonSchema::String {
                description: Some(
                    "The current workflow step id for the artifact being submitted.".to_string(),
                ),
                enum_values: None,
            },
        ),
        (
            "artifact".to_string(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(true.into()),
            },
        ),
    ]);

    ToolDescriptor::Function(FunctionToolDescriptor {
        name: "workflow_submit_artifact".to_string(),
        description: "Submit the JSON artifact for the current workflow step. The workflow only advances if the artifact passes validation.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["step_id".to_string(), "artifact".to_string()]),
            additional_properties: Some(false.into()),
        },
    })
});

#[async_trait]
impl ToolHandler for WorkflowHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "workflow_submit_artifact handler received unsupported payload".to_string(),
                ));
            }
        };

        let submission: ArtifactSubmission = parse_arguments(&arguments)?;
        let result = session
            .submit_workflow_artifact(turn.as_ref(), submission)
            .await;
        let success = matches!(
            result,
            crate::product::agent::workflow::WorkflowSubmissionResult::Accepted { .. }
        );
        let content = serde_json::to_string(&result).map_err(|err| {
            FunctionCallError::Fatal(format!(
                "failed to serialize workflow_submit_artifact response: {err}"
            ))
        })?;

        Ok(ToolOutput::Function {
            content,
            content_items: None,
            success: Some(success),
        })
    }
}
