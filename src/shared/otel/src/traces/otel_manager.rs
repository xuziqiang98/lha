use crate::metrics::names::API_CALL_COUNT_METRIC;
use crate::metrics::names::API_CALL_DURATION_METRIC;
use crate::metrics::names::SSE_EVENT_COUNT_METRIC;
use crate::metrics::names::SSE_EVENT_DURATION_METRIC;
use crate::metrics::names::TOOL_CALL_COUNT_METRIC;
use crate::metrics::names::TOOL_CALL_DURATION_METRIC;
use crate::metrics::names::WEBSOCKET_EVENT_COUNT_METRIC;
use crate::metrics::names::WEBSOCKET_EVENT_DURATION_METRIC;
use crate::metrics::names::WEBSOCKET_REQUEST_COUNT_METRIC;
use crate::metrics::names::WEBSOCKET_REQUEST_DURATION_METRIC;
use crate::otel_provider::traceparent_context_from_env;
use chrono::SecondsFormat;
use chrono::Utc;
use eventsource_stream::Event as StreamEvent;
use eventsource_stream::EventStreamError as StreamError;
use lha_llm::api::ApiError;
use lha_llm::api::ResponseEvent;
use lha_llm::types::TranscriptItem;
use lha_protocol::ThreadId;
use lha_protocol::config_types::ReasoningSummary;
use lha_protocol::openai_models::ReasoningEffort;
use lha_protocol::protocol::AskForApproval;
use lha_protocol::protocol::ReviewDecision;
use lha_protocol::protocol::SandboxPolicy;
use lha_protocol::protocol::SessionSource;
use lha_protocol::user_input::UserInput;
use reqwest::Error;
use reqwest::Response;
use std::borrow::Cow;
use std::fmt::Display;
use std::future::Future;
use std::time::Duration;
use std::time::Instant;
use tokio::time::error::Elapsed;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub use crate::OtelEventMetadata;
pub use crate::OtelManager;
pub use crate::ToolDecisionSource;

const SSE_UNKNOWN_KIND: &str = "unknown";
const WEBSOCKET_UNKNOWN_KIND: &str = "unknown";

