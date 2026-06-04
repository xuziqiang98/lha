use crate::Error;
use crate::kernel::ToolFuture;
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
                } => response.map_err(Error::from),
            }
        })
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
