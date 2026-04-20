//! Single `ApiError` enum covering every row of DESIGN §API error table.
//! `IntoResponse` produces `{error, message}` with the right status.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Debug, Clone)]
pub enum ApiError {
    BadRequest(String),
    Unauthorized(UnauthorizedReason),
    PayloadTooLarge,
    EmptyText,
    EmptyLangs,
    UnknownLanguage(String),
    InvalidMode,
    Overloaded,
    Internal,
}

#[derive(Debug, Clone, Copy)]
pub enum UnauthorizedReason {
    Missing,
    Invalid,
}

impl UnauthorizedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: &'a str,
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, "bad_request", msg.clone()),
            Self::Unauthorized(_) => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "authentication required".to_string(),
            ),
            Self::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "request body exceeds limit".to_string(),
            ),
            Self::EmptyText => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "empty_text",
                "text must be non-empty".to_string(),
            ),
            Self::EmptyLangs => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "empty_langs",
                "langs must be non-empty when provided".to_string(),
            ),
            Self::UnknownLanguage(code) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "unknown_language",
                format!("unknown language: {code}"),
            ),
            Self::InvalidMode => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_mode",
                "mode must be \"strict\" or \"substring\"".to_string(),
            ),
            Self::Overloaded => (
                StatusCode::SERVICE_UNAVAILABLE,
                "overloaded",
                "too many in-flight requests".to_string(),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal server error".to_string(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, msg) = self.parts();
        let body = ErrorBody {
            error: code,
            message: &msg,
        };
        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn body_json(resp: Response) -> (StatusCode, serde_json::Value) {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, v)
    }

    #[tokio::test]
    async fn internal_hides_detail() {
        let (status, body) = body_json(ApiError::Internal.into_response()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "internal");
        assert_eq!(body["message"], "internal server error");
    }

    #[tokio::test]
    async fn unknown_language_echoes_code() {
        let (status, body) =
            body_json(ApiError::UnknownLanguage("xx".to_string()).into_response()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "unknown_language");
        assert!(body["message"].as_str().unwrap().contains("xx"));
    }
}
