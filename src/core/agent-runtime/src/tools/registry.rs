use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use adam_llm::ToolDescriptor;
use adam_llm::ToolResultItem;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolError {
    #[error("{0}")]
    Fatal(String),
    #[error("{0}")]
    RespondToModel(String),
}

#[derive(Debug, Clone)]
pub struct ConfiguredTool {
    pub spec: ToolDescriptor,
    pub supports_parallel_tool_calls: bool,
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn spec(&self) -> ToolDescriptor;

    fn supports_parallel_tool_calls(&self) -> bool {
        false
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        cancellation_token: CancellationToken,
    ) -> Result<ToolOutput, ToolError>;
}

pub struct ToolRegistry {
    handlers: HashMap<String, Arc<dyn ToolHandler>>,
    specs: Vec<ConfiguredTool>,
}

impl ToolRegistry {
    pub fn new(
        handlers: HashMap<String, Arc<dyn ToolHandler>>,
        specs: Vec<ConfiguredTool>,
    ) -> Self {
        Self { handlers, specs }
    }

    pub fn specs(&self) -> Vec<ToolDescriptor> {
        self.specs.iter().map(|tool| tool.spec.clone()).collect()
    }

    pub fn supports_parallel_tool_calls(&self, tool_name: &str) -> bool {
        self.specs
            .iter()
            .find(|tool| tool.spec.name() == tool_name)
            .is_some_and(|tool| tool.supports_parallel_tool_calls)
    }

    pub fn any_parallel_tool_calls(&self) -> bool {
        self.specs
            .iter()
            .any(|tool| tool.supports_parallel_tool_calls)
    }

    pub async fn dispatch(
        &self,
        invocation: ToolInvocation,
        cancellation_token: CancellationToken,
    ) -> Result<ToolResultItem, ToolError> {
        let tool_name = invocation.tool_name.clone();
        let handler = self.handlers.get(tool_name.as_str()).ok_or_else(|| {
            ToolError::RespondToModel(unsupported_tool_call_message(
                &invocation.payload,
                tool_name.as_str(),
            ))
        })?;

        let output = handler
            .handle(invocation.clone(), cancellation_token)
            .await?;
        Ok(output.into_response(invocation.call_id.as_str(), &invocation.payload))
    }
}

#[derive(Default)]
pub struct ToolRegistryBuilder {
    handlers: HashMap<String, Arc<dyn ToolHandler>>,
    specs: Vec<ConfiguredTool>,
}

impl ToolRegistryBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_handler(&mut self, handler: Arc<dyn ToolHandler>) {
        let spec = handler.spec();
        let name = spec.name().to_string();
        self.specs.push(ConfiguredTool {
            spec,
            supports_parallel_tool_calls: handler.supports_parallel_tool_calls(),
        });
        self.handlers.insert(name, handler);
    }

    pub fn push_spec(&mut self, spec: ToolDescriptor) {
        self.specs.push(ConfiguredTool {
            spec,
            supports_parallel_tool_calls: false,
        });
    }

    pub fn build(self) -> ToolRegistry {
        ToolRegistry::new(self.handlers, self.specs)
    }
}

fn unsupported_tool_call_message(payload: &ToolPayload, tool_name: &str) -> String {
    match payload {
        ToolPayload::TextInput { .. } => format!("unsupported custom tool call: {tool_name}"),
        ToolPayload::JsonArguments { .. } => {
            format!("unsupported call: {tool_name}")
        }
    }
}
