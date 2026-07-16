use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client as HttpClient;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;

use crate::ModelInfo;
use crate::ReasoningEffort;
use crate::ReasoningSummary;
use crate::ResponseDelivery;
use crate::Result;
use crate::TranscriptItem;
use crate::Verbosity;
use crate::WebSearchMode;
use crate::compatibility::ChatRoleCompatibilityHandle;
use crate::compatibility::ChatRoleCompatibilityState;
use crate::config::LlmRuntimeConfig;
use crate::error::Error;
use crate::prompt::Prompt;
use crate::prompt::ResponseStream;
use crate::provider::RuntimeEndpoint;
use crate::runtime_client::LlmClient;
use crate::runtime_types::RuntimeCapabilities;
use crate::runtime_types::RuntimeMetadata;
use crate::semantic::RuntimeNotice;
use crate::semantic::RuntimeNoticeKind;
use crate::semantic::TurnEvent;
use crate::semantic::TurnEventStream;
use crate::semantic::TurnRequest;
use crate::semantic::adapt_response_stream;
use crate::telemetry::RuntimeTelemetry;
use crate::telemetry::noop_runtime_telemetry;
use crate::transport::StreamingPreference;
use futures::StreamExt;

static NEXT_DEFAULT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

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

    async fn run_turn_with_delivery(
        &mut self,
        input: &Prompt,
        delivery: ResponseDelivery,
    ) -> Result<ResponseStream> {
        match delivery {
            ResponseDelivery::Streaming => self.run_turn(input).await,
            ResponseDelivery::NonStreaming => Err(Error::UnsupportedOperation(
                "non-streaming response delivery is not supported by this runtime".to_string(),
            )),
        }
    }

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

    async fn run_turn_with_delivery(
        &mut self,
        input: &TurnRequest,
        delivery: ResponseDelivery,
    ) -> Result<TurnEventStream> {
        match delivery {
            ResponseDelivery::Streaming => self.run_turn(input).await,
            ResponseDelivery::NonStreaming => Err(Error::UnsupportedOperation(
                "non-streaming response delivery is not supported by this runtime".to_string(),
            )),
        }
    }
}

pub struct RuntimeBuildSpec {
    pub endpoint_id: String,
    pub http_client: HttpClient,
    pub model_info: ModelInfo,
    pub telemetry: Arc<dyn RuntimeTelemetry>,
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

pub struct RuntimeBuildSpecBuilder {
    spec: RuntimeBuildSpec,
}

impl RuntimeBuildSpec {
    pub fn builder(endpoint: RuntimeEndpoint, model_info: ModelInfo) -> RuntimeBuildSpecBuilder {
        let endpoint_id = endpoint.name.clone();
        RuntimeBuildSpecBuilder {
            spec: RuntimeBuildSpec {
                endpoint_id,
                http_client: HttpClient::new(),
                model_info,
                telemetry: noop_runtime_telemetry(),
                endpoint,
                effort: None,
                summary: ReasoningSummary::Auto,
                session_id: format!(
                    "lha-sdk-session-{}",
                    NEXT_DEFAULT_SESSION_ID.fetch_add(1, Ordering::SeqCst)
                ),
                origin_tag: None,
                show_raw_agent_reasoning: false,
                model_verbosity: None,
                web_search_mode: None,
                experimental_beta_feature_keys: Vec::new(),
                sse_fixture_path: None,
            },
        }
    }

    pub fn builder_from_lha_env(name: impl Into<String>) -> Result<RuntimeBuildSpecBuilder> {
        Self::builder_from_lha_env_with_lookup(name, |var| std::env::var(var).ok())
    }

    pub(crate) fn builder_from_lha_env_with_lookup(
        name: impl Into<String>,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<RuntimeBuildSpecBuilder> {
        let endpoint = RuntimeEndpoint::from_lha_env_with_lookup(name, &lookup)?;
        let model_info = ModelInfo::minimal_from_lha_env_with_lookup(lookup)?;
        Ok(Self::builder(endpoint, model_info))
    }
}

impl RuntimeBuildSpecBuilder {
    pub fn endpoint_id(mut self, endpoint_id: impl Into<String>) -> Self {
        self.spec.endpoint_id = endpoint_id.into();
        self
    }

