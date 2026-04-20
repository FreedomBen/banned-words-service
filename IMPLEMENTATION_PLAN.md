# Banned Words Service — Implementation Plan

A concrete, milestone-ordered plan for building the service specified in [DESIGN.md](./DESIGN.md). Each milestone is independently shippable and verifiable.

## Conventions

- Rust edition 2021, stable toolchain pinned in `rust-toolchain.toml`.
- Formatting: `cargo fmt` (default config). Lints: `cargo clippy -- -D warnings`.
- CI runs `fmt --check`, `clippy`, `test`, `bench --no-run`, and builds the container image.
- Every milestone ends with: code compiles, lints clean, tests pass, docs updated.

## Repository layout (target)

```
banned-words-service/
├── Cargo.toml
├── rust-toolchain.toml
├── build.rs                       # codegen: LDNOOBW → phf term tables
├── DESIGN.md
├── IMPLEMENTATION_PLAN.md
├── vendor/ldnoobw/                # git submodule, pinned SHA
├── src/
│   ├── main.rs                    # binary entry (config load → server)
│   ├── lib.rs                     # re-exports; keeps main.rs thin
│   ├── config.rs                  # figment: env + TOML → Config struct
│   ├── auth.rs                    # Bearer parse + constant-time compare
│   ├── error.rs                   # ApiError enum → IntoResponse
│   ├── routes/
│   │   ├── mod.rs                 # Router wiring, middleware stack
│   │   ├── check.rs               # POST /v1/check
│   │   ├── languages.rs           # GET  /v1/languages
│   │   ├── health.rs              # /healthz, /readyz
│   │   └── metrics.rs             # /metrics
│   ├── matcher/
│   │   ├── mod.rs                 # Engine: Arc<HashMap<Lang, AhoCorasick>>
│   │   ├── normalize.rs           # NFKC + caseless + offset map
│   │   ├── boundary.rs            # UAX #29 word-boundary check
│   │   └── scan.rs                # per-language scan + span remap
│   ├── model.rs                   # Request/Response DTOs (serde)
│   ├── observability.rs           # tracing-subscriber + metrics registry
│   └── limits.rs                  # in-flight gate, body-size layer
├── tests/                         # integration tests (axum TestServer)
├── benches/                       # criterion benches
└── deploy/
    ├── Dockerfile                 # cargo-chef + distroless
    └── k8s/                       # manifests, probes, HPA
```

## Milestone 1 — Scaffold and build-time codegen

**Goal.** Empty binary that loads the LDNOOBW list at compile time.

1. `cargo init --bin`; commit `Cargo.toml` skeleton, workspace-free.
2. Add submodule: `git submodule add https://github.com/LDNOOBW/List-of-Dirty-Naughty-Obscene-and-Otherwise-Bad-Words vendor/ldnoobw`; pin to a specific SHA and record it in `build.rs`.
3. `build.rs`:
   - Walk `vendor/ldnoobw/`, pick up per-language files (`en`, `es`, …).
   - Emit a generated `src/generated_terms.rs` containing:
     - `pub const LIST_VERSION: &str = "<SHA>";`
     - `pub static TERMS: phf::Map<&'static str, &'static [&'static str]>` keyed by lowercase ISO code.
   - Emit `cargo:rerun-if-changed=vendor/ldnoobw` and on the submodule HEAD.
4. Hello-world `main.rs` that prints `LIST_VERSION` and term counts per language. Smoke-test: `cargo run` prints something plausible.

**Exit criteria.** `cargo build` green; `LIST_VERSION` matches the pinned submodule SHA; term counts sum to the expected ~5k.

## Milestone 2 — Matching core (library)

**Goal.** A pure-Rust matching engine, unit-tested in isolation from HTTP.

1. `matcher::normalize`:
   - NFKC via `unicode-normalization`, lowercased via `caseless`.
   - Returns `(String normalized, Vec<u32> offset_map)` where `offset_map[i]` is the starting byte offset in the original text for normalized-byte `i`. Single-pass.
   - Reject on length: normalized > 192 KiB → caller translates to 413.
