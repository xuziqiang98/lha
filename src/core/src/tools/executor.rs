use crate::Error;
use crate::kernel::ToolFuture;
use crate::tools::ToolError;
use crate::tools::ToolInvocation;
use crate::tools::ToolPayload;
use crate::tools::ToolRegistry;
use lha_llm::ToolCallRequest;
use lha_llm::ToolResultItem;
use lha_llm::ToolResultPayload;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    parallel_execution: Arc<RwLock<()>>,
}

impl ToolExecutor {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            registry,
            parallel_execution: Arc::new(RwLock::new(())),
        }
    }

    pub fn handle_tool_call(
        self,
        call: ToolCallRequest,
        cancellation_token: CancellationToken,
    ) -> ToolFuture<Error> {
        let supports_parallel = self
            .registry
            .supports_parallel_tool_calls(call.tool_name.as_str());

        Box::pin(async move {
            tokio::select! {
                _ = cancellation_token.cancelled() => Ok(Self::aborted_response(&call)),
                response = async {
                    let invocation = ToolInvocation {
                        call_id: call.call_id.clone(),
                        tool_name: call.tool_name.clone(),
                        payload: ToolPayload::from_llm(call.payload.clone()),
                    };
                    if supports_parallel {
                        let _guard = self.parallel_execution.read().await;
                        self.registry.dispatch(invocation, cancellation_token.child_token()).await
                    } else {
                        let _guard = self.parallel_execution.write().await;
                        self.registry.dispatch(invocation, cancellation_token.child_token()).await
                    }
                } => match response {
                    Ok(response) => Ok(response),
                    Err(ToolError::RespondToModel(message)) => {
                        Ok(Self::failure_response(&call, message))
                    }
                    Err(ToolError::Fatal(message)) => Err(Error::Tool(ToolError::Fatal(message))),
                },
            }
        })
    }

    fn failure_response(call: &ToolCallRequest, message: String) -> ToolResultItem {
        match call.payload {
            lha_llm::ToolCallPayload::TextInput { .. } => ToolResultItem {
                call_id: call.call_id.clone(),
                tool_name: call.tool_name.clone(),
                payload: ToolResultPayload::Text { output: message },
            },
            lha_llm::ToolCallPayload::JsonArguments { .. } => ToolResultItem {
                call_id: call.call_id.clone(),
                tool_name: call.tool_name.clone(),
                payload: ToolResultPayload::Structured {
                    content: message,
                    content_items: None,
                    success: Some(false),
                },
            },
        }
    }

    fn aborted_response(call: &ToolCallRequest) -> ToolResultItem {
        match call.payload {
            lha_llm::ToolCallPayload::TextInput { .. } => ToolResultItem {
                call_id: call.call_id.clone(),
                tool_name: call.tool_name.clone(),
                payload: ToolResultPayload::Text {
                    output: "aborted by user".to_string(),
                },
            },
            lha_llm::ToolCallPayload::JsonArguments { .. } => ToolResultItem {
                call_id: call.call_id.clone(),
                tool_name: call.tool_name.clone(),
                payload: ToolResultPayload::Structured {
                    content: "aborted by user".to_string(),
                    content_items: None,
                    success: Some(false),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolHandler;
    use crate::tools::ToolOutput;
    use crate::tools::ToolRegistryBuilder;
    use async_trait::async_trait;
    use lha_llm::FunctionToolDescriptor;
    use lha_llm::ToolCallPayload;
    use lha_llm::ToolDescriptor;
    use lha_llm::ToolInputSchema;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct StubTool {
        result: Result<ToolOutput, ToolError>,
    }

    #[async_trait]
    impl ToolHandler for StubTool {
        fn spec(&self) -> ToolDescriptor {
            ToolDescriptor::Function(FunctionToolDescriptor {
                name: "stub_tool".to_string(),
                description: "stub tool".to_string(),
                strict: false,
                parameters: ToolInputSchema::Object {
                    properties: BTreeMap::new(),
                    required: Some(Vec::new()),
                    additional_properties: Some(true.into()),
                },
            })
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
            _cancellation_token: CancellationToken,
        ) -> Result<ToolOutput, ToolError> {
            self.result.clone()
        }
    }

    fn tool_call(tool_name: &str, payload: ToolCallPayload) -> ToolCallRequest {
        ToolCallRequest {
            id: None,
            tool_name: tool_name.to_string(),
            call_id: "call-1".to_string(),
            payload,
        }
    }

    fn executor_with(handler: Option<StubTool>) -> ToolExecutor {
        let mut builder = ToolRegistryBuilder::new();
        if let Some(handler) = handler {
            builder.register_handler(Arc::new(handler));
        }
        ToolExecutor::new(Arc::new(builder.build()))
    }

    #[tokio::test]
    async fn unknown_tool_call_becomes_failed_tool_result() {
        let executor = executor_with(None);
        let response = executor
            .handle_tool_call(
                tool_call(
                    "missing_tool",
                    ToolCallPayload::JsonArguments {
                        arguments: "{}".to_string(),
                    },
                ),
                CancellationToken::new(),
            )
            .await
            .expect("unknown tool call should produce a tool result");

        assert_eq!(
            response,
            ToolResultItem {
                call_id: "call-1".to_string(),
                tool_name: "missing_tool".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "unsupported call: missing_tool".to_string(),
                    content_items: None,
                    success: Some(false),
                },
            }
        );
    }

    #[tokio::test]
    async fn unknown_custom_tool_call_becomes_text_tool_result() {
        let executor = executor_with(None);
        let response = executor
            .handle_tool_call(
                tool_call(
                    "missing_tool",
                    ToolCallPayload::TextInput {
                        input: "hi".to_string(),
                    },
                ),
                CancellationToken::new(),
            )
            .await
            .expect("unknown custom tool call should produce a tool result");

        assert_eq!(
            response,
            ToolResultItem {
                call_id: "call-1".to_string(),
                tool_name: "missing_tool".to_string(),
                payload: ToolResultPayload::Text {
                    output: "unsupported custom tool call: missing_tool".to_string(),
                },
            }
        );
    }

    #[tokio::test]
    async fn handler_respond_to_model_becomes_failed_tool_result() {
        let executor = executor_with(Some(StubTool {
            result: Err(ToolError::RespondToModel("invalid arguments".to_string())),
        }));
        let response = executor
            .handle_tool_call(
                tool_call(
                    "stub_tool",
                    ToolCallPayload::JsonArguments {
                        arguments: "not json".to_string(),
                    },
                ),
                CancellationToken::new(),
            )
            .await
            .expect("respond-to-model error should produce a tool result");

        assert_eq!(
            response,
            ToolResultItem {
                call_id: "call-1".to_string(),
                tool_name: "stub_tool".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "invalid arguments".to_string(),
                    content_items: None,
                    success: Some(false),
                },
            }
        );
    }

    #[tokio::test]
    async fn handler_fatal_error_fails_the_tool_future() {
        let executor = executor_with(Some(StubTool {
            result: Err(ToolError::Fatal("boom".to_string())),
        }));
        let err = executor
            .handle_tool_call(
                tool_call(
                    "stub_tool",
                    ToolCallPayload::JsonArguments {
                        arguments: "{}".to_string(),
                    },
                ),
                CancellationToken::new(),
            )
            .await
            .expect_err("fatal tool error should propagate");

        match err {
            Error::Tool(ToolError::Fatal(message)) => assert_eq!(message, "boom"),
            other => panic!("expected fatal tool error, got: {other:?}"),
        }
    }
}
