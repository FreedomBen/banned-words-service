//! HTTP surface integration tests. Per IMPLEMENTATION_PLAN M3 item 9; each
//! test exercises the full middleware stack via `Router::oneshot` without
//! binding a real TCP listener.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
        inflight: Arc::new(AtomicUsize::new(0)),
    })
}

/// Three-language fixture for M4 tests. Synthetic per-lang terms chosen to be
/// disjoint so a match unambiguously attributes to one language.
fn multi_lang_state() -> Arc<AppState> {
    let mut langs: HashMap<Lang, &[&str]> = HashMap::new();
    langs.insert("en".into(), &["foo"][..]);
    langs.insert("ja".into(), &["バカ"][..]);
    langs.insert("zh".into(), &["笨蛋"][..]);
    let engine = Arc::new(Engine::new(&langs));
    Arc::new(AppState {
        engine,
        api_keys: vec![TEST_KEY.as_bytes().to_vec()],
        list_version: LIST_VERSION,
        ready: AtomicBool::new(true),
        max_inflight: 1024,
        inflight: Arc::new(AtomicUsize::new(0)),
    })
}

/// Pre-saturated fixture for the M5 503 `overloaded` test. `inflight` is
/// already at `max_inflight`, so the very next `/v1/check` admission trips
/// the gate without needing a second concurrent request in flight.
fn saturated_state() -> Arc<AppState> {
    let mut langs: HashMap<Lang, &[&str]> = HashMap::new();
    langs.insert("en".into(), &["foo"][..]);
    let engine = Arc::new(Engine::new(&langs));
    let max = 4usize;
    Arc::new(AppState {
        engine,
        api_keys: vec![TEST_KEY.as_bytes().to_vec()],
        list_version: LIST_VERSION,
        ready: AtomicBool::new(true),
        max_inflight: max,
        inflight: Arc::new(AtomicUsize::new(max)),
    })
}

async fn send_with(state: Arc<AppState>, req: Request<Body>) -> Response<Body> {
    build_router(state).oneshot(req).await.unwrap()
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

// ---------------------------------------------------------------------------
// M4 — multi-language and mode defaults
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multi_lang_request_returns_mode_used_per_lang() {
    let state = multi_lang_state();
    let req = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"foo バカ 笨蛋","langs":["en","ja","zh"]}"#),
    );
    let resp = send_with(state, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    // Defaults: en=strict, ja=substring, zh=substring.
    assert_eq!(v["mode_used"]["en"], "strict");
    assert_eq!(v["mode_used"]["ja"], "substring");
    assert_eq!(v["mode_used"]["zh"], "substring");
    let matches = v["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 3);
    let langs_in_matches: Vec<&str> = matches
        .iter()
        .map(|m| m["lang"].as_str().unwrap())
        .collect();
    assert!(langs_in_matches.contains(&"en"));
    assert!(langs_in_matches.contains(&"ja"));
    assert!(langs_in_matches.contains(&"zh"));
}

#[tokio::test]
async fn default_vs_explicit_mode_parity_latin_strict() {
    // For en, default mode == explicit "strict"; response shape identical for both.
    let req_default = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"hello foo","langs":["en"]}"#),
    );
    let req_explicit = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"hello foo","langs":["en"],"mode":"strict"}"#),
    );
    let v_default = json_body(send_with(multi_lang_state(), req_default).await).await;
    let v_explicit = json_body(send_with(multi_lang_state(), req_explicit).await).await;
    assert_eq!(v_default["mode_used"], v_explicit["mode_used"]);
    assert_eq!(v_default["matches"], v_explicit["matches"]);
}

