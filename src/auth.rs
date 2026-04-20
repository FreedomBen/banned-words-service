//! Bearer auth middleware. Constant-time equality against the full key set on
//! every request; fast-path rejection before body parse and before the M5
//! in-flight gate. See DESIGN §Authentication.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header::AUTHORIZATION, HeaderMap};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::{ApiError, UnauthorizedReason};
use crate::state::AppState;

pub async fn require_bearer(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    match check(&state.api_keys, req.headers()) {
        Ok(token) => {
            tracing::debug!(target: "auth", key_id = %key_id(token), "auth success");
            next.run(req).await
        }
        Err(reason) => {
            tracing::warn!(target: "auth", reason = %reason.as_str(), "auth failure");
            ApiError::Unauthorized(reason).into_response()
        }
    }
}

/// On success, returns the token bytes so the caller can compute and log a
/// key-id. The token reference is borrowed from the `HeaderMap`.
fn check<'h>(api_keys: &[Vec<u8>], headers: &'h HeaderMap) -> Result<&'h [u8], UnauthorizedReason> {
    let Some(raw) = headers.get(AUTHORIZATION) else {
        return Err(UnauthorizedReason::Missing);
    };
    let Ok(s) = raw.to_str() else {
        return Err(UnauthorizedReason::Invalid);
    };
    let Some(token) = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
    else {
        return Err(UnauthorizedReason::Invalid);
    };
    let token_bytes = token.as_bytes();
    // Iterate the full set to keep timing independent of which key matches.
    let mut matched = subtle::Choice::from(0u8);
    for k in api_keys {
        matched |= k.as_slice().ct_eq(token_bytes);
    }
    if bool::from(matched) {
        Ok(token_bytes)
    } else {
        Err(UnauthorizedReason::Invalid)
    }
}

fn key_id(token: &[u8]) -> String {
    let h = Sha256::digest(token);
    // First 4 bytes = 8 hex chars, per IMPLEMENTATION_PLAN M3 item 2.
    hex::encode(&h[..4])
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    fn keys() -> Vec<Vec<u8>> {
        vec![
            b"one-very-long-test-key-one-one-one".to_vec(),
            b"two-very-long-test-key-two-two-two".to_vec(),
        ]
    }

    fn headers_with(auth: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_str(auth).unwrap());
        h
    }

    #[test]
    fn missing_header_is_missing_reason() {
        let err = check(&keys(), &HeaderMap::new()).unwrap_err();
        assert!(matches!(err, UnauthorizedReason::Missing));
    }

    #[test]
    fn non_bearer_scheme_is_invalid() {
        let err = check(&keys(), &headers_with("Basic foo")).unwrap_err();
        assert!(matches!(err, UnauthorizedReason::Invalid));
    }

    #[test]
    fn wrong_key_is_invalid() {
        let err = check(&keys(), &headers_with("Bearer nope")).unwrap_err();
        assert!(matches!(err, UnauthorizedReason::Invalid));
    }

    #[test]
    fn matching_first_key_ok() {
        let h = headers_with("Bearer one-very-long-test-key-one-one-one");
        let token = check(&keys(), &h).expect("ok");
        assert_eq!(token, b"one-very-long-test-key-one-one-one");
    }

    #[test]
    fn matching_second_key_ok() {
        let h = headers_with("Bearer two-very-long-test-key-two-two-two");
        let _ = check(&keys(), &h).expect("ok");
    }

    #[test]
    fn lowercase_scheme_accepted() {
        let h = headers_with("bearer one-very-long-test-key-one-one-one");
        let _ = check(&keys(), &h).expect("ok");
    }

    #[test]
    fn key_id_is_8_hex_chars() {
        let id = key_id(b"anything");
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
