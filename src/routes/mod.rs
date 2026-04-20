//! Router wiring and middleware stack for `/v1/*` and the unauthenticated
//! health/ready endpoints. IMPLEMENTATION_PLAN M3 item 8 prescribes the layer
//! ordering; the comments in `build_router` trace request flow.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::auth::require_bearer;
use crate::error::ApiError;
use crate::limits::gate as inflight_gate;
use crate::state::AppState;

pub mod check;
pub mod health;
pub mod languages;

const RAW_BODY_LIMIT_BYTES: usize = 64 * 1024;

pub fn build_router(state: Arc<AppState>) -> Router {
    let x_list_version = HeaderName::from_static("x-list-version");
    let list_version_value =
        HeaderValue::from_str(state.list_version).expect("LIST_VERSION is ASCII hex");

    // Layers apply inside-out. Request flow on /v1/check is therefore:
    //   X-List-Version (response-only, outermost)
    //   → auth (fast 401 before body parse, before the gate)
    //   → remap_413 (rewrites tower-http's default 413 body)
    //   → RequestBodyLimitLayer (64 KiB raw-body cap)
    //   → inflight_gate (/v1/check only; excludes /v1/languages)
    //   → handler (post-normalization 192 KiB cap via NormalizeError::TooLarge)
    //
    // /v1/languages shares everything in the chain except the gate — DESIGN
    // §Deployment scopes BWS_MAX_INFLIGHT to /v1/check only. The gate layer
    // is applied to a router that only contains /v1/check, then /v1/languages
    // is added afterwards so the layer doesn't reach it.
    let v1: Router<Arc<AppState>> = Router::new()
        .route("/v1/check", post(check::check))
        .layer(from_fn_with_state(state.clone(), inflight_gate))
        .route("/v1/languages", get(languages::languages))
        .layer(RequestBodyLimitLayer::new(RAW_BODY_LIMIT_BYTES))
        .layer(from_fn(remap_413))
        .layer(from_fn_with_state(state.clone(), require_bearer))
        .layer(SetResponseHeaderLayer::overriding(
            x_list_version,
            list_version_value,
        ));

    let unauth: Router<Arc<AppState>> = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz));

    Router::new()
        .merge(v1)
        .merge(unauth)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Catches 413 responses from `RequestBodyLimitLayer` (which ships a plain
/// body) and rewrites them to the canonical `{error, message}` shape so every
/// `/v1/*` 4xx matches DESIGN §API error table.
async fn remap_413(req: Request, next: Next) -> Response {
    let resp = next.run(req).await;
    if resp.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return ApiError::PayloadTooLarge.into_response();
    }
    resp
}
