use crate::api::TransportError;
use crate::api::error::ApiError;
use chrono::DateTime;
use chrono::Utc;
use http::HeaderMap;
use http::StatusCode;
use serde::Deserialize;
use std::time::Duration;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("stream disconnected before completion: {message}")]
    Retryable {
        message: String,
        delay: Option<Duration>,
    },
    #[error("stream disconnected before completion: {0}")]
    Stream(String),
    #[error("context window exceeded")]
    ContextWindowExceeded,
    #[error("quota exceeded")]
    QuotaExceeded,
    #[error("usage not included")]
    UsageNotIncluded,
    #[error("unexpected HTTP status {status}: {body}")]
    UnexpectedStatus {
        status: StatusCode,
        body: String,
        url: Option<String>,
        request_id: Option<String>,
    },
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
    #[error("invalid image request")]
    InvalidImageRequest,
    #[error("internal server error")]
    InternalServerError,
    #[error("exceeded retry limit, last status: {status}{request_id_suffix}")]
    RetryLimit {
        status: StatusCode,
        request_id: Option<String>,
        request_id_suffix: String,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("request timed out while waiting for the server response")]
    RequestTimeout,
    #[error("unsupported operation: {0}")]
    UnsupportedOperation(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("usage limit reached")]
    UsageLimitReached { resets_at: Option<DateTime<Utc>> },
    #[error("model capacity reached for {model}")]
    ModelCap {
        model: String,
        reset_after_seconds: Option<u64>,
    },
    #[error("missing environment variable {var}")]
    EnvVar {
        var: String,
        instructions: Option<String>,
    },
}

impl From<ApiError> for Error {
    fn from(err: ApiError) -> Self {
        match err {
            ApiError::ContextWindowExceeded => Self::ContextWindowExceeded,
            ApiError::QuotaExceeded => Self::QuotaExceeded,
            ApiError::UsageNotIncluded => Self::UsageNotIncluded,
            ApiError::Retryable { message, delay } => Self::Retryable { message, delay },
            ApiError::Stream(message) => Self::Stream(message),
            ApiError::Api { status, message } => Self::UnexpectedStatus {
                status,
                body: message,
                url: None,
                request_id: None,
            },
            ApiError::InvalidRequest { message } => Self::InvalidRequest { message },
            ApiError::Transport(transport) => map_transport_error(transport),
            ApiError::RateLimit(message) => Self::Stream(message),
        }
    }
}

const MODEL_CAP_MODEL_HEADER: &str = "x-codex-model-cap-model";
const MODEL_CAP_RESET_AFTER_HEADER: &str = "x-codex-model-cap-reset-after-seconds";

fn map_transport_error(err: TransportError) -> Error {
    match err {
        TransportError::Http {
            status,
            url,
            headers,
            body,
        } => {
            let body_text = body.unwrap_or_default();

            if status == StatusCode::BAD_REQUEST {
                if body_text
                    .contains("The image data you provided does not represent a valid image")
                {
                    Error::InvalidImageRequest
                } else {
                    Error::InvalidRequest { message: body_text }
                }
            } else if status == StatusCode::INTERNAL_SERVER_ERROR {
                Error::InternalServerError
            } else if status == StatusCode::TOO_MANY_REQUESTS {
                map_too_many_requests(status, headers.as_ref(), body_text)
            } else {
                Error::UnexpectedStatus {
                    status,
                    body: body_text,
                    url,
                    request_id: extract_request_id(headers.as_ref()),
                }
            }
        }
        TransportError::RetryLimit => Error::RetryLimit {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            request_id: None,
            request_id_suffix: String::new(),
        },
        TransportError::Timeout => Error::RequestTimeout,
        TransportError::Network(message) | TransportError::Build(message) => Error::Stream(message),
    }
}

fn map_too_many_requests(
    status: StatusCode,
    headers: Option<&HeaderMap>,
    body_text: String,
) -> Error {
    if let Some(model) = headers
        .and_then(|map| map.get(MODEL_CAP_MODEL_HEADER))
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
    {
        let reset_after_seconds = headers
            .and_then(|map| map.get(MODEL_CAP_RESET_AFTER_HEADER))
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        return Error::ModelCap {
            model,
            reset_after_seconds,
        };
    }

    if let Ok(err) = serde_json::from_str::<UsageErrorResponse>(&body_text) {
        if err.error.error_type.as_deref() == Some("usage_limit_reached") {
            let resets_at = err
                .error
                .resets_at
                .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0));
            return Error::UsageLimitReached { resets_at };
        }
        if err.error.error_type.as_deref() == Some("usage_not_included") {
            return Error::UsageNotIncluded;
        }
    }

    let request_id = extract_request_id(headers);
    let request_id_suffix = request_id
        .as_ref()
        .map(|id| format!(", request id: {id}"))
        .unwrap_or_default();
    Error::RetryLimit {
        status,
        request_id,
        request_id_suffix,
    }
}

fn extract_request_id(headers: Option<&HeaderMap>) -> Option<String> {
    headers.and_then(|map| {
        ["cf-ray", "x-request-id", "x-oai-request-id"]
            .iter()
            .find_map(|name| {
                map.get(*name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
            })
    })
}

#[derive(Debug, Deserialize)]
struct UsageErrorResponse {
    error: UsageErrorBody,
}

#[derive(Debug, Deserialize)]
struct UsageErrorBody {
    #[serde(rename = "type")]
    error_type: Option<String>,
    resets_at: Option<i64>,
}
