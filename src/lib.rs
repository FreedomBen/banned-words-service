//! Vocab Veto — stateless multi-language banned-words HTTP service.
//!
//! Top-level modules: `config` (env+TOML loader), `auth` (bearer middleware),
//! `error` (canonical `ApiError`), `matcher` (scan engine + generated term
//! tables), `model` (request/response DTOs), `routes` (router wiring and
//! handlers), `state` (shared `AppState`).

pub mod auth;
pub mod config;
pub mod error;
pub mod matcher;
pub mod model;
pub mod routes;
pub mod state;

pub use routes::build_router;
