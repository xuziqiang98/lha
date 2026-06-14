use std::collections::BTreeMap;

use async_trait::async_trait;
use lha_llm::AdditionalProperties;
use lha_llm::FunctionToolDescriptor;
use lha_llm::ToolDescriptor;
use lha_llm::ToolInputSchema;
use serde::Deserialize;

use crate::product::agent::function_tool::FunctionCallError;
use crate::product::agent::tools::context::ToolInvocation;
use crate::product::agent::tools::context::ToolOutput;
use crate::product::agent::tools::context::ToolPayload;
use crate::product::agent::tools::registry::ToolHandler;
use crate::product::agent::tools::registry::ToolKind;

pub(crate) const INPUT_RETRIEVE_TOOL_NAME: &str = "lha_input_retrieve";

pub(crate) struct InputRetrieveHandler;

#[derive(Deserialize)]
struct InputRetrieveArgs {
    hash: String,
    query: Option<String>,
}

#[async_trait]
impl ToolHandler for InputRetrieveHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            ToolPayload::Custom { .. }
            | ToolPayload::LocalShell { .. }
            | ToolPayload::Mcp { .. } => {
                return Err(FunctionCallError::RespondToModel(
                    "lha_input_retrieve received unsupported payload".to_string(),
                ));
            }
        };
        let args: InputRetrieveArgs = serde_json::from_str(&arguments).map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to parse lha_input_retrieve arguments: {err}"
            ))
        })?;

        let result = invocation
            .session
            .services
            .input_slimming_store
            .retrieve(args.hash.as_str(), args.query.as_deref())
            .await;

        let strategy = result
            .strategy
            .map(super::InputSlimmingStrategy::as_str)
            .unwrap_or("unknown");
        let tool_name = result.tool_name.as_deref().unwrap_or("unknown");
        invocation.turn.runtime.get_otel_manager().counter(
            "lha.input_slimming.retrieve",
            1,
            &[
                ("success", if result.success { "true" } else { "false" }),
                ("strategy", strategy),
                ("tool_name", tool_name),
            ],
        );
        if !result.success {
            invocation.turn.runtime.get_otel_manager().counter(
                "lha.input_slimming.retrieve_miss",
                1,
                &[],
            );
        }
        if let Some(matched) = result.query_matched {
            invocation.turn.runtime.get_otel_manager().counter(
                "lha.input_slimming.retrieve_query",
                1,
                &[("matched", if matched { "true" } else { "false" })],
            );
        }

        Ok(ToolOutput::Function {
            content: result.content,
            content_items: None,
            success: Some(result.success),
        })
    }
}

pub(crate) fn create_lha_input_retrieve_tool() -> ToolDescriptor {
    let mut properties = BTreeMap::new();
    properties.insert(
        "hash".to_string(),
        ToolInputSchema::String {
            description: Some(
                "The 24-character hash from an <<lha-input:...>> marker.".to_string(),
            ),
            enum_values: None,
        },
    );
    properties.insert(
        "query".to_string(),
        ToolInputSchema::String {
            description: Some(
                "Optional text to search for within the stored original payload.".to_string(),
            ),
            enum_values: None,
        },
    );

    ToolDescriptor::Function(FunctionToolDescriptor {
        name: INPUT_RETRIEVE_TOOL_NAME.to_string(),
        description: "Retrieve the original tool output behind an Input Slimming marker. Use this when a compressed snippet with <<lha-input:...>> does not contain enough detail; pass an optional query to retrieve only relevant lines.".to_string(),
        strict: false,
        parameters: ToolInputSchema::Object {
            properties,
            required: Some(vec!["hash".to_string()]),
            additional_properties: Some(AdditionalProperties::Boolean(false)),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::codex::make_session_and_context;
    use crate::product::agent::input_slimming::InputSlimmingStrategy;
    use crate::product::agent::input_slimming::StoredInputMetadata;
    use crate::product::agent::tools::context::ToolPayload;
    use crate::product::agent::turn_diff_tracker::TurnDiffTracker;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[test]
    fn tool_spec_shape_is_pinned() {
        let ToolDescriptor::Function(tool) = create_lha_input_retrieve_tool() else {
            panic!("expected function tool");
        };

        assert_eq!(tool.name, INPUT_RETRIEVE_TOOL_NAME);
        assert!(!tool.strict);
        let ToolInputSchema::Object {
            properties,
            required,
            additional_properties,
        } = tool.parameters
        else {
            panic!("expected object schema");
        };
        assert!(properties.contains_key("hash"));
        assert!(properties.contains_key("query"));
        assert_eq!(required, Some(vec!["hash".to_string()]));
        assert_eq!(
            additional_properties,
            Some(AdditionalProperties::Boolean(false))
        );
    }

    #[tokio::test]
    async fn handler_retrieves_original_payload() {
        let (session, turn_context) = make_session_and_context().await;
        let hash = session
            .services
            .input_slimming_store
            .put(
                "original payload".to_string(),
                StoredInputMetadata {
                    strategy: InputSlimmingStrategy::PlainTextHeadTail,
                    tool_name: "shell".to_string(),
                    original_tokens: 3,
                    compressed_tokens: 1,
                    created_turn_id: "turn-1".to_string(),
                },
            )
            .await;
        let invocation = ToolInvocation {
            session: Arc::new(session),
            turn: Arc::new(turn_context),
            tracker: Arc::new(Mutex::new(TurnDiffTracker::new())),
            call_id: "call".to_string(),
            tool_name: INPUT_RETRIEVE_TOOL_NAME.to_string(),
            payload: ToolPayload::Function {
                arguments: format!(r#"{{"hash":"{hash}"}}"#),
            },
        };

        let output = InputRetrieveHandler
            .handle(invocation)
            .await
            .expect("tool succeeds");

        match output {
            ToolOutput::Function {
                content,
                content_items,
                success,
            } => {
                assert!(content.contains("original payload"));
                assert_eq!(content_items, None);
                assert_eq!(success, Some(true));
            }
        }
    }

    #[tokio::test]
    async fn handler_reports_missing_hash_as_failure() {
        let (session, turn_context) = make_session_and_context().await;
        let invocation = ToolInvocation {
            session: Arc::new(session),
            turn: Arc::new(turn_context),
            tracker: Arc::new(Mutex::new(TurnDiffTracker::new())),
            call_id: "call".to_string(),
            tool_name: INPUT_RETRIEVE_TOOL_NAME.to_string(),
            payload: ToolPayload::Function {
                arguments: r#"{"hash":"missing"}"#.to_string(),
            },
        };

        let output = InputRetrieveHandler
            .handle(invocation)
            .await
            .expect("tool succeeds");

        match output {
            ToolOutput::Function {
                content,
                content_items,
                success,
            } => {
                assert!(content.contains("store miss"));
                assert_eq!(content_items, None);
                assert_eq!(success, Some(false));
            }
        }
    }
}
