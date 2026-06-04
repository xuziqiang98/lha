use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug, Default)]
pub(crate) struct StreamingPreference {
    prefer_http_streaming: Arc<AtomicBool>,
}

impl StreamingPreference {
    pub fn prefers_http_streaming(&self) -> bool {
        self.prefer_http_streaming.load(Ordering::Relaxed)
    }

    pub fn prefer_http_fallback(&self, realtime_streaming_enabled: bool) -> bool {
        realtime_streaming_enabled && !self.prefer_http_streaming.swap(true, Ordering::Relaxed)
    }
}
