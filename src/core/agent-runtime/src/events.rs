use crate::input::InputQueue;
use crate::session::SessionId;
use crate::session::SubmissionId;
use codex_llm::ItemHandle;
use codex_llm::RateLimitSnapshot;
use codex_llm::RuntimeNotice;
use codex_llm::SemanticOutputItem;
use codex_llm::ToolCallRequest;
use codex_llm::ToolResultItem;
use codex_llm::TranscriptItem;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnItemDelta {
    OutputText { delta: String },
    ProposedPlan { delta: String },
    ReasoningSummary { delta: String, summary_index: i64 },
    ReasoningContent { delta: String, content_index: i64 },
    ReasoningSummaryPartAdded { summary_index: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSummary {
    pub needs_follow_up: bool,
    pub last_agent_message: Option<String>,
    pub response_total_tokens: Option<i64>,
    pub tool_output_tokens: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    SessionStarted {
        session_id: SessionId,
    },
    SessionStatusChanged {
        session_id: SessionId,
        status: crate::SessionStatus,
    },
    InputQueued {
        session_id: SessionId,
        queue: InputQueue,
        items: Vec<TranscriptItem>,
    },
    TurnStarted {
        session_id: SessionId,
        submission_id: SubmissionId,
    },
    RuntimeNotice {
        session_id: SessionId,
        notice: RuntimeNotice,
    },
    OutputItemStarted {
        session_id: SessionId,
        submission_id: SubmissionId,
        handle: ItemHandle,
        item: SemanticOutputItem,
    },
    OutputItemDelta {
        session_id: SessionId,
        submission_id: SubmissionId,
        handle: ItemHandle,
        delta: TurnItemDelta,
    },
    OutputItemCompleted {
        session_id: SessionId,
        submission_id: SubmissionId,
        handle: ItemHandle,
        item: SemanticOutputItem,
    },
    ToolCallRequested {
        session_id: SessionId,
        submission_id: SubmissionId,
        call: ToolCallRequest,
    },
    ToolCallCompleted {
        session_id: SessionId,
        submission_id: SubmissionId,
        response: ToolResultItem,
    },
    RateLimitsUpdated {
        session_id: SessionId,
        snapshot: RateLimitSnapshot,
    },
    ServerReasoningIncluded {
        session_id: SessionId,
        included: bool,
    },
    ModelsEtagUpdated {
        session_id: SessionId,
        etag: String,
    },
    TurnCompleted {
        session_id: SessionId,
        submission_id: SubmissionId,
        outcome: TurnSummary,
    },
    TurnFailed {
        session_id: SessionId,
        submission_id: SubmissionId,
        error: String,
    },
    TurnAborted {
        session_id: SessionId,
        submission_id: SubmissionId,
    },
}
