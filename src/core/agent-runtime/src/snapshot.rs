use crate::session::SessionId;
use crate::session::SubmissionId;
use crate::status::SessionStatus;
use codex_llm::RuntimeMetadata;
use codex_protocol::models::ConversationItem;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTurnSnapshot {
    pub submission_id: SubmissionId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub status: SessionStatus,
    pub conversation: Vec<ConversationItem>,
    pub steering_queue: Vec<Vec<ConversationItem>>,
    pub follow_up_queue: Vec<Vec<ConversationItem>>,
    pub runtime: RuntimeMetadata,
    pub active_turn: Option<ActiveTurnSnapshot>,
}