2. `matcher::boundary`: `is_word_boundary(s: &str, byte_idx: usize) -> bool` per UAX #29, using `unicode-segmentation`.
3. `matcher::scan`:
   - `Engine::new(langs: &HashMap<Lang, &[&str]>) -> Engine` builds one `AhoCorasick` per lang with `MatchKind::LeftmostLongest`, non-overlapping.
   - `Engine::scan(text, langs, mode) -> ScanResult { mode_used, matches, truncated }`.
   - Span widening across NFKC expansions as specified in DESIGN §"Mapping across NFKC expansions".
   - 256-match cap applied *after* concatenation in caller-supplied `langs` order (alphabetical when omitted).
4. Unit tests covering: ASCII strict vs substring, fullwidth evasion, ligature expansion (`ﬁ`), CJK substring, Scunthorpe case, truncation boundary at exactly 256 and 257 hits, empty-text rejection at boundary layer.

**Exit criteria.** `cargo test --lib` green; criterion bench skeleton compiles.

## Milestone 3 — HTTP surface (happy path)

**Goal.** `/v1/check` end-to-end for a single language (`en`), `/v1/languages`, `/healthz`, `/readyz`.

1. `config.rs`: `BWS_LISTEN_ADDR`, `BWS_API_KEYS` (required, parsed per DESIGN), `BWS_LANGS`, `BWS_MAX_INFLIGHT` (default 1024). Fail-closed on missing keys.
2. `auth.rs`: extract `Authorization: Bearer <k>`, compare each candidate via `subtle::ConstantTimeEq`, **always iterating the full set**. Log `key_id = hex(sha256(key))[..8]` on success; log only `reason` on failure.
3. `error.rs`: single `ApiError` enum → `IntoResponse` producing `{error, message}` with the right status. All variants attach `X-List-Version` header.
4. `routes/check.rs`: deserialize `CheckRequest`, validate, call `Engine::scan`, serialize `CheckResponse`. `mode_used` populated for every requested language.
5. `routes/languages.rs`: static response from compiled table, alphabetical order.
6. `routes/health.rs`: `/healthz` always 200; `/readyz` reflects "engine built" flag. Listener binds *after* the engine is ready, so 503 is essentially unobservable in practice — still implemented for correctness.
7. Middleware: body-size limit (64 KiB), `X-List-Version` injector, request-id, `tracing` span per request.
8. Integration tests (hurl-style via `axum::Router::oneshot`) for: auth missing/invalid/valid, body too large, malformed JSON, empty `text`, empty `langs`, unknown language, happy path.

**Exit criteria.** `curl -H "Authorization: Bearer $K" -d '{"text":"..."}' :8080/v1/check` returns the documented shape.

## Milestone 4 — Multi-language and mode defaults

**Goal.** All LDNOOBW languages loaded; per-language mode default table wired up.

1. `matcher::mod`: `DEFAULT_MODE: phf::Map<&str, Mode>` — space-delimited langs → `Strict`, CJK (`ja`, `zh`, `ko`) → `Substring`.
2. `langs` defaulting: when omitted, scan every loaded language in alphabetical order.
3. `mode` defaulting: per-language lookup, echoed in `mode_used`. Explicit caller mode wins, including `strict` on CJK (no clamping).
4. `BWS_LANGS` runtime allowlist: fatal startup error on unknown codes, with a helpful message listing compiled codes.
5. Tests: mixed-language request, default vs explicit mode parity, CJK-strict honored, `BWS_LANGS` trimming and dedup.

**Exit criteria.** A single request across `en,ja,zh` returns a well-formed `mode_used` map and correctly-ordered matches.

## Milestone 5 — Limits, backpressure, and error surface

**Goal.** Every documented error code is reachable by a test.

