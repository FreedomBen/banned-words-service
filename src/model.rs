//! Request/response DTOs for `/v1/check` and `/v1/languages`, plus the
//! `/readyz` body shape. See DESIGN §"POST /v1/check" and §"Other endpoints".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `/v1/check` request.
///
/// `mode` is typed as `Option<String>` (not a dedicated enum) so an
/// unrecognized value reaches the handler and returns 422 `invalid_mode`,
/// keeping that row distinct from the malformed-JSON 400 rail. The `overrides`
/// field reserved by DESIGN is silently accepted because `serde`'s
/// `deny_unknown_fields` is deliberately off — see CLAUDE.md invariants.
#[derive(Debug, Deserialize)]
pub struct CheckRequest {
    pub text: String,
    #[serde(default)]
    pub langs: Option<Vec<String>>,
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MatchDto {
    pub lang: String,
    pub term: String,
    pub matched_text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Serialize)]
pub struct CheckResponse {
    pub list_version: &'static str,
    /// `lang → wire-mode-string`. `BTreeMap` gives alphabetical, deterministic
    /// JSON ordering, which matters for test stability.
    pub mode_used: BTreeMap<String, &'static str>,
    pub matches: Vec<MatchDto>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct LanguagesEntry {
    pub code: String,
    pub default_mode: &'static str,
}

#[derive(Debug, Serialize)]
pub struct LanguagesResponse {
    pub languages: Vec<LanguagesEntry>,
}

/// `/readyz` body. When `ready=false`, the two optional fields are omitted
/// entirely (DESIGN §"Other endpoints").
#[derive(Debug, Serialize)]
pub struct ReadyResponse {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_version: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub languages: Option<usize>,
}
