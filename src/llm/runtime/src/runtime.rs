use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use lha_otel::OtelManager;
use reqwest::Client as HttpClient;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;

use crate::ModelInfo;
use crate::ReasoningEffort;
use crate::ReasoningSummary;
use crate::Result;
use crate::TranscriptItem;
use crate::Verbosity;
use crate::WebSearchMode;
use crate::client::LlmClient;
use crate::compatibility::ChatRoleCompatibilityHandle;
use crate::compatibility::ChatRoleCompatibilityState;
use crate::config::LlmRuntimeConfig;
use crate::error::Error;
use crate::prompt::Prompt;
use crate::prompt::ResponseStream;
use crate::provider::RuntimeEndpoint;
use crate::runtime_types::RuntimeCapabilities;
use crate::runtime_types::RuntimeMetadata;
use crate::semantic::RuntimeNotice;
use crate::semantic::RuntimeNoticeKind;
use crate::semantic::TurnEvent;
use crate::semantic::TurnEventStream;
use crate::semantic::TurnRequest;
use crate::semantic::adapt_response_stream;
use crate::transport::StreamingPreference;
use futures::StreamExt;

#[async_trait]
trait PromptConversationCompactor: Send + Sync {
    async fn compact_conversation_history(&self, input: &Prompt) -> Result<Vec<TranscriptItem>>;
}

#[async_trait]
trait PromptRuntime: PromptConversationCompactor + Send + Sync {
    fn new_session(&self) -> Box<dyn PromptRuntimeSession>;
    fn capabilities(&self) -> RuntimeCapabilities;
    fn metadata(&self) -> RuntimeMetadata;
    fn estimated_input_tokens(&self, input: &Prompt) -> Option<i64>;
    fn stream_retry_limit(&self) -> u64;
}

#[async_trait]
trait PromptRuntimeSession: Send {
    async fn run_turn(&mut self, input: &Prompt) -> Result<ResponseStream>;
    fn try_switch_fallback_transport(&mut self) -> bool;
}

#[async_trait]
pub trait SemanticConversationCompactor: Send + Sync {
    async fn compact_conversation_history(
        &self,
        input: &TurnRequest,
    ) -> Result<Vec<TranscriptItem>>;
}

#[async_trait]
pub trait SemanticRuntime: SemanticConversationCompactor + Send + Sync {
    fn new_session(&self) -> Box<dyn SemanticRuntimeSession>;
    fn capabilities(&self) -> RuntimeCapabilities;
    fn metadata(&self) -> RuntimeMetadata;
    fn estimated_input_tokens(&self, input: &TurnRequest) -> Option<i64>;
}

#[async_trait]
pub trait SemanticRuntimeSession: Send {
    async fn run_turn(&mut self, input: &TurnRequest) -> Result<TurnEventStream>;
}

pub struct RuntimeBuildSpec {
    pub endpoint_id: String,
    pub http_client: HttpClient,
    pub model_info: ModelInfo,
    pub otel_manager: OtelManager,
    pub endpoint: RuntimeEndpoint,
    pub effort: Option<ReasoningEffort>,
    pub summary: ReasoningSummary,
    pub session_id: String,
    pub origin_tag: Option<String>,
    pub show_raw_agent_reasoning: bool,
    pub model_verbosity: Option<Verbosity>,
    pub web_search_mode: Option<WebSearchMode>,
    pub experimental_beta_feature_keys: Vec<String>,
    pub sse_fixture_path: Option<String>,
}

pub trait RuntimeClientFactory: Send + Sync {
    fn build_client(&self, spec: RuntimeBuildSpec) -> Arc<dyn SemanticRuntime>;
}

#[derive(Clone, Default)]
pub struct DefaultPromptRuntimeFactory {
    streaming_preference: StreamingPreference,
    chat_role_compatibility: Arc<Mutex<HashMap<String, ChatRoleCompatibilityHandle>>>,
}

impl DefaultPromptRuntimeFactory {
    pub fn new() -> Self {
        Self::default()
    }

