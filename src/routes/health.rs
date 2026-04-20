//! /healthz and /readyz. Unauthenticated per DESIGN §Authentication.
//!
//! The listener binds only *after* `AppState::ready` flips to `true`, so the
//! 503 path is essentially unobservable in production; it exists for
//! correctness (sidecar races) and is covered by handler-level unit tests
//! here rather than via oneshot integration.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::model::ReadyResponse;
use crate::state::AppState;

pub async fn healthz() -> StatusCode {
    StatusCode::OK
}

pub async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    if state.ready.load(Ordering::Acquire) {
        let body = ReadyResponse {
            ready: true,
            list_version: Some(state.list_version),
            languages: Some(state.engine.languages().count()),
        };
        (StatusCode::OK, Json(body)).into_response()
    } else {
        let body = ReadyResponse {
            ready: false,
            list_version: None,
            languages: None,
        };
        (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::{Engine, Lang, LIST_VERSION};
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;

    fn state_with_ready(ready: bool) -> Arc<AppState> {
        let mut langs: HashMap<Lang, &[&str]> = HashMap::new();
        langs.insert("en".into(), &["test"][..]);
        let engine = Arc::new(Engine::new(&langs));
        Arc::new(AppState {
            engine,
            api_keys: vec![b"unused".to_vec()],
            list_version: LIST_VERSION,
            ready: AtomicBool::new(ready),
            max_inflight: 1024,
        })
    }

    #[tokio::test]
    async fn readyz_503_when_not_ready() {
        let resp = readyz(State(state_with_ready(false))).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v, serde_json::json!({ "ready": false }));
    }

    #[tokio::test]
    async fn readyz_200_with_full_shape_when_ready() {
        let resp = readyz(State(state_with_ready(true))).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["ready"], true);
        assert_eq!(v["list_version"], LIST_VERSION);
        assert_eq!(v["languages"], 1);
    }
}
