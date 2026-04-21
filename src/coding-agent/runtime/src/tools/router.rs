use crate::codex::Session;
use crate::codex::TurnContext;
use crate::function_tool::FunctionCallError;
use crate::sandboxing::SandboxPermissions;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ConfiguredToolSpec;
use crate::tools::registry::ToolRegistry;
use crate::tools::registry::unsupported_tool_call_message;
use crate::tools::spec::ToolsConfig;
use crate::tools::spec::build_specs;
use codex_llm::ToolCallPayload as LlmToolCallPayload;
use codex_llm::ToolCallRequest as LlmToolCallRequest;
use codex_llm::ToolDescriptor;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ShellToolCallParams;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::instrument;

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub tool_name: String,
    pub call_id: String,
    pub payload: ToolPayload,
}

pub struct ToolRouter {
    registry: ToolRegistry,
    specs: Vec<ConfiguredToolSpec>,
    enforce_declared_tool_names: bool,
}

impl ToolRouter {
    pub fn from_config(
        config: &ToolsConfig,
        mcp_tools: Option<HashMap<String, mcp_types::Tool>>,
        dynamic_tools: &[DynamicToolSpec],
    ) -> Self {
        let builder = build_specs(config, mcp_tools, dynamic_tools);
        let (specs, registry) = builder.build();

        Self {
            registry,
            specs,
            enforce_declared_tool_names: config.enforce_declared_tool_names,
        }
    }

    pub fn specs(&self) -> Vec<ToolDescriptor> {
        self.specs
            .iter()
            .map(|config| config.spec.clone())
            .collect()
    }

    pub fn tool_supports_parallel(&self, tool_name: &str) -> bool {
        self.specs
            .iter()
            .filter(|config| config.supports_parallel_tool_calls)
            .any(|config| config.spec.name() == tool_name)
    }

    fn tool_is_declared(&self, tool_name: &str) -> bool {
        self.specs
            .iter()
            .any(|config| config.spec.name() == tool_name)
    }

    #[instrument(level = "trace", skip_all, err)]
    pub async fn build_tool_call(
        session: &Session,
        request: LlmToolCallRequest,
    ) -> Result<ToolCall, FunctionCallError> {
        match request.payload {
            LlmToolCallPayload::Function { arguments } => {
                if request.tool_name == "local_shell" {
                    if request.call_id.is_empty() {
                        return Err(FunctionCallError::MissingLocalShellCallId);
                    }
                    let mut params: ShellToolCallParams = serde_json::from_str(&arguments)
                        .map_err(|err| {
                            FunctionCallError::Fatal(format!(
                                "failed to parse local_shell arguments: {err}"
                            ))
                        })?;
                    params.sandbox_permissions = params
                        .sandbox_permissions
                        .or(Some(SandboxPermissions::UseDefault));
                    return Ok(ToolCall {
                        tool_name: request.tool_name,
                        call_id: request.call_id,
                        payload: ToolPayload::LocalShell { params },
                    });
                }
                if let Some((server, tool)) = session.parse_mcp_tool_name(&request.tool_name).await
                {
                    Ok(ToolCall {
                        tool_name: request.tool_name,
                        call_id: request.call_id,
                        payload: ToolPayload::Mcp {
                            server,
                            tool,
                            raw_arguments: arguments,
                        },
                    })
                } else {
                    Ok(ToolCall {
                        tool_name: request.tool_name,
                        call_id: request.call_id,
                        payload: ToolPayload::Function { arguments },
                    })
                }
            }
            LlmToolCallPayload::Custom { input } => Ok(ToolCall {
                tool_name: request.tool_name,
                call_id: request.call_id,
                payload: ToolPayload::Custom { input },
            }),
        }
    }

    #[instrument(level = "trace", skip_all, err)]
    pub async fn dispatch_tool_call(
        &self,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        tracker: SharedTurnDiffTracker,
        call: ToolCall,
    ) -> Result<ResponseInputItem, FunctionCallError> {
        let ToolCall {
            tool_name,
            call_id,
            payload,
        } = call;
        let payload_outputs_custom = matches!(payload, ToolPayload::Custom { .. });
        let failure_call_id = call_id.clone();

        if self.enforce_declared_tool_names && !self.tool_is_declared(tool_name.as_str()) {
            let message = unsupported_tool_call_message(&payload, tool_name.as_str());
            return Ok(Self::failure_response(
                failure_call_id,
                payload_outputs_custom,
                FunctionCallError::RespondToModel(message),
            ));
        }

        let invocation = ToolInvocation {
            session,
            turn,
            tracker,
            call_id,
            tool_name,
            payload,
        };

        match self.registry.dispatch(invocation).await {
            Ok(response) => Ok(response),
            Err(FunctionCallError::Fatal(message)) => Err(FunctionCallError::Fatal(message)),
            Err(err) => Ok(Self::failure_response(
                failure_call_id,
                payload_outputs_custom,
                err,
            )),
        }
    }