    fn build_prompt_runtime(&self, spec: RuntimeBuildSpec) -> Arc<dyn PromptRuntime> {
        let RuntimeBuildSpec {
            endpoint_id,
            http_client,
            model_info,
            otel_manager,
            endpoint,
            effort,
            summary,
            session_id,
            origin_tag,
            show_raw_agent_reasoning,
            model_verbosity,
            web_search_mode,
            experimental_beta_feature_keys,
            sse_fixture_path,
        } = spec;

        let capabilities = RuntimeCapabilities::from_endpoint_and_model(&endpoint, &model_info);
        let metadata = RuntimeMetadata {
            endpoint_name: endpoint.name.clone(),
            model: model_info.slug.clone(),
        };
        let runtime_config = Arc::new(LlmRuntimeConfig {
            model_provider_id: endpoint_id.clone(),
            show_raw_agent_reasoning,
            model_verbosity,
            web_search_mode,
            experimental_beta_feature_keys,
            sse_fixture_path,
        });
        let chat_role_compatibility = endpoint
            .uses_chat_completions_api()
            .then(|| self.chat_role_compatibility_handle(endpoint_id));
        let client = LlmClient::new(
            runtime_config,
            http_client,
            model_info,
            chat_role_compatibility,
            otel_manager,
            endpoint.clone(),
            effort,
            summary,
            session_id,
            origin_tag,
            self.streaming_preference.clone(),
        );

        Arc::new(DefaultPromptRuntime {
            client,
            capabilities,
            metadata,
            stream_retry_limit: endpoint.stream_max_retries(),
        })
    }

    fn chat_role_compatibility_handle(&self, endpoint_id: String) -> ChatRoleCompatibilityHandle {
        let mut state = self
            .chat_role_compatibility
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .entry(endpoint_id)
            .or_insert_with(|| Arc::new(Mutex::new(ChatRoleCompatibilityState::new())))
            .clone()
    }
}

impl RuntimeClientFactory for DefaultPromptRuntimeFactory {
    fn build_client(&self, spec: RuntimeBuildSpec) -> Arc<dyn SemanticRuntime> {
        adapt_prompt_runtime(self.build_prompt_runtime(spec))
    }
}

fn adapt_prompt_runtime(runtime: Arc<dyn PromptRuntime>) -> Arc<dyn SemanticRuntime> {
    Arc::new(PromptRuntimeAdapter { inner: runtime })
}

struct DefaultPromptRuntime {
    client: LlmClient,
    capabilities: RuntimeCapabilities,
    metadata: RuntimeMetadata,
    stream_retry_limit: u64,
}

#[async_trait]
impl PromptConversationCompactor for DefaultPromptRuntime {
    async fn compact_conversation_history(&self, input: &Prompt) -> Result<Vec<TranscriptItem>> {
        self.client.compact_conversation_history(input).await
    }
}

#[async_trait]
impl PromptRuntime for DefaultPromptRuntime {
    fn new_session(&self) -> Box<dyn PromptRuntimeSession> {
        Box::new(DefaultPromptRuntimeSession {
            inner: self.client.new_session(),
        })
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        self.capabilities.clone()
    }

    fn metadata(&self) -> RuntimeMetadata {
        self.metadata.clone()
    }

    fn estimated_input_tokens(&self, input: &Prompt) -> Option<i64> {
        self.client.estimated_input_tokens_for_prompt(input)
    }

    fn stream_retry_limit(&self) -> u64 {
        self.stream_retry_limit
    }
}

struct DefaultPromptRuntimeSession {
    inner: crate::client::LlmClientSession,
}

#[async_trait]
impl PromptRuntimeSession for DefaultPromptRuntimeSession {
    async fn run_turn(&mut self, input: &Prompt) -> Result<ResponseStream> {
        self.inner.stream(input).await
    }

    fn try_switch_fallback_transport(&mut self) -> bool {
        self.inner.try_switch_fallback_transport()
    }
}

struct PromptRuntimeAdapter {
    inner: Arc<dyn PromptRuntime>,
}

#[async_trait]
impl SemanticConversationCompactor for PromptRuntimeAdapter {
    async fn compact_conversation_history(
        &self,
        input: &TurnRequest,
    ) -> Result<Vec<TranscriptItem>> {
        self.inner
            .compact_conversation_history(&input.to_prompt())
            .await
    }
}

#[async_trait]
impl SemanticRuntime for PromptRuntimeAdapter {
    fn new_session(&self) -> Box<dyn SemanticRuntimeSession> {
        Box::new(PromptRuntimeSessionAdapter {
            inner: Arc::new(AsyncMutex::new(self.inner.new_session())),
            stream_retry_limit: self.inner.stream_retry_limit(),
        })
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        self.inner.capabilities()
    }

