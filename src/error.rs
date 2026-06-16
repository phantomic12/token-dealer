//! Router error types. `AppError` is what handlers turn into HTTP responses.
//! Internal code uses `anyhow::Error` for ergonomics; the public surface
//! uses `AppError` so the type→status mapping is explicit.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("provider {provider} returned {status}: {message}")]
    ProviderError {
        provider: String,
        status: u16,
        message: String,
    },

    #[error("upstream context too long for {model} (limit {limit})")]
    ContextTooLong { model: String, limit: u32 },

    #[error("all providers failed for tier {tier}")]
    AllProvidersFailed { tier: String },

    #[error("upstream timeout after {ms}ms")]
    UpstreamTimeout { ms: u64 },

    #[error("internal: {0}")]
    Internal(String),
}

impl AppError {
    pub fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::ProviderError { status, .. } => {
                StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
            }
            AppError::ContextTooLong { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            AppError::AllProvidersFailed { .. } => StatusCode::BAD_GATEWAY,
            AppError::UpstreamTimeout { .. } => StatusCode::GATEWAY_TIMEOUT,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        // Mirror OpenAI's error envelope so existing clients can parse it.
        let body = Json(json!({
            "error": {
                "message": self.to_string(),
                "type": match status {
                    StatusCode::BAD_REQUEST => "invalid_request_error",
                    StatusCode::UNAUTHORIZED => "invalid_api_key",
                    StatusCode::PAYLOAD_TOO_LARGE => "context_length_exceeded",
                    StatusCode::GATEWAY_TIMEOUT => "timeout",
                    _ => "server_error",
                },
                "code": status.as_u16(),
            }
        }));
        (status, body).into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            AppError::UpstreamTimeout { ms: 0 }
        } else {
            AppError::Internal(format!("reqwest: {e}"))
        }
    }
}

pub type AppResult<T> = std::result::Result<T, AppError>;
