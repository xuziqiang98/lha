use crate::error::Result;
pub use codex_llm::AgentEvent as ResponseEvent;
pub use codex_llm::AgentTurnInput as Prompt;
use futures::Stream;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use tokio::sync::mpsc;

/// Review thread system prompt. Edit `core/src/review_prompt.md` to customize.
pub const REVIEW_PROMPT: &str = include_str!("../review_prompt.md");

// Centralized templates for review-related user messages
pub const REVIEW_EXIT_SUCCESS_TMPL: &str = include_str!("../templates/review/exit_success.xml");
pub const REVIEW_EXIT_INTERRUPTED_TMPL: &str =
    include_str!("../templates/review/exit_interrupted.xml");

pub(crate) mod tools {
    pub use codex_llm::prompt::AdditionalProperties;
    pub use codex_llm::prompt::FreeformTool;
    pub use codex_llm::prompt::FreeformToolFormat;
    pub use codex_llm::prompt::JsonSchema;
    pub use codex_llm::prompt::ResponsesApiTool;
    pub use codex_llm::prompt::ToolSpec;
}

pub struct ResponseStream {
    pub(crate) rx_event: mpsc::Receiver<Result<ResponseEvent>>,
}

impl Stream for ResponseStream {
    type Item = Result<ResponseEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx_event.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_prompt_is_available() {
        assert!(!REVIEW_PROMPT.is_empty());
        assert!(!REVIEW_EXIT_SUCCESS_TMPL.is_empty());
        assert!(!REVIEW_EXIT_INTERRUPTED_TMPL.is_empty());
    }
}