    fn metadata(&self) -> RuntimeMetadata {
        self.inner.metadata()
    }

    fn estimated_input_tokens(&self, input: &TurnRequest) -> Option<i64> {
        self.inner.estimated_input_tokens(&input.to_prompt())
    }
}

struct PromptRuntimeSessionAdapter {
    inner: Arc<AsyncMutex<Box<dyn PromptRuntimeSession>>>,
    stream_retry_limit: u64,
}

#[async_trait]
impl SemanticRuntimeSession for PromptRuntimeSessionAdapter {
    async fn run_turn(&mut self, input: &TurnRequest) -> Result<TurnEventStream> {
        let prompt = input.to_prompt();
        let inner = Arc::clone(&self.inner);
        let stream_retry_limit = self.stream_retry_limit;
        let (tx_event, rx_event) = mpsc::channel(1600);

        tokio::spawn(async move {
            let mut retries = 0u64;

            loop {
                let response_stream = {
                    let mut session = inner.lock().await;
                    session.run_turn(&prompt).await
                };

                let stream = match response_stream {
                    Ok(stream) => stream,
                    Err(err) => {
                        if handle_stream_failure(
                            &inner,
                            &tx_event,
                            stream_retry_limit,
                            &mut retries,
                            &err,
                        )
                        .await
                        {
                            continue;
                        }
                        let _ = tx_event.send(Err(err)).await;
                        return;
                    }
                };

                let mut stream = adapt_response_stream(stream);
                let mut completed = false;
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(event) => {
                            completed |= matches!(event, TurnEvent::Completed { .. });
                            if tx_event.send(Ok(event)).await.is_err() {
                                return;
                            }
                        }
                        Err(err) => {
                            if handle_stream_failure(
                                &inner,
                                &tx_event,
                                stream_retry_limit,
                                &mut retries,
                                &err,
                            )
                            .await
                            {
                                break;
                            }
                            let _ = tx_event.send(Err(err)).await;
                            return;
                        }
                    }
                }

                if completed {
                    return;
                }

                let err = Error::Stream("stream closed before response.completed".to_string());
                if handle_stream_failure(&inner, &tx_event, stream_retry_limit, &mut retries, &err)
                    .await
                {
                    continue;
                }
                let _ = tx_event.send(Err(err)).await;
                return;
            }
        });

        Ok(TurnEventStream { rx_event })
    }
}

async fn handle_stream_failure(
    inner: &Arc<AsyncMutex<Box<dyn PromptRuntimeSession>>>,
    tx_event: &mpsc::Sender<Result<TurnEvent>>,
    stream_retry_limit: u64,
    retries: &mut u64,
    err: &Error,
) -> bool {
    if !is_retryable_stream_error(err) {
        return false;
    }

    if *retries >= stream_retry_limit {
        let switched = {
            let mut session = inner.lock().await;
            session.try_switch_fallback_transport()
        };
        if switched {
            *retries = 0;
            let notice = TurnEvent::RuntimeNotice(RuntimeNotice {
                kind: RuntimeNoticeKind::TransportFallback,
                message: format!("Falling back from WebSockets to HTTPS transport. {err:#}"),
            });
            let _ = tx_event.send(Ok(notice)).await;
            return true;
        }
        return false;
    }

    *retries += 1;
    let delay = retry_delay(err, *retries);
    let notice = TurnEvent::RuntimeNotice(RuntimeNotice {
        kind: RuntimeNoticeKind::Reconnecting,
        message: format!("Reconnecting... {retries}/{stream_retry_limit}"),
    });
    let _ = tx_event.send(Ok(notice)).await;
    tokio::time::sleep(delay).await;
    true
}

fn retry_delay(err: &Error, retries: u64) -> Duration {
    match err {
        Error::Retryable {
            delay: Some(delay), ..
        } => *delay,
        _ => backoff(retries),
    }
}

fn backoff(retries: u64) -> Duration {
    let exponent = retries.saturating_sub(1).min(6);
    let millis = 200u64.saturating_mul(1u64 << exponent);
    Duration::from_millis(millis)
}

fn is_retryable_stream_error(err: &Error) -> bool {
    matches!(
        err,
        Error::Retryable { .. } | Error::Stream(_) | Error::RequestTimeout
    )
}