impl OtelManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conversation_id: ThreadId,
        model: &str,
        slug: &str,
        account_id: Option<String>,
        account_email: Option<String>,
        auth_mode: Option<String>,
        log_user_prompts: bool,
        terminal_type: String,
        session_source: SessionSource,
    ) -> OtelManager {
        Self {
            metadata: OtelEventMetadata {
                conversation_id,
                auth_mode,
                account_id,
                account_email,
                session_source: session_source.to_string(),
                model: model.to_owned(),
                slug: slug.to_owned(),
                log_user_prompts,
                app_version: env!("CARGO_PKG_VERSION"),
                terminal_type,
            },
            metrics: crate::metrics::global(),
            metrics_use_metadata_tags: true,
        }
    }

    pub fn apply_traceparent_parent(&self, span: &Span) {
        if let Some(context) = traceparent_context_from_env() {
            let _ = span.set_parent(context);
        }
    }

    pub fn record_responses(&self, handle_responses_span: &Span, event: &ResponseEvent) {
        handle_responses_span.record("otel.name", OtelManager::responses_type(event));

        match event {
            ResponseEvent::OutputItemDone(item) => {
                handle_responses_span.record("from", "output_item_done");
                if let TranscriptItem::ToolCall { tool_name, .. } = &item {
                    handle_responses_span.record("tool_name", tool_name.as_str());
                }
            }
            ResponseEvent::OutputItemAdded(item) => {
                handle_responses_span.record("from", "output_item_added");
                if let TranscriptItem::ToolCall { tool_name, .. } = &item {
                    handle_responses_span.record("tool_name", tool_name.as_str());
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn conversation_starts(
        &self,
        provider_name: &str,
        reasoning_effort: Option<ReasoningEffort>,
        reasoning_summary: ReasoningSummary,
        context_window: Option<i64>,
        auto_compact_token_limit: Option<i64>,
        approval_policy: AskForApproval,
        sandbox_policy: SandboxPolicy,
        mcp_servers: Vec<&str>,
        active_profile: Option<String>,
    ) {
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.conversation_starts",
            event.timestamp = %timestamp(),
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            provider_name = %provider_name,
            reasoning_effort = reasoning_effort.map(|e| e.to_string()),
            reasoning_summary = %reasoning_summary,
            context_window = context_window,
            auto_compact_token_limit = auto_compact_token_limit,
            approval_policy = %approval_policy,
            sandbox_policy = %sandbox_policy,
            mcp_servers = mcp_servers.join(", "),
            active_profile = active_profile,
        )
    }

    pub async fn log_request<F, Fut>(&self, attempt: u64, f: F) -> Result<Response, Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Response, Error>>,
    {
        let start = Instant::now();
        let response = f().await;
        let duration = start.elapsed();

        let (status, error) = match &response {
            Ok(response) => (Some(response.status().as_u16()), None),
            Err(error) => (error.status().map(|s| s.as_u16()), Some(error.to_string())),
        };
        self.record_api_request(attempt, status, error.as_deref(), duration);

        response
    }

    pub fn record_api_request(
        &self,
        attempt: u64,
        status: Option<u16>,
        error: Option<&str>,
        duration: Duration,
    ) {
        let success = status.is_some_and(|code| (200..=299).contains(&code)) && error.is_none();
        let success_str = if success { "true" } else { "false" };
        let status_str = status
            .map(|code| code.to_string())
            .unwrap_or_else(|| "none".to_string());
        self.counter(
            API_CALL_COUNT_METRIC,
            1,
            &[("status", status_str.as_str()), ("success", success_str)],
        );
        self.record_duration(
            API_CALL_DURATION_METRIC,
            duration,
            &[("status", status_str.as_str()), ("success", success_str)],
        );
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.api_request",
            event.timestamp = %timestamp(),
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            duration_ms = %duration.as_millis(),
            http.response.status_code = status,
            error.message = error,
            attempt = attempt,
        );
    }

    pub fn record_websocket_request(&self, duration: Duration, error: Option<&str>) {
        let success_str = if error.is_none() { "true" } else { "false" };
        self.counter(
            WEBSOCKET_REQUEST_COUNT_METRIC,
            1,
            &[("success", success_str)],
        );
        self.record_duration(
            WEBSOCKET_REQUEST_DURATION_METRIC,
            duration,
            &[("success", success_str)],
        );
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.websocket_request",
            event.timestamp = %timestamp(),
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            duration_ms = %duration.as_millis(),
            success = success_str,
            error.message = error,
        );
    }

    pub fn record_websocket_event(
        &self,
        result: &Result<
            Option<
                Result<
                    tokio_tungstenite::tungstenite::Message,
                    tokio_tungstenite::tungstenite::Error,
                >,
            >,
            ApiError,
        >,
        duration: Duration,
    ) {
        let mut kind = None;
        let mut error_message = None;
        let mut success = true;

        match result {
            Ok(Some(Ok(message))) => match message {
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    match serde_json::from_str::<serde_json::Value>(text) {
                        Ok(value) => {
                            kind = value
                                .get("type")
                                .and_then(|value| value.as_str())
                                .map(std::string::ToString::to_string);
                            if kind.as_deref() == Some("response.failed") {
                                success = false;
                                error_message = value
                                    .get("response")
                                    .and_then(|value| value.get("error"))
                                    .map(serde_json::Value::to_string)
                                    .or_else(|| Some("response.failed event received".to_string()));
                            }
                        }
                        Err(err) => {
                            kind = Some("parse_error".to_string());
                            error_message = Some(err.to_string());
                            success = false;
                        }
                    }
                }
                tokio_tungstenite::tungstenite::Message::Binary(_) => {
                    success = false;
                    error_message = Some("unexpected binary websocket event".to_string());
                }
                tokio_tungstenite::tungstenite::Message::Ping(_)
                | tokio_tungstenite::tungstenite::Message::Pong(_) => {
                    return;
                }
                tokio_tungstenite::tungstenite::Message::Close(_) => {
                    success = false;
                    error_message =
                        Some("websocket closed by server before response.completed".to_string());
                }
                tokio_tungstenite::tungstenite::Message::Frame(_) => {
                    success = false;
                    error_message = Some("unexpected websocket frame".to_string());
                }
            },
            Ok(Some(Err(err))) => {
                success = false;
                error_message = Some(err.to_string());
            }
            Ok(None) => {
                success = false;
                error_message = Some("stream closed before response.completed".to_string());
            }
            Err(err) => {
                success = false;
                error_message = Some(err.to_string());
            }
        }

        let kind_str = kind.as_deref().unwrap_or(WEBSOCKET_UNKNOWN_KIND);
        let success_str = if success { "true" } else { "false" };
        let tags = [("kind", kind_str), ("success", success_str)];
        self.counter(WEBSOCKET_EVENT_COUNT_METRIC, 1, &tags);
        self.record_duration(WEBSOCKET_EVENT_DURATION_METRIC, duration, &tags);
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.websocket_event",
            event.timestamp = %timestamp(),
            event.kind = %kind_str,
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            duration_ms = %duration.as_millis(),
            success = success_str,
            error.message = error_message.as_deref(),
        );
    }

    pub fn log_sse_event<E>(
        &self,
        response: &Result<Option<Result<StreamEvent, StreamError<E>>>, Elapsed>,
        duration: Duration,
    ) where
        E: Display,
    {
        match response {
            Ok(Some(Ok(sse))) => {
                if sse.data.trim() == "[DONE]" {
                    self.sse_event(&sse.event, duration);
                } else {
                    match serde_json::from_str::<serde_json::Value>(&sse.data) {
                        Ok(error) if sse.event == "response.failed" => {
                            self.sse_event_failed(Some(&sse.event), duration, &error);
                        }
                        Ok(_) if sse.event == "response.output_item.done" => {
                            self.sse_event(&sse.event, duration);
                        }
                        Ok(_) => {
                            self.sse_event(&sse.event, duration);
                        }
                        Err(error) => {
                            self.sse_event_failed(Some(&sse.event), duration, &error);
                        }
                    }
                }
            }
            Ok(Some(Err(error))) => {
                self.sse_event_failed(None, duration, error);
            }
            Ok(None) => {}
            Err(_) => {
                self.sse_event_failed(None, duration, &"idle timeout waiting for SSE");
            }
        }
    }

    fn sse_event(&self, kind: &str, duration: Duration) {
        self.counter(
            SSE_EVENT_COUNT_METRIC,
            1,
            &[("kind", kind), ("success", "true")],
        );
        self.record_duration(
            SSE_EVENT_DURATION_METRIC,
            duration,
            &[("kind", kind), ("success", "true")],
        );
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.sse_event",
            event.timestamp = %timestamp(),
            event.kind = %kind,
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            duration_ms = %duration.as_millis(),
        );
    }

    pub fn sse_event_failed<T>(&self, kind: Option<&String>, duration: Duration, error: &T)
    where
        T: Display,
    {
        let kind_str = kind.map_or(SSE_UNKNOWN_KIND, String::as_str);
        self.counter(
            SSE_EVENT_COUNT_METRIC,
            1,
            &[("kind", kind_str), ("success", "false")],
        );
        self.record_duration(
            SSE_EVENT_DURATION_METRIC,
            duration,
            &[("kind", kind_str), ("success", "false")],
        );
        match kind {
            Some(kind) => tracing::event!(
                tracing::Level::INFO,
                event.name = "codex.sse_event",
                event.timestamp = %timestamp(),
                event.kind = %kind,
                conversation.id = %self.metadata.conversation_id,
                app.version = %self.metadata.app_version,
                auth_mode = self.metadata.auth_mode,
                user.account_id = self.metadata.account_id,
                user.email = self.metadata.account_email,
                terminal.type = %self.metadata.terminal_type,
                model = %self.metadata.model,
                slug = %self.metadata.slug,
                duration_ms = %duration.as_millis(),
                error.message = %error,
            ),
            None => tracing::event!(
                tracing::Level::INFO,
                event.name = "codex.sse_event",
                event.timestamp = %timestamp(),
                conversation.id = %self.metadata.conversation_id,
                app.version = %self.metadata.app_version,
                auth_mode = self.metadata.auth_mode,
                user.account_id = self.metadata.account_id,
                user.email = self.metadata.account_email,
                terminal.type = %self.metadata.terminal_type,
                model = %self.metadata.model,
                slug = %self.metadata.slug,
                duration_ms = %duration.as_millis(),
                error.message = %error,
            ),
        }
    }

    pub fn see_event_completed_failed<T>(&self, error: &T)
    where
        T: Display,
    {
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.sse_event",
            event.kind = %"response.completed",
            event.timestamp = %timestamp(),
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            error.message = %error,
        )
    }

    pub fn sse_event_completed(
        &self,
        input_token_count: i64,
        output_token_count: i64,
        cached_token_count: Option<i64>,
        reasoning_token_count: Option<i64>,
        tool_token_count: i64,
    ) {
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.sse_event",
            event.timestamp = %timestamp(),
            event.kind = %"response.completed",
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            input_token_count = %input_token_count,
            output_token_count = %output_token_count,
            cached_token_count = cached_token_count,
            reasoning_token_count = reasoning_token_count,
            tool_token_count = %tool_token_count,
        );
    }

    pub fn user_prompt(&self, items: &[UserInput]) {
        let prompt = items
            .iter()
            .flat_map(|item| match item {
                UserInput::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();

        let prompt_to_log = if self.metadata.log_user_prompts {
            prompt.as_str()
        } else {
            "[REDACTED]"
        };

        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.user_prompt",
            event.timestamp = %timestamp(),
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            prompt_length = %prompt.chars().count(),
            prompt = %prompt_to_log,
        );
    }

    pub fn tool_decision(
        &self,
        tool_name: &str,
        call_id: &str,
        decision: &ReviewDecision,
        source: ToolDecisionSource,
    ) {
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.tool_decision",
            event.timestamp = %timestamp(),
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            tool_name = %tool_name,
            call_id = %call_id,
            decision = %decision.clone().to_string().to_lowercase(),
            source = %source.to_string(),
        );
    }

    pub async fn log_tool_result<F, Fut, E>(
        &self,
        tool_name: &str,
        call_id: &str,
        arguments: &str,
        f: F,
    ) -> Result<(String, bool), E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(String, bool), E>>,
        E: Display,
    {
        let start = Instant::now();
        let result = f().await;
        let duration = start.elapsed();

        let (output, success) = match &result {
            Ok((preview, success)) => (Cow::Borrowed(preview.as_str()), *success),
            Err(error) => (Cow::Owned(error.to_string()), false),
        };

        self.tool_result(
            tool_name,
            call_id,
            arguments,
            duration,
            success,
            output.as_ref(),
        );

        result
    }

    pub fn log_tool_failed(&self, tool_name: &str, error: &str) {
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.tool_result",
            event.timestamp = %timestamp(),
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            tool_name = %tool_name,
            duration_ms = %Duration::ZERO.as_millis(),
            success = %false,
            output = %error,
        );
    }

    pub fn tool_result(
        &self,
        tool_name: &str,
        call_id: &str,
        arguments: &str,
        duration: Duration,
        success: bool,
        output: &str,
    ) {
        let success_str = if success { "true" } else { "false" };
        self.counter(
            TOOL_CALL_COUNT_METRIC,
            1,
            &[("tool", tool_name), ("success", success_str)],
        );
        self.record_duration(
            TOOL_CALL_DURATION_METRIC,
            duration,
            &[("tool", tool_name), ("success", success_str)],
        );
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.tool_result",
            event.timestamp = %timestamp(),
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            tool_name = %tool_name,
            call_id = %call_id,
            arguments = %arguments,
            duration_ms = %duration.as_millis(),
            success = %success_str,
            output = %output,
        );
    }

    fn responses_type(event: &ResponseEvent) -> String {
        match event {
            ResponseEvent::Created => "created".into(),
            ResponseEvent::OutputItemDone(item) => OtelManager::responses_item_type(item),
            ResponseEvent::OutputItemAdded(item) => OtelManager::responses_item_type(item),
            ResponseEvent::Completed { .. } => "completed".into(),
            ResponseEvent::OutputTextDelta(_) => "text_delta".into(),
            ResponseEvent::ProposedPlanDelta(_) => "proposed_plan_delta".into(),
            ResponseEvent::ProposedPlanDone(_) => "proposed_plan_done".into(),
            ResponseEvent::ReasoningSummaryDelta { .. } => "reasoning_summary_delta".into(),
            ResponseEvent::ReasoningContentDelta { .. } => "reasoning_content_delta".into(),
            ResponseEvent::ReasoningSummaryPartAdded { .. } => {
                "reasoning_summary_part_added".into()
            }
            ResponseEvent::ServerReasoningIncluded(_) => "server_reasoning_included".into(),
            ResponseEvent::ModelsEtag(_) => "models_etag".into(),
        }
    }

    fn responses_item_type(item: &TranscriptItem) -> String {
        match item {
            TranscriptItem::Message { role, .. } => format!("message_from_{role}"),
            TranscriptItem::Reasoning { .. } => "reasoning".into(),
            TranscriptItem::HostedActivity { activity_type, .. } => {
                if activity_type == "web_search" {
                    "web_search_call".into()
                } else {
                    format!("hosted_activity_{activity_type}")
                }
            }
            TranscriptItem::ToolCall { payload, .. } => match payload {
                lha_llm::types::ToolCallPayload::JsonArguments { .. } => "function_call".into(),
                lha_llm::types::ToolCallPayload::TextInput { .. } => "custom_tool_call".into(),
            },
            TranscriptItem::ToolResult { payload, .. } => match payload {
                lha_llm::types::ToolResultPayload::Structured { .. } => {
                    "function_call_output".into()
                }
                lha_llm::types::ToolResultPayload::Text { .. } => "custom_tool_call_output".into(),
            },
            TranscriptItem::Unknown { .. } => "unknown".into(),
        }
    }
}

