//! In-flight gate for `/v1/check` (IMPLEMENTATION_PLAN M5 item 1). An
//! `Arc<AtomicUsize>` held on `AppState` tracks live handler occupancy; the
//! middleware rejects with 503 `overloaded` when admission would push past
//! `max_inflight`. An RAII guard decrements the counter whether the handler
//! returns a response or the future is dropped mid-flight (client disconnect).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::state::AppState;

struct InFlightGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

pub async fn gate(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let prev = state.inflight.fetch_add(1, Ordering::Relaxed);
    if prev >= state.max_inflight {
        state.inflight.fetch_sub(1, Ordering::Relaxed);
        return ApiError::Overloaded.into_response();
    }
    let _guard = InFlightGuard {
        counter: state.inflight.clone(),
    };
    next.run(req).await
}
