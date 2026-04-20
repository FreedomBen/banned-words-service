# Banned Words Service — Design

## Goal

A high-throughput, low-latency HTTP service that answers: *"Does this string contain a banned word?"* across multiple languages. The authoritative word list is sourced from [LDNOOBW](https://github.com/LDNOOBW/List-of-Dirty-Naughty-Obscene-and-Otherwise-Bad-Words).

Target: p99 latency < 1 ms for strings up to 1 KiB on a single core; tens of thousands of RPS per instance.

## Non-goals

- Semantic moderation, toxicity scoring, or ML-based classification.
- Leetspeak / homoglyph normalization in v1 (tracked as future work).
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

**Alternative considered:** Go + `cloudflare/ahocorasick`. Faster to ship, ~20–30% slower in microbenchmarks, GC jitter affects tail latency. Rust wins on the stated perf goal; revisit if team velocity matters more than p99.

## Data model

LDNOOBW ships one file per language (e.g. `en`, `es`, `fr`, `de`, `ja`, …). At build time we:

1. Vendor the repo as a git submodule or download pinned by commit SHA.
2. Generate a `phf`-backed map `lang -> &[&str]` via `build.rs`.
3. At startup, build one `AhoCorasick` automaton per language and store them in an `Arc<HashMap<Lang, AhoCorasick>>`. Automatons are immutable and shared across all request tasks.

Memory footprint estimate: LDNOOBW is ~5k terms total across languages; the combined DFAs are well under 10 MiB.

## Matching semantics

Both **whole-word** and **substring** matching are first-class in v1. Callers pick per request via the `mode` field; there is no hidden default magic beyond a sensible fallback when `mode` is omitted.

- Input is normalized (NFKC, lowercased via `caseless`) in both modes.
- `mode: "strict"` — term must be bounded by non-word characters or string boundaries. Mitigates the **Scunthorpe problem** for space-delimited languages.
- `mode: "substring"` — raw Aho-Corasick hit anywhere in the input. Appropriate for CJK and for callers who explicitly want aggressive matching.
- When `mode` is omitted, the server picks per language: `strict` for space-delimited languages (en, es, fr, de, …), `substring` for CJK (ja, zh, …). The chosen mode is echoed back in the response so callers can audit.

Both modes share the same automaton; the difference is a post-match boundary check, so there is no meaningful perf gap between them.

## API

Single endpoint, JSON in, JSON out.

```
POST /v1/check
Content-Type: application/json

{
  "text": "some user input",
  "langs": ["en", "es"],        // optional; defaults to all loaded languages
  "mode": "strict"              // optional; "strict" | "substring" — omit for per-language default
}
```

Response:

```json
{
  "banned": true,
  "mode_used": { "en": "strict", "ja": "substring" },
  "matches": [
    { "lang": "en", "term": "****", "start": 12, "end": 16, "mode": "strict" }
  ]
}
```

Also:

- `GET /healthz` — liveness.
- `GET /readyz` — readiness (automatons loaded).
- `GET /metrics` — Prometheus scrape.
- `GET /v1/languages` — list of loaded language codes.

### Why return match spans

Callers often want to redact or highlight, not just know the boolean. Returning spans costs almost nothing (Aho-Corasick produces them natively) and avoids a second round trip.

## Performance plan

- Single shared `Arc<AhoCorasick>` per language; no per-request allocation for the automaton.
- Reuse a `Vec<Match>` buffer per task via a small object pool if profiling shows allocations dominate.
- Criterion benchmarks committed alongside the code; regressions fail CI.
- Load test with `oha` or `vegeta` against a representative corpus before each release.

## Deployment

- Single stateless binary, horizontally scalable.
- Container image built via `cargo chef` for fast layer caching.
- Config via env vars: `BWS_LANGS`, `BWS_DEFAULT_MODE`, `BWS_LISTEN_ADDR`.
- **List updates ship via redeploy.** No hot reload, ever — it keeps the hot path lock-free and makes the running version trivially auditable (image tag = list version).

## Deferred to v2

- **Per-tenant allowlist / denylist overrides.** The request schema will reserve an `overrides` field in v1 responses as `null` so adding it later is non-breaking.
- **Leetspeak / homoglyph normalization.** Requires careful false-positive analysis before shipping.
- **Multi-tenant rate limiting.** Likely belongs in the gateway, not this service — revisit if that assumption breaks.

## Open questions

1. **Versioning of the word list.** Expose the LDNOOBW commit SHA in `/readyz` so callers can audit exactly which list version they're hitting.

## Milestones

1. Scaffold crate, vendor LDNOOBW, build-time codegen of term tables.
2. `/v1/check` end-to-end with whole-word matching for `en`.
3. All LDNOOBW languages loaded; per-language mode override.
4. Metrics, health checks, container image.
5. Criterion benches + CI perf gates.
6. Load test report + v1.0 tag.
