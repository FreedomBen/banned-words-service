# Vocab Veto — Implementation Plan

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
├── Makefile                       # build/test/lint/docker/run/help (M7)
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
├── tests/                         # integration tests (Router::oneshot)
├── benches/                       # criterion benches
└── deploy/
    ├── Dockerfile                 # cargo-chef + distroless
    └── k8s/                       # DOKS manifests — ClusterIP only, no Ingress in v1
        ├── deployment.yaml
        ├── service.yaml           # type: ClusterIP, port 8080
        ├── configmap.yaml         # BWS_LANGS, BWS_MAX_INFLIGHT, BWS_LISTEN_ADDR
        ├── secret.example.yaml    # BWS_API_KEYS template; real Secret managed out-of-repo
        └── hpa.yaml               # CPU + bws_inflight (custom-metrics adapter stubbed)
```

## Milestone 1 — Scaffold and build-time codegen

**Goal.** Empty binary that loads the LDNOOBW list at compile time.

1. `cargo init --bin`; commit `Cargo.toml` skeleton, workspace-free.
2. Add submodule: `git submodule add https://github.com/LDNOOBW/List-of-Dirty-Naughty-Obscene-and-Otherwise-Bad-Words vendor/ldnoobw`; pin to `5faf2ba42d7b1c0977169ec3611df25a3c08eb13` (LDNOOBW default-branch HEAD as of scaffold) and surface the SHA as `LIST_VERSION` in the generated terms file. Re-pinning later is a deliberate, release-worthy act — not routine maintenance.
3. `build.rs`:
   - Walk `vendor/ldnoobw/`. Every file there must fall into exactly one of two disjoint sets: the **language allowlist** — the 27 codes keyed in `DEFAULT_MODE` (see M3 item 4), each producing a term table — or the **explicit skip list** — `fr-CA-u-sd-caqc` (regional variant redundant with `fr`; the only BCP-47-tagged file at the pinned SHA) plus non-language files (`README.md`, `LICENSE`, `USERS.md`). Fail the build if any file matches neither set, or if an allowlisted code has no file — catches LDNOOBW drift and forces an explicit default-mode decision for any new language.
   - Emit the generated file to `$OUT_DIR/generated_terms.rs` (pulled in from `src/matcher/mod.rs` via `include!`). Never write into the source tree — that dirties the working copy, races cargo's rerun detection, and breaks the reproducible-build claim in M9. The file contains:
     - `pub const LIST_VERSION: &str = "<SHA>";`
     - `pub static TERMS: phf::Map<&'static str, &'static [&'static str]>` keyed by lowercase ISO code.
   - Emit `cargo:rerun-if-changed=vendor/ldnoobw` and `cargo:rerun-if-changed=.git/modules/vendor/ldnoobw/HEAD` so a submodule pin change actually triggers rerun.
4. Hello-world `main.rs` that prints `LIST_VERSION` and term counts per language. Smoke-test: `cargo run` prints something plausible.

**Exit criteria.** `cargo build` green; `LIST_VERSION == "5faf2ba42d7b1c0977169ec3611df25a3c08eb13"`; term counts sum to 2656 across 27 languages (the actual total at the pinned SHA, after whitespace-trim and empty-line skip).

## Milestone 2 — Matching core (library)

**Goal.** A pure-Rust matching engine, unit-tested in isolation from HTTP.

1. `matcher::normalize`:
   - NFKC via `unicode-normalization`, lowercased via `caseless`.
   - Returns `(String normalized, Vec<u32> offset_map)` where `offset_map[i]` is the starting byte offset in the original text for normalized-byte `i`. Single-pass.
   - Reject on length: normalized > 192 KiB → caller translates to 413.
2. `matcher::boundary`: `is_word_boundary(s: &str, byte_idx: usize) -> bool` per UAX #29, using `unicode-segmentation`.
3. `matcher::scan`:
   - `Engine::new(langs: &HashMap<Lang, &[&str]>) -> Engine` builds one `AhoCorasick` per lang with `MatchKind::LeftmostLongest`, non-overlapping.
   - `Engine::scan(text: &str, langs: &[Lang], mode: Option<Mode>) -> ScanResult { mode_used, matches, truncated }`. `mode = Some(m)` applies `m` uniformly to every scanned language (including CJK — no clamping) and echoes `m` in `mode_used` for each; `mode = None` looks each lang up in the `DEFAULT_MODE` table (populated in M3) and echoes the resolved value. `mode_used` always has one entry per scanned language.
   - Both modes share the same per-language `AhoCorasick`; strict mode is a **post-match boundary filter** over the hits produced by the shared automaton, not a second automaton. Keeps hot-path memory flat regardless of which mode a request picks.
   - Span widening across NFKC expansions as specified in DESIGN §"Mapping across NFKC expansions".
   - 256-match cap applied *after* concatenation in caller-supplied `langs` order (alphabetical when omitted).