impl lha_llm::RuntimeTelemetry for OtelManager {
    fn record_api_request(
        &self,
        attempt: u64,
        status: Option<u16>,
        error: Option<&str>,
        duration: Duration,
    ) {
        OtelManager::record_api_request(self, attempt, status, error, duration);
    }

    fn record_sse_event(
        &self,
        kind: Option<&str>,
        success: bool,
        error: Option<&str>,
        duration: Duration,
    ) {
        if success {
            self.sse_event(kind.unwrap_or(SSE_UNKNOWN_KIND), duration);
            return;
        }

        let kind = kind.map(str::to_string);
        self.sse_event_failed(
            kind.as_ref(),
            duration,
            &error.unwrap_or("unknown SSE error"),
        );
    }

    fn record_response_completed(
        &self,
        input_tokens: i64,
        output_tokens: i64,
        cached_input_tokens: Option<i64>,
        reasoning_output_tokens: Option<i64>,
        total_tokens: i64,
    ) {
        self.sse_event_completed(
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_output_tokens,
            total_tokens,
        );
    }

    fn record_response_completed_failed(&self, error: &str) {
        self.see_event_completed_failed(&error);
    }

    fn record_websocket_request(&self, duration: Duration, error: Option<&str>) {
        OtelManager::record_websocket_request(self, duration, error);
    }

