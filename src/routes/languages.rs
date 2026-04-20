//! GET /v1/languages. Serves the canonical shape
//! `{"languages": [{"code", "default_mode"}, ...]}` in alphabetical order,
//! restricted to languages currently loaded in the engine. DESIGN §"Other
//! endpoints" and IMPLEMENTATION_PLAN M3 item 6.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;

use crate::matcher::{Mode, DEFAULT_MODE};
use crate::model::{LanguagesEntry, LanguagesResponse};
use crate::state::AppState;

pub async fn languages(State(state): State<Arc<AppState>>) -> Json<LanguagesResponse> {
    let mut codes: Vec<String> = state.engine.languages().cloned().collect();
    codes.sort();
    let entries: Vec<LanguagesEntry> = codes
        .into_iter()
        .map(|code| {
            let default_mode = DEFAULT_MODE
                .get(code.as_str())
                .copied()
                .unwrap_or(Mode::Substring);
            LanguagesEntry {
                code,
                default_mode: default_mode.as_wire_str(),
            }
        })
        .collect();
    Json(LanguagesResponse { languages: entries })
}