#[tokio::test]
async fn default_mode_is_substring_for_cjk() {
    let req = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"笨蛋","langs":["zh"]}"#),
    );
    let resp = send_with(multi_lang_state(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["mode_used"]["zh"], "substring");
    assert_eq!(v["matches"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn explicit_strict_on_cjk_not_clamped() {
    // Caller-supplied strict on zh wins; mode_used echoes strict without clamping.
    let req = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"笨蛋","langs":["zh"],"mode":"strict"}"#),
    );
    let resp = send_with(multi_lang_state(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert_eq!(v["mode_used"]["zh"], "strict");
}

#[tokio::test]
async fn omitted_langs_scans_every_loaded_language() {
    let req = authed("POST", "/v1/check", Body::from(r#"{"text":"foo"}"#));
    let resp = send_with(multi_lang_state(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let mode_used = v["mode_used"].as_object().unwrap();
    assert_eq!(mode_used.len(), 3);
    assert!(mode_used.contains_key("en"));
    assert!(mode_used.contains_key("ja"));
    assert!(mode_used.contains_key("zh"));
    // Match is in en; mode_used still carries every scanned lang.
    let matches = v["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["lang"], "en");
}

#[tokio::test]
async fn languages_endpoint_shows_cjk_defaults() {
    let req = authed("GET", "/v1/languages", Body::empty());
    let resp = send_with(multi_lang_state(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let entries = v["languages"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    // Alphabetical order: en, ja, zh.
    assert_eq!(entries[0]["code"], "en");
    assert_eq!(entries[0]["default_mode"], "strict");
    assert_eq!(entries[1]["code"], "ja");
    assert_eq!(entries[1]["default_mode"], "substring");
    assert_eq!(entries[2]["code"], "zh");
    assert_eq!(entries[2]["default_mode"], "substring");
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

// ---------------------------------------------------------------------------
// M5 — limits, backpressure, and remaining error-table rows
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalid_mode_is_422() {
    let req = authed(
        "POST",
        "/v1/check",
        Body::from(r#"{"text":"hi","mode":"loose"}"#),
    );
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let v = json_body(resp).await;
    assert_eq!(v["error"], "invalid_mode");
}

#[tokio::test]
async fn post_normalization_size_returns_413() {
    // U+FDFA (ARABIC LIGATURE SALLALLAHOU...) NFKC-decomposes to 18 codepoints
    // (~33 UTF-8 bytes). 10 000 copies: ~30 KiB raw (under the 64 KiB
    // RequestBodyLimitLayer cap) but ~322 KiB normalized — over the 192 KiB
    // MAX_NORMALIZED_BYTES. Exercises the post-normalization 413 rail that
    // lives in routes::check, distinct from the raw-body 413 from M3.
    let text: String = "\u{FDFA}".repeat(10_000);
    let body = serde_json::to_vec(&serde_json::json!({ "text": text })).unwrap();
    assert!(
        body.len() < 64 * 1024,
        "raw body must stay under the 64 KiB cap so only the post-norm rail fires"
    );
    let req = authed("POST", "/v1/check", Body::from(body));
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let v = json_body(resp).await;
    assert_eq!(v["error"], "payload_too_large");
}

#[tokio::test]
async fn overloaded_returns_503_with_x_list_version() {
    let req = authed("POST", "/v1/check", Body::from(r#"{"text":"hi"}"#));
    let resp = send_with(saturated_state(), req).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        resp.headers()
            .get("x-list-version")
            .expect("X-List-Version on 503 overloaded")
            .to_str()
            .unwrap(),
        LIST_VERSION
    );
    let v = json_body(resp).await;
    assert_eq!(v["error"], "overloaded");
}

#[tokio::test]
async fn overloaded_does_not_gate_v1_languages() {
    // DESIGN §Deployment scopes BWS_MAX_INFLIGHT to /v1/check; /v1/languages
    // must succeed even when the counter is saturated.
    let req = authed("GET", "/v1/languages", Body::empty());
    let resp = send_with(saturated_state(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn overloaded_state_still_fast_paths_missing_auth() {
    // Auth runs before the gate; an unauthenticated request to a saturated
    // service still gets 401, not 503. Protects DESIGN's ordering invariant.
    let req = Request::builder()
        .method("POST")
        .uri("/v1/check")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"hi"}"#))
        .unwrap();
    let resp = send_with(saturated_state(), req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn inflight_counter_decrements_after_successful_request() {
    // RAII guard in limits::gate must decrement the shared counter so a burst
    // of sequential requests doesn't leak admissions and eventually self-DoS.
    let state = test_state();
    assert_eq!(state.inflight.load(Ordering::Relaxed), 0);
    let req = authed("POST", "/v1/check", Body::from(r#"{"text":"hi"}"#));
    let resp = send_with(state.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(state.inflight.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn inflight_counter_decrements_after_rejected_request() {
    // 422/400 paths still run inside the gate, so the guard's Drop must fire
    // on error responses too.
    let state = test_state();
    assert_eq!(state.inflight.load(Ordering::Relaxed), 0);
    let req = authed("POST", "/v1/check", Body::from(r#"{"text":""}"#));
    let resp = send_with(state.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(state.inflight.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn overloaded_rejection_does_not_leak_counter() {
    // Saturated gate: reject path does fetch_add then fetch_sub — the counter
    // must land back on its pre-request value.
    let state = saturated_state();
    let start = state.inflight.load(Ordering::Relaxed);
    let req = authed("POST", "/v1/check", Body::from(r#"{"text":"hi"}"#));
    let resp = send_with(state.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(state.inflight.load(Ordering::Relaxed), start);
}