4. Unit tests covering: ASCII strict vs substring, fullwidth evasion, ligature expansion (`ﬁ`), asymmetric NFKC widening (a match spanning multiple source codepoints where only one edge expands — directly exercises DESIGN §"Mapping across NFKC expansions"'s "widening applies independently at each edge" rule), CJK substring, Scunthorpe case, truncation boundary at exactly 256 and 257 hits. Empty-text rejection lives at the handler (DESIGN §"text — string, required" — the ≥1-byte check runs on raw input before normalization) and is covered by the M3 integration tests, not here.

**Exit criteria.** `cargo test --lib` green; criterion bench skeleton compiles.

## Milestone 3 — HTTP surface (happy path)

**Goal.** `/v1/check` end-to-end for a single language (`en`), `/v1/languages`, `/healthz`, `/readyz`.

1. `config.rs`: figment loads **TOML first, then env** — env wins on overlap. TOML lives at `/etc/bws/config.toml`, overridable via `BWS_CONFIG_FILE`; the default path being absent is not an error (Config is built from env alone), but a `BWS_CONFIG_FILE` pointing at a missing path **is** fatal — explicit operator intent shouldn't silently fall back. TOML keys are the lowercase env-var names without the `BWS_` prefix (`listen_addr`, `api_keys`, `langs`, `max_inflight`); array-valued keys (`api_keys`, `langs`) are TOML arrays rather than comma-separated strings. Post-parse rules (whitespace trim, empty-entry rejection, dedup, ASCII-lowercase for `langs`, short-key warning for `api_keys`) apply identically to both sources; the comma-split step applies to env-string forms only.
   - `BWS_LISTEN_ADDR`: HTTP listen address. Defaults to `0.0.0.0:8080` when unset — matches DESIGN §Deployment, and keeps `cargo run` and local Docker usage working with only `BWS_API_KEYS` set.
   - `BWS_API_KEYS`: **required**. Parse per DESIGN §Deployment — split on `,`, trim surrounding ASCII whitespace from each entry, reject empty entries, deduplicate; warn (do not reject) on entries shorter than 32 bytes. Unset / empty / zero-keys after parsing is a fatal startup error with a clear message. No comma-containing-entry check: the split guarantees entries are comma-free, and DESIGN §Deployment frames "keys cannot contain `,`" as operator guidance, not a runtime validation.
   - `BWS_LANGS`: optional runtime allowlist. Parse per DESIGN §Deployment — split on `,`, trim surrounding ASCII whitespace, ASCII-lowercase, reject empty entries, deduplicate. Defaults to every compiled language. Unknown-code rejection lands in M4; parsing rules apply from M3.
   - `BWS_MAX_INFLIGHT`: default `1024`.
   Config unit tests cover each `BWS_API_KEYS` rule independently (whitespace trim, empty-entry rejection, dedup, short-key warning emission, zero-keys fatal) plus source-precedence cases: env overrides TOML per key; absent default TOML path yields the same Config as env-only; `BWS_CONFIG_FILE` pointing at a non-existent path is fatal.
2. `auth.rs`: extract `Authorization: Bearer <k>`, compare each candidate via `subtle::ConstantTimeEq`, **always iterating the full set**. Log `key_id = hex(sha256(key))[..8]` on success; log only `reason` on failure.
3. `error.rs`: single `ApiError` enum → `IntoResponse` producing `{error, message}` with the right status. `X-List-Version` attachment is **not** done here — it lives in a response-layer middleware scoped to the `/v1` sub-router (see item 8), so `/healthz`, `/readyz`, `/metrics` do not carry the header while every `/v1/*` response (success, 4xx including fast-pathed 401, and 5xx) does.
4. `matcher::DEFAULT_MODE: phf::Map<&str, Mode>` — concrete 27-entry table keyed by the LDNOOBW codes shipped at the pinned SHA:
   - **`Substring`** (4): `ja`, `ko`, `th`, `zh` — scripts without reliable inter-word spaces, so UAX #29 boundaries under-match.
   - **`Strict`** (23): `ar`, `cs`, `da`, `de`, `en`, `eo`, `es`, `fa`, `fi`, `fil`, `fr`, `hi`, `hu`, `it`, `kab`, `nl`, `no`, `pl`, `pt`, `ru`, `sv`, `tlh`, `tr`.

   Full table lands here (pulled forward from M4) even though only `en` is actively loaded in M3, so `routes/languages.rs` can serve its canonical shape from day one. M4 then adds languages to the automaton map without churning the `/v1/languages` response contract. Build-time drift (a new LDNOOBW language with no `DEFAULT_MODE` entry, or an entry with no vendored file) is caught by the allowlist check in M1 item 3.
5. `routes/check.rs`: deserialize `CheckRequest` — `mode` is typed as `Option<String>` in the DTO so an unrecognized value reaches the handler instead of aborting at serde with a generic `400 bad_request`; the handler then validates the string against `"strict"`/`"substring"` and returns **422 `invalid_mode`** on mismatch, keeping that row distinct from the malformed-JSON `400` rail. Raw-text ≥1-byte check (→ **422 `empty_text`** when zero bytes — DESIGN §"text — string, required"). ASCII-lowercase each `langs` entry before membership check so `"EN"` and `"en"` are equivalent (DESIGN §"POST /v1/check"). Invoke `matcher::normalize` on the raw text and translate its length-exceeded signal to **413 `payload_too_large`** via `ApiError::PayloadTooLarge → IntoResponse` — this enforces the post-normalization 192 KiB cap, complementing the 64 KiB raw-body cap applied by middleware (item 8). Call `Engine::scan`; serialize `CheckResponse`. `mode_used` populated for every requested language.
6. `routes/languages.rs`: response from the compiled table in alphabetical order by ISO code, shape `{"languages": [{code, default_mode}, ...]}` (DESIGN §"Other endpoints"), restricted to languages currently in the automaton map. `default_mode` is sourced from `matcher::DEFAULT_MODE`.
7. `routes/health.rs`: `/healthz` always returns 200. `/readyz` returns 200 with `{ "ready": true, "list_version": "<SHA>", "languages": N }` once all automatons are built, else 503 with `{ "ready": false }`. The listener binds only *after* the engine is ready, so the 503 state is essentially unobservable in practice — still implemented for correctness and for operators inspecting a sidecar that races startup.
8. Middleware stack, ordered outermost → innermost (first to see the request first): request-id → `tracing` span → RED metrics layer (M6; applied globally so `/v1/*`, `/healthz`, `/readyz`, and `/metrics` all register in the RED series). Routing then splits: `/healthz`, `/readyz`, and `/metrics` pass directly into their handlers with no auth, no `X-List-Version`, and no body cap. Every `/v1/*` request additionally traverses, in request order: auth (fast 401 before body work) → raw body-size limit (64 KiB, `tower_http::limit::RequestBodyLimitLayer`; its default plain 413 body is remapped by a companion response layer to the `{error: "payload_too_large", message}` shape so every `/v1/*` 4xx matches DESIGN §API error table) → handler. The in-flight gate (M5) is mounted on the `/v1/check` sub-route specifically, not on the `/v1/*` chain — so `/v1/languages` does not pass through it (matching DESIGN §Deployment's `BWS_MAX_INFLIGHT` scope, which names `/v1/check` only). Wrapping the `/v1/*` chain is an `X-List-Version` response-side layer that attaches the header to every outbound response — success, fast-path 401, or any error — without touching the request path. M3 lands this stack minus the RED layer (M6) and the in-flight gate (M5). This ordering realises the DESIGN invariants that 401 runs before body parse and before the gate, that fast-pathed 401s still carry `X-List-Version` and still increment the RED series, and that `/healthz`/`/readyz`/`/metrics` remain unauthenticated per DESIGN §Authentication.
9. Integration tests via `axum::Router::oneshot` (exercise auth on both `/v1/check` and `/v1/languages` since both share the `/v1/*` auth layer per DESIGN §Authentication): auth missing/invalid/valid — each 401 response asserts `X-List-Version` is attached (fast-path traverses the response-attach layer per DESIGN §"POST /v1/check"), body too large, malformed JSON, missing `text` field (→400 `bad_request`), empty `text` (→422 `empty_text`), whitespace-only `text` accepted (DESIGN §"text — string, required"), empty `langs` (→422 `empty_langs`), unknown language (→422 `unknown_language`), case-folded `langs` entries (`"EN"` ≡ `"en"`), `/v1/languages` response-shape contract, `/healthz` 200, `/readyz` 200 with full body shape (`ready: true`, `list_version` matches `LIST_VERSION`, `languages` > 0; the 503 pre-ready branch is covered by a handler-level unit test since the listener binds only after readiness flips — DESIGN §"Other endpoints"), happy path.

**Exit criteria.** `curl -H "Authorization: Bearer $K" -d '{"text":"..."}' :8080/v1/check` returns the documented shape.

## Milestone 4 — Multi-language and mode defaults

**Goal.** All LDNOOBW languages loaded; per-language mode default table wired up.

1. Load all LDNOOBW languages (subject to `BWS_LANGS` in the next item) into the automaton map at startup; M3 ran with only `en`, and `DEFAULT_MODE` is already in place from M3.
2. `langs` defaulting: when omitted, scan every loaded language in alphabetical order.
3. `mode` defaulting: per-language lookup via `matcher::DEFAULT_MODE`, echoed in `mode_used`. Explicit caller mode wins, including `strict` on CJK (no clamping).
4. `BWS_LANGS` runtime allowlist: fatal startup error on unknown codes, with a helpful message listing compiled codes.
5. Tests: mixed-language request, default vs explicit mode parity, CJK-strict honored, `BWS_LANGS` trimming, ASCII-lowercasing, and dedup.

**Exit criteria.** A single request across `en,ja,zh` returns a well-formed `mode_used` map and correctly-ordered matches.

## Milestone 5 — Limits, backpressure, and error surface

**Goal.** Every documented error code is reachable by a test.

1. In-flight cap: a tower layer backed by `Arc<AtomicUsize>` gating `/v1/check` only. Excludes `/healthz`, `/readyz`, `/metrics`, and 401-fast-path rejections (auth runs *before* the gate).
2. 413 rails are already in place by end of M3 — raw-body 64 KiB via `tower_http::limit::RequestBodyLimitLayer` (M3 item 8) and post-normalization 192 KiB via `ApiError::PayloadTooLarge` in `routes/check.rs` (M3 item 5). M5 adds no 413 implementation; only the post-normalization test in item 5 below.
3. 503 `overloaded` returns immediately when the gate is full.
4. Unknown-fields pass-through confirmed by test (including the reserved `overrides` key).
5. Error-table tests for the rows M3 item 9 didn't cover: `422 invalid_mode`, `413 payload_too_large` (post-normalization path, via the 192 KiB cap in item 2), `503 overloaded` (via the in-flight gate in item 1), and `500 internal`. The `500 internal` row is covered by a unit test on `ApiError::Internal.into_response()` asserting status, `{error: "internal", message}` body shape, and no leaked diagnostic detail — end-to-end triggering isn't worth a test-only code path. Rows already asserted in M3 (`401 unauthorized`, `400 bad_request`, `413 payload_too_large` raw-body, `422 empty_text`, `422 empty_langs`, `422 unknown_language`) are not re-tested here.

**Exit criteria.** All documented 4xx/5xx paths have a test; `X-List-Version` present on every `/v1/*` response including errors.

## Milestone 6 — Observability

**Goal.** `/metrics` exposes the DESIGN §"Metrics contract" series with correct labels.

1. RED pair via `axum-prometheus` (committed in DESIGN §Tech stack), named per DESIGN §"Metrics contract":
   - `bws_requests_total{status}` counter, with `status` bucketed as `2xx` / `4xx` / `5xx`.
   - `bws_request_duration_seconds{status,endpoint}` histogram; `endpoint` ∈ {`/v1/check`, `/v1/languages`, `/healthz`, `/readyz`, `/metrics`}.
   Bucket boundaries come from the `axum-prometheus` sub-millisecond preset by default; `BWS_HISTOGRAM_BUCKETS` (optional; comma-separated ascending floats in seconds) overrides them for **both** `bws_request_duration_seconds` and `bws_match_duration_seconds`. Parse errors (non-float entries, non-ascending order, empty list) are a fatal startup error, unit-tested in `observability.rs`. The RED layer must sit **outside** the auth layer (see M3 middleware order) so fast-pathed 401s flow through it — DESIGN explicitly requires them to increment both `bws_requests_total{status="4xx"}` and `bws_request_duration_seconds`, in addition to `bws_auth_failures_total`.
2. Custom metrics registered in `observability.rs`:
   - `bws_auth_failures_total{reason}`, `reason` ∈ {`missing`, `invalid`}.
   - `bws_match_duration_seconds{lang,mode}` — observed inside `scan` per lang.
   - `bws_matches_per_request`, `bws_truncated_total`, `bws_input_bytes`.
   - `bws_list_version_info{list_version}` set to 1 at startup.
   - `bws_languages_loaded` gauge, `bws_inflight` gauge (observes the same `Arc<AtomicUsize>` the M5 gate increments and `BWS_MAX_INFLIGHT` caps).
3. `tracing-subscriber` with JSON formatter; `RUST_LOG` honored.
4. Test: scrape `/metrics` after a mixed workload, assert label sets and non-zero counters.

**Exit criteria.** Prometheus scrape returns a stable, low-cardinality series set matching DESIGN.

## Milestone 7 — Container, deploy, and config plumbing

**Goal.** Immutable, auditable container.

1. Dockerfile: cargo-chef recipe → builder targeting `x86_64-unknown-linux-musl` → `gcr.io/distroless/static-debian12:nonroot` final stage (aligns with DESIGN §Tech stack's "distroless static image" commitment). Non-root UID, read-only root FS.
2. Image labels: `org.opencontainers.image.revision`, `list_version` (the LDNOOBW SHA).
3. k8s manifests under `deploy/k8s/`, targeting DOKS (existing cluster; cluster provisioning out of scope). Per DESIGN §"Kubernetes deployment (DOKS)":
   - `Deployment` with non-root UID, `readOnlyRootFilesystem: true`, `allowPrivilegeEscalation: false`, all capabilities dropped, `seccompProfile: RuntimeDefault`. Standard `RollingUpdate` strategy.
   - `Service` of type **`ClusterIP`** on port 8080. **No Ingress, no LoadBalancer, no NodePort** — v1 is in-cluster only; consumers reach the service at `banned-words-service.<namespace>.svc.cluster.local:8080`.
   - `Secret` holding `BWS_API_KEYS` (template only — real Secret managed out-of-repo); `ConfigMap` holding `BWS_LANGS`, `BWS_MAX_INFLIGHT`, `BWS_LISTEN_ADDR`; both mounted as env vars.
   - `livenessProbe` → `GET /healthz`; `readinessProbe` → `GET /readyz`. Neither authenticated (per DESIGN §Authentication).
   - `HPA` on CPU + `bws_inflight` via custom-metrics adapter — stubbed; concrete adapter wiring is the operator's responsibility (DOKS does not preinstall one).
   - Namespace not hardcoded in manifests; operator selects via `-n` / kustomize overlay.
   - Resource requests/limits left as placeholders until M8 load-test data is in; memory ceiling is `BWS_MAX_INFLIGHT × ~256 KiB` per DESIGN §Deployment.
4. `README` snippet: env-var table mirrored from DESIGN (single source of truth kept in DESIGN; README links there).
5. Root `Makefile` (default `PREFIX=/usr/local` per global convention) with targets: `help` (default; lists the targets with one-line descriptions), `build` (`cargo build --release`), `test` (`cargo test`), `bench` (`cargo bench --no-run`; the same invocation CI runs per the Conventions section), `lint` (`cargo fmt --check && cargo clippy -- -D warnings`), `docker` (build the container image, tagged with the LDNOOBW SHA), and `run` (`cargo run` with a dev-only `BWS_API_KEYS`). M9's `make docker` exit criterion depends on this target existing.

**Exit criteria.** `docker run` locally serves `/v1/check` end-to-end; image size under 30 MB.

## Milestone 8 — Benchmarks and CI perf gates

**Goal.** Regressions fail CI.

1. Criterion benches in `benches/`:
   - 1 KiB reference input, English, strict vs substring.
   - 1 KiB input, all languages scanned.
   - 64 KiB input, English only.
   - Normalization-heavy input (fullwidth + NFKC expansions).
2. CI job runs benches against main and PR, fails if p99 regresses > 10%. Use `critcmp` or a small harness.
3. Load test script (`oha` or `vegeta`) committed under `benches/load/` (same root as the criterion benches; cargo ignores non-`.rs` files there); target p99 < 1 ms on the 1 KiB reference input, single core.

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
- Stricter substring matching for CJK/Thai via a segmentation crate (`lindera` or similar for CJK; a dedicated Thai segmenter for Thai) — revisit only if a caller explicitly asks for it. v1's default is `substring` for these scripts, and explicit `strict` is honored but under-matches by design; segmentation-crate dictionaries are multi-megabyte and would bloat the image for a feature no v1 caller has asked for.
- Per-language scan-bytes counter (`bws_scan_bytes_total{lang}`) — existing `bws_match_duration_seconds{lang,mode}` and `bws_input_bytes` cover per-language hot-path cost and aggregate throughput; add only if a dashboard needs absolute byte counts per language that can't be derived from existing series.
