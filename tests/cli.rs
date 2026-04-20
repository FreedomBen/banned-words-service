//! End-to-end tests for the `vv` CLI. Spawns the compiled binary via
//! `assert_cmd` and asserts stdout/stderr/exit-code for the scenarios
//! enumerated in CLI_IMPLEMENTATION_PLAN.md (CM2+).
//!
//! Exit-code semantics mirror the plan's table:
//! `0` clean, `1` hits or truncated, `2` usage/input-validation,
//! `3` post-normalization too large.

use assert_cmd::Command;
use banned_words_service::MAX_NORMALIZED_BYTES;
use serde_json::Value;

fn vv() -> Command {
    Command::cargo_bin("vv").expect("vv binary not built — run `cargo build --bin vv`")
}

fn stdout_json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON: {e}\nraw: {:?}", out.stdout))
}

fn stderr_str(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn scunthorpe_under_strict_en_is_clean() {
    let out = vv()
        .args(["check", "--text", "Scunthorpe", "--lang", "en"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let body = stdout_json(&out);
    assert_eq!(body["matches"].as_array().unwrap().len(), 0);
    assert_eq!(body["truncated"], Value::Bool(false));
    assert_eq!(body["mode_used"]["en"], "strict");
}

#[test]
fn matched_word_exits_one_with_match_dto() {
    // Uses a term present in LDNOOBW 'en' at the pinned SHA; the match
    // verifies exit code 1 and the DTO shape.
    let out = vv()
        .args(["check", "--text", "fuck that", "--lang", "en"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_str(&out));
    let body = stdout_json(&out);
    let matches = body["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    let m = &matches[0];
    assert_eq!(m["lang"], "en");
    assert!(m["start"].is_u64());
    assert!(m["end"].is_u64());
    assert!(m["term"].is_string());
    assert!(m["matched_text"].is_string());
}

#[test]
fn fullwidth_evasion_substring_folds_to_ascii() {
    // FULLWIDTH LATIN CAPITAL F U C K → normalized "fuck" → matches en.
    let fullwidth = "\u{FF26}\u{FF35}\u{FF23}\u{FF2B}";
    let out = vv()
        .args([
            "check",
            "--text",
            fullwidth,
            "--lang",
            "en",
            "--mode",
            "substring",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_str(&out));
    let body = stdout_json(&out);
    let matches = body["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["matched_text"], fullwidth);
}

#[test]
fn explicit_strict_on_cjk_is_honored_not_clamped() {
    // Even though zh defaults to substring, an explicit --mode strict wins
    // and mode_used echoes "strict" — the audit trail the plan mandates.
    let out = vv()
        .args([
            "check",
            "--text",
            "hello",
            "--lang",
            "zh",
            "--mode",
            "strict",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let body = stdout_json(&out);
    assert_eq!(body["mode_used"]["zh"], "strict");
}

#[test]
fn json_input_silently_ignores_overrides_and_unknown_fields() {
    let body = r#"{"text":"hello","langs":["en"],"overrides":{"allowlist":["x"]},"future_field":42}"#;
    let out = vv()
        .args(["check", "--json-input", "-"])
        .write_stdin(body)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let parsed = stdout_json(&out);
    assert_eq!(parsed["mode_used"]["en"], "strict");
    assert_eq!(parsed["matches"].as_array().unwrap().len(), 0);
}

#[test]
fn omitted_lang_scans_every_compiled_language() {
    let out = vv().args(["check", "--text", "hello"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_str(&out));
    let body = stdout_json(&out);
    let mode_used = body["mode_used"].as_object().unwrap();
    // 27 is the compile-time allowlist width (see build.rs ALLOWLIST).
    // Using a literal ensures drift in the vendored list is flagged here
    // rather than silently widening scan scope.
    assert_eq!(mode_used.len(), 27);
}

#[test]
fn unknown_language_exits_two_with_listing() {
    let out = vv()
        .args(["check", "--text", "x", "--lang", "xx"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = stderr_str(&out);
    assert!(stderr.contains("unknown language: xx"), "stderr: {stderr}");
    assert!(stderr.contains("en"), "stderr should list en: {stderr}");
}

#[test]
fn invalid_mode_exits_two() {
    let out = vv()
        .args(["check", "--text", "x", "--lang", "en", "--mode", "loose"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_str(&out).contains("invalid mode: loose"));
}

#[test]
fn empty_text_via_text_flag_exits_two() {
    let out = vv()
        .args(["check", "--text", "", "--lang", "en"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_str(&out).contains("empty text"));
}

#[test]
fn json_input_empty_langs_exits_two() {
    let body = r#"{"text":"hello","langs":[]}"#;
    let out = vv()
        .args(["check", "--json-input", "-"])
        .write_stdin(body)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_str(&out).contains("empty langs"));
}

#[test]
fn json_input_malformed_exits_two_as_invalid_json() {
    let out = vv()
        .args(["check", "--json-input", "-"])
        .write_stdin("{not json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_str(&out).contains("invalid JSON"));
}

#[test]
fn payload_too_large_exits_three() {
    // One byte past the normalize cap; ASCII 'a' is byte-identical after
    // NFKC so the raw length equals the normalized length.
    let big = "a".repeat(MAX_NORMALIZED_BYTES + 1);
    let out = vv()
        .args(["check", "--stdin", "--lang", "en"])
        .write_stdin(big)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        stderr_str(&out),
    );
}