    pub fn http_client(mut self, http_client: HttpClient) -> Self {
        self.spec.http_client = http_client;
        self
    }

    pub fn telemetry(mut self, telemetry: Arc<dyn RuntimeTelemetry>) -> Self {
        self.spec.telemetry = telemetry;
        self
    }

    pub fn effort(mut self, effort: Option<ReasoningEffort>) -> Self {
        self.spec.effort = effort;
        self
    }

    pub fn summary(mut self, summary: ReasoningSummary) -> Self {
        self.spec.summary = summary;
        self
    }

    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.spec.session_id = session_id.into();
        self
    }

    pub fn origin_tag(mut self, origin_tag: impl Into<String>) -> Self {
        self.spec.origin_tag = Some(origin_tag.into());
        self
    }

    pub fn show_raw_agent_reasoning(mut self, show_raw_agent_reasoning: bool) -> Self {
        self.spec.show_raw_agent_reasoning = show_raw_agent_reasoning;
        self
    }

    pub fn model_verbosity(mut self, model_verbosity: Option<Verbosity>) -> Self {
        self.spec.model_verbosity = model_verbosity;
        self
    }

    pub fn web_search_mode(mut self, web_search_mode: Option<WebSearchMode>) -> Self {
        self.spec.web_search_mode = web_search_mode;
        self
    }

    pub fn experimental_beta_feature_keys(mut self, keys: Vec<String>) -> Self {
        self.spec.experimental_beta_feature_keys = keys;
        self
    }

    pub fn sse_fixture_path(mut self, path: impl Into<String>) -> Self {
        self.spec.sse_fixture_path = Some(path.into());
        self
    }

    pub fn build(self) -> RuntimeBuildSpec {
        self.spec
    }
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
            telemetry,
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
            telemetry,
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
    inner: crate::runtime_client::LlmClientSession,
}

#[async_trait]
impl PromptRuntimeSession for DefaultPromptRuntimeSession {
    async fn run_turn(&mut self, input: &Prompt) -> Result<ResponseStream> {
        self.inner.stream(input).await
    }

    async fn run_turn_with_delivery(
        &mut self,
        input: &Prompt,
        delivery: ResponseDelivery,
    ) -> Result<ResponseStream> {
        self.inner.run_with_delivery(input, delivery).await
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
        self.run_turn_with_delivery(input, ResponseDelivery::Streaming)
            .await
    }

