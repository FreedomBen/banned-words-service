//! POST /v1/check. Validates the request, runs the matcher, returns the
//! canonical response. See DESIGN §"POST /v1/check" and IMPLEMENTATION_PLAN
//! M3 item 5.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::error::ApiError;
use crate::matcher::{Mode, NormalizeError};
use crate::model::{CheckRequest, CheckResponse, MatchDto};
use crate::state::AppState;

pub async fn check(State(state): State<Arc<AppState>>, req: Request) -> Result<Response, ApiError> {
    // The outer RequestBodyLimitLayer rejects oversize bodies upfront when
    // Content-Length is present. For chunked transfers it wraps the body in
    // `http_body_util::Limited`, which errors mid-stream once the cap is
    // exceeded; walking the source chain catches that path and keeps the 413
    // classification consistent across both.
    let bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            if is_length_limit_error(&e) {
                return Err(ApiError::PayloadTooLarge);
            }
            return Err(ApiError::BadRequest(format!("failed to read body: {e}")));
        }
    };

    let CheckRequest { text, langs, mode } = serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::BadRequest(format!("malformed JSON: {e}")))?;

    if text.is_empty() {
        return Err(ApiError::EmptyText);
    }

    let mode_resolved: Option<Mode> = match mode.as_deref() {
        None => None,
        Some("strict") => Some(Mode::Strict),
        Some("substring") => Some(Mode::Substring),
        Some(_) => return Err(ApiError::InvalidMode),
    };

    let scan_langs: Vec<String> = match langs {
        Some(v) if v.is_empty() => return Err(ApiError::EmptyLangs),
        Some(v) => {
            let mut out: Vec<String> = Vec::with_capacity(v.len());
            for raw in v {
                let lower = raw.to_ascii_lowercase();
                if !state.engine.has_language(&lower) {
                    return Err(ApiError::UnknownLanguage(lower));
                }
                out.push(lower);
            }
            out
        }
        None => {
            let mut all: Vec<String> = state.engine.languages().cloned().collect();
            all.sort();
            all
        }
    };

    let result = match state.engine.scan(&text, &scan_langs, mode_resolved) {
        Ok(r) => r,
        Err(NormalizeError::TooLarge) => return Err(ApiError::PayloadTooLarge),
    };

    let mut mode_used: BTreeMap<String, &'static str> = BTreeMap::new();
    for (lang, m) in result.mode_used {
        mode_used.insert(lang, m.as_wire_str());
    }

    let matches: Vec<MatchDto> = result
        .matches
        .into_iter()
        .map(|m| MatchDto {
            lang: m.lang,
            term: m.term,
            matched_text: m.matched_text,
            start: m.start,
            end: m.end,
        })
        .collect();

    let resp = CheckResponse {
        list_version: state.list_version,
        mode_used,
        matches,
        truncated: result.truncated,
    };

    Ok((StatusCode::OK, Json(resp)).into_response())
}

/// Walk the error's source chain looking for `http_body_util::LengthLimitError`
/// — signals that the `RequestBodyLimitLayer`'s `Limited` wrapper tripped
/// mid-stream (no Content-Length set, so the upfront check couldn't fire).
fn is_length_limit_error(err: &axum::Error) -> bool {
    use std::error::Error;
    let mut cur: Option<&dyn Error> = Some(err);
    while let Some(e) = cur {
        if e.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        cur = e.source();
    }
    false
}