    fn record_websocket_event(
        &self,
        kind: Option<&str>,
        success: bool,
        error: Option<&str>,
        duration: Duration,
    ) {
        let kind = kind.unwrap_or(WEBSOCKET_UNKNOWN_KIND);
        let success = if success { "true" } else { "false" };
        let tags = [("kind", kind), ("success", success)];
        self.counter(WEBSOCKET_EVENT_COUNT_METRIC, 1, &tags);
        self.record_duration(WEBSOCKET_EVENT_DURATION_METRIC, duration, &tags);
        tracing::event!(
            tracing::Level::INFO,
            event.name = "codex.websocket_event",
            event.timestamp = %timestamp(),
            event.kind = %kind,
            conversation.id = %self.metadata.conversation_id,
            app.version = %self.metadata.app_version,
            auth_mode = self.metadata.auth_mode,
            user.account_id = self.metadata.account_id,
            user.email = self.metadata.account_email,
            terminal.type = %self.metadata.terminal_type,
            model = %self.metadata.model,
            slug = %self.metadata.slug,
            duration_ms = %duration.as_millis(),
            success = success,
            error.message = error,
        );
    }

    fn counter(&self, name: &str, inc: i64, tags: &[(&str, &str)]) {
        OtelManager::counter(self, name, inc, tags);
    }
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
