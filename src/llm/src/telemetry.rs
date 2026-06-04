use std::sync::Arc;
use std::time::Duration;

pub trait RuntimeTelemetry: Send + Sync {
    fn record_api_request(
        &self,
        attempt: u64,
        status: Option<u16>,
        error: Option<&str>,
        duration: Duration,
    ) {
        let _ = (attempt, status, error, duration);
    }

    fn record_sse_event(
        &self,
        kind: Option<&str>,
        success: bool,
        error: Option<&str>,
        duration: Duration,
    ) {
        let _ = (kind, success, error, duration);
    }

    fn record_response_completed(
        &self,
        input_tokens: i64,
        output_tokens: i64,
        cached_input_tokens: Option<i64>,
        reasoning_output_tokens: Option<i64>,
        total_tokens: i64,
    ) {
        let _ = (
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_output_tokens,
            total_tokens,
        );
    }

    fn record_response_completed_failed(&self, error: &str) {
        let _ = error;
    }

    fn record_websocket_request(&self, duration: Duration, error: Option<&str>) {
        let _ = (duration, error);
    }

    fn record_websocket_event(
        &self,
        kind: Option<&str>,
        success: bool,
        error: Option<&str>,
        duration: Duration,
    ) {
        let _ = (kind, success, error, duration);
    }

    fn counter(&self, name: &str, inc: i64, tags: &[(&str, &str)]) {
        let _ = (name, inc, tags);
    }
}

#[derive(Debug, Default)]
pub struct NoopRuntimeTelemetry;

impl RuntimeTelemetry for NoopRuntimeTelemetry {}

pub fn noop_runtime_telemetry() -> Arc<dyn RuntimeTelemetry> {
    Arc::new(NoopRuntimeTelemetry)
}
