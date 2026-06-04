use crate::session::SessionId;
use crate::session::SubmissionId;
use crate::status::SessionStatus;
use lha_llm::RuntimeMetadata;
use lha_llm::TranscriptItem;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveTurnSnapshot {
    pub submission_id: SubmissionId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub status: SessionStatus,
    pub conversation: Vec<TranscriptItem>,
    pub steering_queue: Vec<Vec<TranscriptItem>>,
    pub follow_up_queue: Vec<Vec<TranscriptItem>>,
    pub runtime: RuntimeMetadata,
    pub active_turn: Option<ActiveTurnSnapshot>,
}