1. In-flight cap: a tower layer backed by `Arc<AtomicUsize>` gating `/v1/check` only. Excludes `/healthz`, `/readyz`, `/metrics`, and 401-fast-path rejections (auth runs *before* the gate).
2. 413 at both raw-body (64 KiB, via `tower_http::limit::RequestBodyLimitLayer`) and post-normalization (192 KiB, inside the handler before scan).
3. 503 `overloaded` returns immediately when the gate is full.
4. Unknown-fields pass-through confirmed by test (including the reserved `overrides` key).
5. Error-table test: one test per row of the DESIGN §API error table.

**Exit criteria.** All documented 4xx/5xx paths have a test; `X-List-Version` present on every `/v1/*` response including errors.

## Milestone 6 — Observability

**Goal.** `/metrics` exposes the DESIGN §"Metrics contract" series with correct labels.

1. `axum-prometheus` for RED metrics; override buckets via env.
2. Custom metrics registered in `observability.rs`:
   - `bws_auth_failures_total{reason}`
   - `bws_match_duration_seconds{lang,mode}` — observed inside `scan` per lang.
   - `bws_matches_per_request`, `bws_truncated_total`, `bws_input_bytes`.
   - `bws_list_version_info{list_version}` set to 1 at startup.
   - `bws_languages_loaded` gauge, `bws_inflight` gauge (tied to the cap).
3. `tracing-subscriber` with JSON formatter; `RUST_LOG` honored.
4. Test: scrape `/metrics` after a mixed workload, assert label sets and non-zero counters.

**Exit criteria.** Prometheus scrape returns a stable, low-cardinality series set matching DESIGN.

## Milestone 7 — Container, deploy, and config plumbing

**Goal.** Immutable, auditable container.

1. Dockerfile: cargo-chef recipe → builder → `gcr.io/distroless/cc-debian12:nonroot` (or static) final stage. Non-root UID, read-only root FS.
2. Image labels: `org.opencontainers.image.revision`, `list_version` (the LDNOOBW SHA).
3. k8s manifests under `deploy/k8s/`: Deployment, Service, HPA (CPU + `bws_inflight` via custom metric adapter — stubbed), liveness → `/healthz`, readiness → `/readyz`.
4. `README` snippet: env-var table mirrored from DESIGN (single source of truth kept in DESIGN; README links there).

**Exit criteria.** `docker run` locally serves `/v1/check` end-to-end; image size under 30 MB.

## Milestone 8 — Benchmarks and CI perf gates

**Goal.** Regressions fail CI.

1. Criterion benches in `benches/`:
   - 1 KiB reference input, English, strict vs substring.
   - 1 KiB input, all languages scanned.
   - 64 KiB input, English only.
   - Normalization-heavy input (fullwidth + NFKC expansions).
2. CI job runs benches against main and PR, fails if p99 regresses > 10%. Use `critcmp` or a small harness.
3. Load test script (`oha` or `vegeta`) committed under `bench/load/`; target p99 < 1 ms on the 1 KiB reference input, single core.

**Exit criteria.** A release-candidate tag produces a bench report checked into the PR description.

## Milestone 9 — v1.0 tag

**Goal.** Ship.

1. Fresh clone + `make docker` reproduces an identical image (modulo timestamps).
2. Load test report attached to the release notes.
3. `X-List-Version` in every response matches the git tag's submodule SHA.
4. Tag `v1.0.0`; cut image `ghcr.io/.../banned-words-service:v1.0.0` and `:$LIST_SHA`.

## Out of scope (tracked, not built)

- Per-tenant overrides — schema already accepts `overrides`; semantics land in v2.
- Leetspeak / homoglyph normalization.
- Multi-tenant rate limiting (belongs in gateway).
- Hot reload of the list (deliberately never).

## Open questions to resolve during M1–M2

- Exact LDNOOBW submodule SHA to pin (pick the latest commit at scaffold time; record in `build.rs` and PR description).
- Which CJK segmentation crate to depend on, if any, for stricter substring variants in v2 (none needed for v1 — `substring` is the default).
- Whether to expose a `bws_scan_bytes_total{lang}` counter; deferred unless a dashboard needs it.
