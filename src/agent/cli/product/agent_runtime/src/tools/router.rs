use crate::product::agent::codex::Session;
use crate::product::agent::codex::TurnContext;
use crate::product::agent::function_tool::FunctionCallError;
use crate::product::agent::sandboxing::SandboxPermissions;
use crate::product::agent::tools::context::SharedTurnDiffTracker;
use crate::product::agent::tools::context::ToolInvocation;
use crate::product::agent::tools::context::ToolPayload;
use crate::product::agent::tools::registry::ConfiguredToolSpec;
use crate::product::agent::tools::registry::ToolHandler;
use crate::product::agent::tools::registry::ToolRegistry;
use crate::product::agent::tools::registry::unsupported_tool_call_message;
use crate::product::agent::tools::spec::ToolsConfig;
use crate::product::agent::tools::spec::build_specs;
use crate::product::protocol::dynamic_tools::DynamicToolSpec;
use crate::product::protocol::models::ShellToolCallParams;
use lha_llm::ToolCallPayload as LlmToolCallPayload;
use lha_llm::ToolCallRequest as LlmToolCallRequest;
use lha_llm::ToolDescriptor;
use lha_llm::ToolResultItem;
use lha_llm::ToolResultPayload;
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
        mcp_tools: Option<HashMap<String, crate::product::mcp_types::Tool>>,
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

    pub(crate) fn register_extra_tool(
        &mut self,
        spec: ToolDescriptor,
        supports_parallel_tool_calls: bool,
        handler_name: impl Into<String>,
        handler: Arc<dyn ToolHandler>,
    ) {
        self.specs
            .push(ConfiguredToolSpec::new(spec, supports_parallel_tool_calls));
        self.registry.register(handler_name, handler);
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
            LlmToolCallPayload::JsonArguments { arguments } => {
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
            LlmToolCallPayload::TextInput { input } => Ok(ToolCall {
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
    ) -> Result<ToolResultItem, FunctionCallError> {
        let ToolCall {
            tool_name,
            call_id,
            payload,
        } = call;
        let payload_outputs_custom = matches!(payload, ToolPayload::Custom { .. });
        let failure_call_id = call_id.clone();
        let failure_tool_name = tool_name.clone();

        if self.enforce_declared_tool_names && !self.tool_is_declared(tool_name.as_str()) {
            let message = unsupported_tool_call_message(&payload, tool_name.as_str());
            return Ok(Self::failure_response(
                failure_call_id,
                failure_tool_name,
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
                failure_tool_name,
                payload_outputs_custom,
                err,
            )),
        }
    }

    fn failure_response(
        call_id: String,
        tool_name: String,
        payload_outputs_custom: bool,
        err: FunctionCallError,
    ) -> ToolResultItem {
        let message = err.to_string();
        if payload_outputs_custom {
            ToolResultItem {
                call_id,
                tool_name,
                payload: ToolResultPayload::Text { output: message },
            }
        } else {
            ToolResultItem {
                call_id,
                tool_name,
                payload: ToolResultPayload::Structured {
                    content: message,
                    content_items: None,
                    success: Some(false),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::codex::make_session_and_context;
    use crate::product::agent::tools::context::ToolOutput;
    use crate::product::agent::tools::registry::ToolHandler;
    use crate::product::agent::tools::registry::ToolKind;
    use crate::product::agent::tools::spec::AdditionalProperties;
    use crate::product::agent::tools::spec::JsonSchema;
    use crate::product::agent::turn_diff_tracker::TurnDiffTracker;
    use async_trait::async_trait;
    use lha_llm::FunctionToolDescriptor;
    use lha_llm::ToolResultItem;
    use lha_llm::ToolResultPayload;
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
            ToolResultItem {
                call_id: "call-1".to_string(),
                tool_name: "apply_patch".to_string(),
                payload: ToolResultPayload::Text {
                    output: "unsupported custom tool call: apply_patch".to_string(),
                },
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
            ToolResultItem {
                call_id: "call-2".to_string(),
                tool_name: "container.exec".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "unsupported call: container.exec".to_string(),
                    content_items: None,
                    success: Some(false),
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
            ToolResultItem {
                call_id: "call-3".to_string(),
                tool_name: "container.exec".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "alias ok".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            }
        );
    }

    #[tokio::test]
    async fn register_extra_tool_adds_spec_and_handler_for_declared_runtime() {
        let (session, turn) = make_session_and_context().await;
        let called = Arc::new(AtomicBool::new(false));
        let mut router = ToolRouter {
            registry: ToolRegistry::new(HashMap::new()),
            specs: vec![function_spec("update_plan")],
            enforce_declared_tool_names: true,
        };

        router.register_extra_tool(
            function_spec("lha_input_retrieve").spec,
            true,
            "lha_input_retrieve",
            Arc::new(StubHandler {
                called: Arc::clone(&called),
                output: "retrieved",
                accepts_custom_payload: false,
            }),
        );

        assert!(
            router
                .specs()
                .iter()
                .any(|spec| spec.name() == "lha_input_retrieve")
        );
        assert!(router.tool_supports_parallel("lha_input_retrieve"));

        let response = router
            .dispatch_tool_call(
                Arc::new(session),
                Arc::new(turn),
                tracker(),
                ToolCall {
                    tool_name: "lha_input_retrieve".to_string(),
                    call_id: "call-4".to_string(),
                    payload: ToolPayload::Function {
                        arguments: "{}".to_string(),
                    },
                },
            )
            .await
            .expect("extra tool should dispatch");

        assert!(called.load(Ordering::SeqCst));
        assert_eq!(
            response,
            ToolResultItem {
                call_id: "call-4".to_string(),
                tool_name: "lha_input_retrieve".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "retrieved".to_string(),
                    content_items: None,
                    success: Some(true),
                },
            }
        );
    }

    #[tokio::test]
    async fn declared_runtime_rejects_input_retrieve_without_registration() {
        let (session, turn) = make_session_and_context().await;
        let called = Arc::new(AtomicBool::new(false));
        let router = ToolRouter {
            registry: ToolRegistry::new(HashMap::from([(
                "lha_input_retrieve".to_string(),
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
                    tool_name: "lha_input_retrieve".to_string(),
                    call_id: "call-5".to_string(),
                    payload: ToolPayload::Function {
                        arguments: r#"{"hash":"abc"}"#.to_string(),
                    },
                },
            )
            .await
            .expect("router should return tool failure output");

        assert!(!called.load(Ordering::SeqCst));
        assert_eq!(
            response,
            ToolResultItem {
                call_id: "call-5".to_string(),
                tool_name: "lha_input_retrieve".to_string(),
                payload: ToolResultPayload::Structured {
                    content: "unsupported call: lha_input_retrieve".to_string(),
                    content_items: None,
                    success: Some(false),
                },
            }
        );
    }
}