    fn failure_response(
        call_id: String,
        payload_outputs_custom: bool,
        err: FunctionCallError,
    ) -> ResponseInputItem {
        let message = err.to_string();
        if payload_outputs_custom {
            ResponseInputItem::CustomToolCallOutput {
                call_id,
                output: message,
            }
        } else {
            ResponseInputItem::FunctionCallOutput {
                call_id,
                output: codex_protocol::models::FunctionCallOutputPayload {
                    content: message,
                    success: Some(false),
                    ..Default::default()
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex::make_session_and_context;
    use crate::tools::context::ToolOutput;
    use crate::tools::registry::ToolHandler;
    use crate::tools::registry::ToolKind;
    use crate::tools::spec::AdditionalProperties;
    use crate::tools::spec::JsonSchema;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use async_trait::async_trait;
    use codex_llm::FunctionToolDescriptor;
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use tokio::sync::Mutex;

    struct StubHandler {
        called: Arc<AtomicBool>,
        output: &'static str,
        accepts_custom_payload: bool,
    }

    #[async_trait]
    impl ToolHandler for StubHandler {
        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        fn matches_kind(&self, payload: &ToolPayload) -> bool {
            matches!(payload, ToolPayload::Function { .. })
                || (self.accepts_custom_payload && matches!(payload, ToolPayload::Custom { .. }))
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
        ) -> Result<ToolOutput, FunctionCallError> {
            self.called.store(true, Ordering::SeqCst);
            Ok(ToolOutput::Function {
                content: self.output.to_string(),
                content_items: None,
                success: Some(true),
            })
        }
    }

    fn function_spec(name: &str) -> ConfiguredToolSpec {
        ConfiguredToolSpec::new(
            ToolDescriptor::Function(FunctionToolDescriptor {
                name: name.to_string(),
                description: format!("tool {name}"),
                strict: false,
                parameters: JsonSchema::Object {
                    properties: BTreeMap::new(),
                    required: None,
                    additional_properties: Some(AdditionalProperties::Boolean(false)),
                },
            }),
            false,
        )
    }

    fn tracker() -> SharedTurnDiffTracker {
        Arc::new(Mutex::new(TurnDiffTracker::new()))
    }

    #[tokio::test]
    async fn messages_router_rejects_undeclared_apply_patch_before_handler() {
        let (session, turn) = make_session_and_context().await;
        let called = Arc::new(AtomicBool::new(false));
        let router = ToolRouter {
            registry: ToolRegistry::new(HashMap::from([(
                "apply_patch".to_string(),
                Arc::new(StubHandler {
                    called: Arc::clone(&called),
                    output: "should not run",
                    accepts_custom_payload: true,
                }) as Arc<dyn ToolHandler>,
            )])),
            specs: vec![function_spec("update_plan")],
            enforce_declared_tool_names: true,
        };

        let response = router
            .dispatch_tool_call(
                Arc::new(session),
                Arc::new(turn),
                tracker(),
                ToolCall {
                    tool_name: "apply_patch".to_string(),
                    call_id: "call-1".to_string(),
                    payload: ToolPayload::Custom {
                        input: "patch".to_string(),
                    },
                },
            )
            .await
            .expect("router should return tool failure output");

        assert!(!called.load(Ordering::SeqCst));
        assert_eq!(
            response,
            ResponseInputItem::CustomToolCallOutput {
                call_id: "call-1".to_string(),
                output: "unsupported custom tool call: apply_patch".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn messages_router_rejects_undeclared_shell_alias_before_handler() {
        let (session, turn) = make_session_and_context().await;
        let called = Arc::new(AtomicBool::new(false));
        let router = ToolRouter {
            registry: ToolRegistry::new(HashMap::from([(
                "container.exec".to_string(),
                Arc::new(StubHandler {
                    called: Arc::clone(&called),
                    output: "should not run",
                    accepts_custom_payload: false,
                }) as Arc<dyn ToolHandler>,
            )])),
            specs: vec![function_spec("update_plan")],
            enforce_declared_tool_names: true,
        };

        let response = router
            .dispatch_tool_call(
                Arc::new(session),
                Arc::new(turn),
                tracker(),
                ToolCall {
                    tool_name: "container.exec".to_string(),
                    call_id: "call-2".to_string(),
                    payload: ToolPayload::Function {
                        arguments: "{}".to_string(),
                    },
                },
            )
            .await
            .expect("router should return tool failure output");

        assert!(!called.load(Ordering::SeqCst));
        assert_eq!(
            response,
            ResponseInputItem::FunctionCallOutput {
                call_id: "call-2".to_string(),
                output: codex_protocol::models::FunctionCallOutputPayload {
                    content: "unsupported call: container.exec".to_string(),
                    success: Some(false),
                    ..Default::default()
                },
            }
        );
    }

    #[tokio::test]
    async fn responses_router_still_allows_undeclared_aliases() {
        let (session, turn) = make_session_and_context().await;
        let called = Arc::new(AtomicBool::new(false));
        let router = ToolRouter {
            registry: ToolRegistry::new(HashMap::from([(
                "container.exec".to_string(),
                Arc::new(StubHandler {
                    called: Arc::clone(&called),
                    output: "alias ok",
                    accepts_custom_payload: false,
                }) as Arc<dyn ToolHandler>,
            )])),
            specs: vec![function_spec("shell")],
            enforce_declared_tool_names: false,
        };

        let response = router
            .dispatch_tool_call(
                Arc::new(session),
                Arc::new(turn),
                tracker(),
                ToolCall {
                    tool_name: "container.exec".to_string(),
                    call_id: "call-3".to_string(),
                    payload: ToolPayload::Function {
                        arguments: "{}".to_string(),
                    },
                },
            )
            .await
            .expect("responses router should allow alias handler");

        assert!(called.load(Ordering::SeqCst));
        assert_eq!(
            response,
            ResponseInputItem::FunctionCallOutput {
                call_id: "call-3".to_string(),
                output: codex_protocol::models::FunctionCallOutputPayload {
                    content: "alias ok".to_string(),
                    success: Some(true),
                    ..Default::default()
                },
            }
        );
    }
}