    async fn run_turn_with_delivery(
        &mut self,
        input: &TurnRequest,
        delivery: ResponseDelivery,
    ) -> Result<TurnEventStream> {
        let prompt = input.to_prompt();
        let inner = Arc::clone(&self.inner);
        let stream_retry_limit = self.stream_retry_limit;
        let (tx_event, rx_event) = mpsc::channel(1600);

        tokio::spawn(async move {
            let mut retries = 0u64;

            loop {
                let response_stream = {
                    let mut session = inner.lock().await;
                    session.run_turn_with_delivery(&prompt, delivery).await
                };

                let stream = match response_stream {
                    Ok(stream) => stream,
                    Err(err) => {
                        if delivery == ResponseDelivery::Streaming
                            && handle_stream_failure(
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
                            if delivery == ResponseDelivery::Streaming
                                && handle_stream_failure(
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
                if delivery == ResponseDelivery::Streaming
                    && handle_stream_failure(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::LHA_API_KEY_ENV_VAR;
    use crate::env::LHA_BASE_URL_ENV_VAR;
    use crate::env::LHA_MODEL_ENV_VAR;
    use pretty_assertions::assert_eq;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars = vars
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<HashMap<_, _>>();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn runtime_build_spec_builder_sets_expected_defaults() {
        let endpoint = RuntimeEndpoint::openai_compatible_chat("sdk", "https://api.example.com/v1")
            .with_env_key(Some("TEST_API_KEY".to_string()));
        let model_info = ModelInfo::minimal("test-model");
        let spec = RuntimeBuildSpec::builder(endpoint.clone(), model_info.clone()).build();

        assert_eq!(spec.endpoint_id, "sdk");
        assert_eq!(spec.model_info, model_info);
        assert_eq!(spec.endpoint, endpoint);
        assert_eq!(spec.effort, None);
        assert_eq!(spec.summary, ReasoningSummary::Auto);
        assert!(spec.session_id.starts_with("lha-sdk-session-"));
        assert_eq!(spec.origin_tag, None);
        assert!(!spec.show_raw_agent_reasoning);
        assert_eq!(spec.model_verbosity, None);
        assert_eq!(spec.web_search_mode, None);
        assert_eq!(spec.experimental_beta_feature_keys, Vec::<String>::new());
        assert_eq!(spec.sse_fixture_path, None);
    }

    #[test]
    fn runtime_build_spec_builder_applies_overrides() {
        let endpoint = RuntimeEndpoint::openai_compatible_chat("sdk", "https://api.example.com/v1");
        let spec = RuntimeBuildSpec::builder(endpoint, ModelInfo::minimal("test-model"))
            .endpoint_id("custom")
            .effort(Some(ReasoningEffort::High))
            .summary(ReasoningSummary::Detailed)
            .session_id("session-123")
            .origin_tag("example")
            .show_raw_agent_reasoning(true)
            .model_verbosity(Some(Verbosity::High))
            .web_search_mode(Some(WebSearchMode::Live))
            .experimental_beta_feature_keys(vec!["beta".to_string()])
            .sse_fixture_path("fixture.sse")
            .build();

        assert_eq!(spec.endpoint_id, "custom");
        assert_eq!(spec.effort, Some(ReasoningEffort::High));
        assert_eq!(spec.summary, ReasoningSummary::Detailed);
        assert_eq!(spec.session_id, "session-123");
        assert_eq!(spec.origin_tag, Some("example".to_string()));
        assert!(spec.show_raw_agent_reasoning);
        assert_eq!(spec.model_verbosity, Some(Verbosity::High));
        assert_eq!(spec.web_search_mode, Some(WebSearchMode::Live));
        assert_eq!(
            spec.experimental_beta_feature_keys,
            vec!["beta".to_string()]
        );
        assert_eq!(spec.sse_fixture_path, Some("fixture.sse".to_string()));
    }

    #[test]
    fn runtime_build_spec_builder_from_lha_env_uses_endpoint_and_model() {
        let spec = RuntimeBuildSpec::builder_from_lha_env_with_lookup(
            "sdk",
            lookup(&[
                (LHA_BASE_URL_ENV_VAR, "https://api.example.com/v1/responses"),
                (LHA_API_KEY_ENV_VAR, "secret"),
                (LHA_MODEL_ENV_VAR, "test-model"),
            ]),
        )
        .expect("builder should be created")
        .build();

        assert_eq!(spec.endpoint_id, "sdk");
        assert!(spec.endpoint.uses_responses_api());
        assert_eq!(
            spec.endpoint.base_url.as_deref(),
            Some("https://api.example.com/v1")
        );
        assert_eq!(spec.endpoint.env_key.as_deref(), Some(LHA_API_KEY_ENV_VAR));
        assert_eq!(spec.model_info, ModelInfo::minimal("test-model"));
    }

    struct StreamingOnlySession;

    #[async_trait::async_trait]
    impl SemanticRuntimeSession for StreamingOnlySession {
        async fn run_turn(&mut self, _input: &TurnRequest) -> Result<TurnEventStream> {
            panic!("streaming run_turn should not be called for a non-streaming request");
        }
    }

    #[tokio::test]
    async fn custom_runtime_defaults_non_streaming_delivery_to_unsupported() {
        let mut session = StreamingOnlySession;
        let result = session
            .run_turn_with_delivery(&TurnRequest::default(), ResponseDelivery::NonStreaming)
            .await;
        let Err(err) = result else {
            panic!("custom runtime should reject non-streaming delivery by default");
        };

        assert_eq!(
            err.to_string(),
            "unsupported operation: non-streaming response delivery is not supported by this runtime"
        );
    }
}
