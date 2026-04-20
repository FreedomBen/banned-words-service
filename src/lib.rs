//! Vocab Veto — stateless multi-language banned-words HTTP service.
//!
//! Top-level modules: `config` (env+TOML loader), `auth` (bearer middleware),
//! `error` (canonical `ApiError`), `matcher` (scan engine + generated term
//! tables), `model` (request/response DTOs), `observability` (tracing +
//! Prometheus recorder + RED middleware), `routes` (router wiring and
//! handlers), `state` (shared `AppState`).

pub mod auth;
pub mod cli;
pub mod config;
pub mod error;
pub mod limits;
pub mod matcher;
pub mod model;
pub mod observability;
pub mod routes;
pub mod state;

pub use matcher::{
    Engine, Match, Mode, NormalizeError, ScanResult, DEFAULT_MODE, LIST_VERSION,
    MAX_NORMALIZED_BYTES,
};
pub use model::*;
pub use routes::build_router;
