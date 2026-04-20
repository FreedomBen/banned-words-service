//! HTTP surface integration tests. Per IMPLEMENTATION_PLAN M3 item 9; each
//! test exercises the full middleware stack via `Router::oneshot` without
//! binding a real TCP listener.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use banned_words_service::build_router;
use banned_words_service::matcher::{Engine, Lang, LIST_VERSION};
use banned_words_service::state::AppState;

const TEST_KEY: &str = "test-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn test_state() -> Arc<AppState> {
    let mut langs: HashMap<Lang, &[&str]> = HashMap::new();
    langs.insert("en".into(), &["fuck", "shit"][..]);
    let engine = Arc::new(Engine::new(&langs));
    Arc::new(AppState {
        engine,
        api_keys: vec![TEST_KEY.as_bytes().to_vec()],
        list_version: LIST_VERSION,
        ready: AtomicBool::new(true),
        max_inflight: 1024,
    })
}

fn authed(method: &str, path: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TEST_KEY}"))
        .body(body)
        .unwrap()
}

async fn json_body(resp: Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("non-JSON body: {e}: bytes={bytes:?}"))
}

async fn send(req: Request<Body>) -> Response<Body> {
    build_router(test_state()).oneshot(req).await.unwrap()
}

#[tokio::test]
async fn auth_missing_returns_401_with_x_list_version() {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/check")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"hi"}"#))
        .unwrap();
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers()
            .get("x-list-version")
            .expect("x-list-version attached on fast-path 401")
            .to_str()
            .unwrap(),
        LIST_VERSION
    );
    let body = json_body(resp).await;
    assert_eq!(body["error"], "unauthorized");
}

#[tokio::test]
async fn auth_invalid_returns_401() {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/check")
        .header("content-type", "application/json")
        .header("authorization", "Bearer not-a-real-key")
        .body(Body::from(r#"{"text":"hi"}"#))
        .unwrap();
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn v1_languages_requires_auth() {
    let req = Request::builder()
        .method("GET")
        .uri("/v1/languages")
        .body(Body::empty())
        .unwrap();
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers()
            .get("x-list-version")
            .expect("x-list-version attached on /v1/languages fast-path 401")
            .to_str()
            .unwrap(),
        LIST_VERSION
    );
}

#[tokio::test]
async fn body_too_large_with_content_length_returns_413() {
    // Realistic client: Content-Length is present, layer rejects upfront.
    let text = "x".repeat(70 * 1024);
    let body = serde_json::to_vec(&serde_json::json!({ "text": text })).unwrap();
    let len = body.len();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/check")
        .header("content-type", "application/json")
        .header("content-length", len.to_string())
        .header("authorization", format!("Bearer {TEST_KEY}"))
        .body(Body::from(body))
        .unwrap();
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        resp.headers()
            .get("x-list-version")
            .expect("X-List-Version attached on remapped 413")
            .to_str()
            .unwrap(),
        LIST_VERSION
    );
    let v = json_body(resp).await;
    assert_eq!(v["error"], "payload_too_large");
}

#[tokio::test]
async fn body_too_large_chunked_returns_413() {
    // No Content-Length: the request body streams, the layer's Limited wrapper
    // errors mid-stream, and the handler's length-limit fallback classifies
    // it as payload_too_large so the contract stays uniform.
    let text = "x".repeat(70 * 1024);
    let body = serde_json::to_vec(&serde_json::json!({ "text": text })).unwrap();
    let req = authed("POST", "/v1/check", Body::from(body));
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let v = json_body(resp).await;
    assert_eq!(v["error"], "payload_too_large");
}

#[tokio::test]
async fn malformed_json_is_400_bad_request() {
    let req = authed("POST", "/v1/check", Body::from("not json"));
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = json_body(resp).await;
    assert_eq!(v["error"], "bad_request");
}

#[tokio::test]
async fn missing_text_field_is_400_bad_request() {
    let req = authed("POST", "/v1/check", Body::from(r#"{"langs":["en"]}"#));
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = json_body(resp).await;
    assert_eq!(v["error"], "bad_request");
}

#[tokio::test]
async fn empty_text_is_422_empty_text() {
    let req = authed("POST", "/v1/check", Body::from(r#"{"text":""}"#));
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let v = json_body(resp).await;
    assert_eq!(v["error"], "empty_text");
}

#[tokio::test]
async fn whitespace_only_text_accepted() {
    let req = authed("POST", "/v1/check", Body::from(r#"{"text":"   "}"#));
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn empty_langs_is_422_empty_langs() {
    let req = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"hi","langs":[]}"#),
    );
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let v = json_body(resp).await;
    assert_eq!(v["error"], "empty_langs");
}

#[tokio::test]
async fn unknown_language_is_422_unknown_language() {
    let req = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"hi","langs":["xx"]}"#),
    );
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let v = json_body(resp).await;
    assert_eq!(v["error"], "unknown_language");
}

#[tokio::test]
async fn langs_case_folded() {
    // "EN" must fold to "en" (loaded in test_state), reaching the happy path.
    let req = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"hi","langs":["EN"]}"#),
    );
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert!(v["mode_used"]["en"].is_string());
}

#[tokio::test]
async fn x_list_version_attached_on_success() {
    let req = authed("POST", "/v1/check", Body::from(r#"{"text":"hi"}"#));
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-list-version")
            .unwrap()
            .to_str()
            .unwrap(),
        LIST_VERSION
    );
}

#[tokio::test]
async fn languages_response_shape() {
    let req = authed("GET", "/v1/languages", Body::empty());
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let entries = v["languages"].as_array().expect("languages is array");
    assert!(entries
        .iter()
        .any(|e| e["code"] == "en" && e["default_mode"] == "strict"));
}

#[tokio::test]
async fn healthz_is_200_and_unauthenticated() {
    let req = Request::builder()
        .method("GET")
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // Unauthenticated endpoints do not carry X-List-Version.
    assert!(resp.headers().get("x-list-version").is_none());
}

#[tokio::test]
async fn readyz_200_with_full_body_shape() {
    let req = Request::builder()
        .method("GET")
        .uri("/readyz")
        .body(Body::empty())
        .unwrap();
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["ready"], true);
    assert_eq!(v["list_version"], LIST_VERSION);
    assert_eq!(v["languages"], 1);
}

#[tokio::test]
async fn happy_path_returns_match() {
    let req = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"holy shit!","langs":["en"],"mode":"strict"}"#),
    );
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["list_version"], LIST_VERSION);
    assert_eq!(v["mode_used"]["en"], "strict");
    let matches = v["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["term"], "shit");
    assert_eq!(matches[0]["matched_text"], "shit");
    assert_eq!(matches[0]["start"], 5);
    assert_eq!(matches[0]["end"], 9);
    assert_eq!(v["truncated"], false);
}

#[tokio::test]
async fn unknown_fields_silently_accepted() {
    // DESIGN invariant: serde's deny_unknown_fields is deliberately off, so
    // callers can pass extras (e.g. the reserved `overrides` key) without 400.
    let req = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"hi","overrides":{"foo":1},"novel":true}"#),
    );
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}
