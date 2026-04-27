use adam_llm::Error as LlmError;

use crate::error::CodexErr;
use crate::error::ModelCapError;
use crate::error::RefreshTokenFailedError;
use crate::error::RefreshTokenFailedReason;
use crate::error::RetryLimitReachedError;
use crate::error::UnexpectedResponseError;
use crate::error::UsageLimitReachedError;
use crate::token_data::KnownPlan;
use crate::token_data::PlanType;

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
            LlmError::UnauthorizedRecoveryFailed { reason, message } => {
                CodexErr::RefreshTokenFailed(RefreshTokenFailedError::new(
                    match reason {
                        adam_llm::UnauthorizedRecoveryFailedReason::Expired => {
                            RefreshTokenFailedReason::Expired
                        }
                        adam_llm::UnauthorizedRecoveryFailedReason::Exhausted => {
                            RefreshTokenFailedReason::Exhausted
                        }
                        adam_llm::UnauthorizedRecoveryFailedReason::Revoked => {
                            RefreshTokenFailedReason::Revoked
                        }
                        adam_llm::UnauthorizedRecoveryFailedReason::Other => {
                            RefreshTokenFailedReason::Other
                        }
                    },
                    message,
                ))
            }
            LlmError::UsageLimitReached {
                plan_type,
                resets_at,
                rate_limits,
                promo_message,
            } => CodexErr::UsageLimitReached(UsageLimitReachedError {
                plan_type: plan_type.map(map_plan_type),
                resets_at,
                rate_limits,
                promo_message,
            }),
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

fn map_plan_type(plan_type: adam_protocol::account::PlanType) -> PlanType {
    match plan_type {
        adam_protocol::account::PlanType::Free => PlanType::Known(KnownPlan::Free),
        adam_protocol::account::PlanType::Go => PlanType::Known(KnownPlan::Go),
        adam_protocol::account::PlanType::Plus => PlanType::Known(KnownPlan::Plus),
        adam_protocol::account::PlanType::Pro => PlanType::Known(KnownPlan::Pro),
        adam_protocol::account::PlanType::Team => PlanType::Known(KnownPlan::Team),
        adam_protocol::account::PlanType::Business => PlanType::Known(KnownPlan::Business),
        adam_protocol::account::PlanType::Enterprise => PlanType::Known(KnownPlan::Enterprise),
        adam_protocol::account::PlanType::Edu => PlanType::Known(KnownPlan::Edu),
        adam_protocol::account::PlanType::Unknown => PlanType::Unknown("unknown".to_string()),
    }
}
