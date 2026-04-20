//! Shared runtime state. An `Arc<AppState>` is threaded through axum's
//! `State<S>` extractor and captured by the auth middleware closure.

use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;

use crate::matcher::Engine;

pub struct AppState {
    pub engine: Arc<Engine>,
    /// Pre-parsed bearer tokens as raw bytes. Comparison is constant-time over
    /// the full set (see `auth::require_bearer`).
    pub api_keys: Vec<Vec<u8>>,
    /// Pinned LDNOOBW SHA; attached to every `/v1/*` response via the
    /// `X-List-Version` layer and echoed in `CheckResponse` / `/readyz`.
    pub list_version: &'static str,
    /// Flipped to `true` in `main` before the listener binds. `/readyz`
    /// observes this with `Acquire` ordering.
    pub ready: AtomicBool,
    /// Configured `BWS_MAX_INFLIGHT`; ceiling consulted by the `limits::gate`
    /// middleware mounted on `/v1/check`.
    pub max_inflight: usize,
    /// Live count of requests currently executing the `/v1/check` handler.
    /// `Arc<AtomicUsize>` so the gate and the M6 `bws_inflight` gauge share a
    /// single cell without touching the whole `AppState`.
    pub inflight: Arc<AtomicUsize>,
}
