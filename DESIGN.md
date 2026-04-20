# Banned Words Service — Design

## Goal

A high-throughput, low-latency HTTP service that answers: *"Does this string contain a banned word?"* across multiple languages. The authoritative word list is sourced from [LDNOOBW](https://github.com/LDNOOBW/List-of-Dirty-Naughty-Obscene-and-Otherwise-Bad-Words).

Target: p99 latency < 1 ms for strings up to 1 KiB on a single core; tens of thousands of RPS per instance.

## Non-goals

- Semantic moderation, toxicity scoring, or ML-based classification.
- Admin UI for editing the list (the list is baked in at build or startup).

## Design principles

1. **Fully stateless, zero external dependencies.** No database, no Redis, no disk reads on the hot path. The entire word list is compiled into the binary via `build.rs` and loaded into Aho-Corasick automatons at startup. Pods are fungible — kill one, start another, identical state in milliseconds. Horizontal scaling is `replicas: N`.
2. **Immutable at runtime.** The word list only changes via redeploy. The image tag *is* the list version. This keeps the hot path lock-free and makes the running version trivially auditable.
3. **Hot path is allocation-free.** One `Arc<AhoCorasick>` per language, shared across all request tasks. No per-request automaton construction, no per-request heap churn beyond the match buffer.
4. **Sensible defaults, explicit overrides.** Callers can send just `{"text": "..."}` and get a correct answer. Power users can pin `langs` and `mode` per request.

## Tech stack

| Layer        | Choice                                    | Why                                                           |
| ------------ | ----------------------------------------- | ------------------------------------------------------------- |
| Language     | Rust (stable)                             | Zero-cost abstractions, predictable latency, no GC pauses.    |
| HTTP         | `axum` + `tokio`                          | Mature async stack; trivial to scale across cores.            |
| Matching     | `aho-corasick` crate                      | O(n) multi-pattern scan in a single pass over the input.      |
| Normalization| `unicode-normalization` (NFKC) + `caseless` | Handle case folding and Unicode equivalence consistently.   |
| Config       | `figment` (env + TOML)                    | 12-factor friendly; easy local overrides.                     |
| Observability| `tracing` + `tracing-subscriber`, Prometheus metrics via `axum-prometheus` | Structured logs + RED metrics out of the box. |
| Container    | Distroless static image                   | Small attack surface, fast cold start.                        |

**Alternative considered:** Go + `cloudflare/ahocorasick`. Faster to ship, meaningfully slower in published benchmarks, and GC jitter affects tail latency. Rust wins on the stated perf goal; revisit if team velocity matters more than p99.

## Data model

LDNOOBW ships one file per language (e.g. `en`, `es`, `fr`, `de`, `ja`, …). At build time we:

1. Vendor the repo as a git submodule or download pinned by commit SHA.
2. Generate a `phf`-backed map `lang -> &[&str]` via `build.rs`.
3. At startup, build one `AhoCorasick` automaton per language and store them in an `Arc<HashMap<Lang, AhoCorasick>>`. Automatons are immutable and shared across all request tasks.

Memory footprint estimate: LDNOOBW is ~5k terms total across languages; the combined DFAs are well under 10 MiB.

## Matching semantics

Both **strict** (whole-word) and **substring** matching are first-class in v1. Callers pick per request via the `mode` field; when `mode` is omitted, the server applies the per-language default listed below.

- **Normalization.** Input is NFKC-normalized, then lowercased via `caseless`, in both modes. NFKC (vs NFC) is chosen deliberately — it folds compatibility forms (fullwidth, ligatures, superscripts) that are otherwise trivial evasion vectors.
- **`mode: "strict"`.** A term matches only when both edges fall on a Unicode word boundary per UAX #29. Mitigates the **Scunthorpe problem** for space-delimited languages.
- **`mode: "substring"`.** Any Aho-Corasick hit counts. Appropriate for CJK (UAX #29 word boundaries are unreliable there) and for callers who explicitly want aggressive matching.
- **Per-language default** (applied when `mode` is omitted): `strict` for space-delimited languages (en, es, fr, de, pt, it, nl, ru, …); `substring` for CJK (ja, zh, ko). The mode chosen per language is echoed back in `mode_used` so callers can audit.
- **Span semantics.** Match `start`/`end` are byte offsets into the *normalized* request text, not the caller's original input. NFKC can change byte length, so they do not round-trip directly. Callers wishing to redact must re-normalize on their side. (See Open Question #2 — we may revisit and map back to original offsets.)

Both modes share the same automaton; the difference is a post-match boundary check, so there is no meaningful perf gap between them.

## API

### POST /v1/check

Request body (JSON, max 64 KiB — 413 `payload_too_large` above that):

```json
{
  "text": "some user input",
  "langs": ["en", "es"],
  "mode": "strict"
}
```

- `text` — string, required, non-empty.
- `langs` — array of loaded language codes, optional. Defaults to every loaded language.
- `mode` — `"strict"` | `"substring"`, optional. Omit to apply the per-language default.

Success response (200):

```json
{
  "banned": true,
  "mode_used": { "en": "strict", "ja": "substring" },
  "matches": [
    { "lang": "en", "term": "****", "start": 12, "end": 16 }
  ],
  "truncated": false
}
```

- When `banned: false`, `matches` is `[]`. `mode_used` is always populated with an entry for every requested (or defaulted) language.
- Matches are returned in leftmost-longest, non-overlapping order, capped at **256 per request**. If more were found, `truncated` is `true`. This bounds response size on pathological input.

Error response (4xx):

```json
{ "error": "invalid_mode", "message": "mode must be 'strict' or 'substring'" }
```

| HTTP | `error`             | Condition                                         |
| ---- | ------------------- | ------------------------------------------------- |
| 400  | `bad_request`       | Malformed JSON, or missing/empty `text`.          |
| 413  | `payload_too_large` | Request body exceeds 64 KiB.                      |
| 422  | `invalid_mode`      | `mode` is present but not in the enum.            |
| 422  | `unknown_language`  | A `langs` entry is not a loaded language code.    |

### Other endpoints

- `GET /healthz` — liveness.
- `GET /readyz` — readiness (automatons loaded).
- `GET /metrics` — Prometheus scrape.
- `GET /v1/languages` — list of loaded language codes and each one's default mode.

### Why return match spans

Callers often want to redact or highlight, not just know the boolean. Returning spans costs almost nothing (Aho-Corasick produces them natively) and avoids a second round trip.

## Performance plan

- Single shared `Arc<AhoCorasick>` per language; no per-request allocation for the automaton.
- Automatons built with leftmost-longest, non-overlapping match kind — bounds match cardinality without extra filtering logic.
- Reuse a `Vec<Match>` buffer per task via a small object pool if profiling shows allocations dominate.
- Criterion benchmarks committed alongside the code; regressions fail CI.
- Load test with `oha` or `vegeta` against a representative corpus before each release.

## Deployment

- Single stateless binary, horizontally scalable.
- Container image built via `cargo chef` for fast layer caching.
- Config via env vars:
  - `BWS_LISTEN_ADDR` — HTTP listen address (e.g. `0.0.0.0:8080`).
  - `BWS_LANGS` — optional comma-separated runtime allowlist (e.g. `en,es,fr`). Defaults to every language compiled into the binary. Useful for slimming a single fat image down to a specific deployment's needs without rebuilding.
  - *(No `BWS_DEFAULT_MODE` — mode defaulting is per-language and defined in code, not config, so behavior is identical across deployments.)*
- **List updates ship via redeploy.** No hot reload, ever — it keeps the hot path lock-free and makes the running version trivially auditable (image tag = list version).

## Deferred to v2

- **Per-tenant allowlist / denylist overrides.** The v1 request schema will silently accept (and ignore) an `overrides` field, so adding real semantics later is non-breaking.
- **Leetspeak / homoglyph normalization.** Requires careful false-positive analysis before shipping.
- **Multi-tenant rate limiting.** Likely belongs in the gateway, not this service — revisit if that assumption breaks.

## Open questions

1. **Versioning of the word list.** Expose the LDNOOBW commit SHA in `/readyz` so callers can audit exactly which list version they're hitting.
2. **Match span offsets — normalized vs original.** v1 currently documents spans as byte offsets over the *normalized* text. If callers need to redact directly from their original input, we should instead maintain a normalization offset map and translate spans back to original byte offsets (small amount of extra code, negligible perf impact). Decision needed before v1.0.

## Milestones

1. Scaffold crate, vendor LDNOOBW, build-time codegen of term tables.
2. `/v1/check` end-to-end with both `strict` and `substring` modes for `en`.
3. All LDNOOBW languages loaded; per-language mode override.
4. Metrics, health checks, container image.
5. Criterion benches + CI perf gates.
6. Load test report + v1.0 tag.
