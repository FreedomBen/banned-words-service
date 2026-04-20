# Banned Words Service — Design

## Goal

A high-throughput, low-latency HTTP service that answers: *"Does this string contain a banned word?"* across multiple languages. The authoritative word list is sourced from [LDNOOBW](https://github.com/LDNOOBW/List-of-Dirty-Naughty-Obscene-and-Otherwise-Bad-Words).

Target: p99 latency < 1 ms for strings up to 1 KiB on a single core; tens of thousands of RPS per instance.

## Non-goals

- Semantic moderation, toxicity scoring, or ML-based classification.
- Leetspeak / homoglyph normalization in v1 (tracked as future work).
- Admin UI for editing the list (the list is baked in at build or startup).

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

The **Scunthorpe problem** forces an explicit choice. v1 will use **whole-word matching** with Unicode word boundaries:

- Input is normalized (NFKC, lowercased via `caseless`).
- A term matches only when surrounded by non-word characters (or string boundaries).
- For CJK languages where whitespace tokenization is unreliable, fall back to substring match. This is flagged per-language in config.

A `mode` query parameter (`strict` | `substring`) can override the default for callers who know what they want.

## API

Single endpoint, JSON in, JSON out.

```
POST /v1/check
Content-Type: application/json

{
  "text": "some user input",
  "langs": ["en", "es"],        // optional; defaults to all loaded languages
  "mode": "strict"              // optional; "strict" | "substring"
}
```

Response:

```json
{
  "banned": true,
  "matches": [
    { "lang": "en", "term": "****", "start": 12, "end": 16 }
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
- List updates = new image. No hot reload in v1 (keeps the hot path lock-free).

## Open questions

1. **Custom allowlist / denylist per tenant.** Out of scope for v1, but the API should leave room (`overrides` field) so we don't break clients later.
2. **Multi-tenant rate limiting.** Probably belongs in the gateway, not this service.
3. **Leetspeak normalization.** v2. Requires careful false-positive analysis.
4. **Versioning of the word list.** Expose the LDNOOBW commit SHA in `/readyz` so callers can audit what they're hitting.

## Milestones

1. Scaffold crate, vendor LDNOOBW, build-time codegen of term tables.
2. `/v1/check` end-to-end with whole-word matching for `en`.
3. All LDNOOBW languages loaded; per-language mode override.
4. Metrics, health checks, container image.
5. Criterion benches + CI perf gates.
6. Load test report + v1.0 tag.
