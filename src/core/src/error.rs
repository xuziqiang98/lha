use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("session is already running")]
    SessionBusy,
    #[error("cannot continue: session history is empty")]
    EmptyConversation,
    #[error("cannot continue: last conversation item is assistant output")]
    InvalidContinuation,
    #[error("turn stream closed before response.completed")]
    StreamClosed,
    #[error("event channel closed")]
    EventChannelClosed,
    #[error("turn aborted")]
    Aborted,
    #[error("turn failed: {0}")]
    TurnFailed(String),
    #[error("runtime error: {0}")]
    Runtime(#[from] lha_llm::Error),
    #[error(transparent)]
    Skill(#[from] crate::skills::SkillError),
    #[error("tool error: {0}")]
    Tool(#[from] crate::tools::ToolError),
}

pub type Result<T> = std::result::Result<T, Error>;
