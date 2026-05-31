use lha_llm::Error as LlmError;

use crate::error::CodexErr;
use crate::error::ModelCapError;
use crate::error::RetryLimitReachedError;
use crate::error::UnexpectedResponseError;
use crate::error::UsageLimitReachedError;

impl From<LlmError> for CodexErr {
    fn from(err: LlmError) -> Self {
        match err {
            LlmError::Retryable { message, delay } => CodexErr::Stream(message, delay),
            LlmError::Stream(message) => CodexErr::Stream(message, None),
            LlmError::ContextWindowExceeded => CodexErr::ContextWindowExceeded,
            LlmError::QuotaExceeded => CodexErr::QuotaExceeded,
            LlmError::UsageNotIncluded => CodexErr::UsageNotIncluded,
            LlmError::UnexpectedStatus {
                status,
                body,
                url,
                request_id,
            } => CodexErr::UnexpectedStatus(UnexpectedResponseError {
                status,
                body,
                url,
                request_id,
            }),
            LlmError::InvalidRequest { message } => CodexErr::InvalidRequest(message),
            LlmError::InvalidImageRequest => CodexErr::InvalidImageRequest(),
            LlmError::InternalServerError => CodexErr::InternalServerError,
            LlmError::RetryLimit {
                status,
                request_id,
                request_id_suffix: _,
            } => CodexErr::RetryLimit(RetryLimitReachedError { status, request_id }),
            LlmError::Json(err) => CodexErr::Json(err),
            LlmError::RequestTimeout => CodexErr::RequestTimeout,
            LlmError::UnsupportedOperation(message) => CodexErr::UnsupportedOperation(message),
            LlmError::Io(err) => CodexErr::Io(err),
            LlmError::UsageLimitReached { resets_at } => {
                CodexErr::UsageLimitReached(UsageLimitReachedError { resets_at })
            }
            LlmError::ModelCap {
                model,
                reset_after_seconds,
            } => CodexErr::ModelCap(ModelCapError {
                model,
                reset_after_seconds,
            }),
            LlmError::EnvVar { var, instructions } => {
                CodexErr::EnvVar(crate::error::EnvVarError { var, instructions })
            }
        }
    }
}
