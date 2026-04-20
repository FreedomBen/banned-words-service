# Banned Words Service — Design

## Goal

A high-throughput, low-latency HTTP service that answers: *"Does this string contain a banned word?"* across multiple languages. The authoritative word list is sourced from [LDNOOBW](https://github.com/LDNOOBW/List-of-Dirty-Naughty-Obscene-and-Otherwise-Bad-Words).

Target: p99 latency < 1 ms for a 1 KiB reference input on a single core; tens of thousands of RPS per instance. Larger inputs (up to the 64 KiB body limit; see API) scale linearly in the Aho-Corasick scan — the p99 target applies to the reference size, not the hard limit.

## Non-goals

- Semantic moderation, toxicity scoring, or ML-based classification.
- Admin UI for editing the list (the list is baked in at build or startup).

## Design principles

1. **Fully stateless, zero external dependencies.** No database, no Redis, no disk reads on the hot path. The entire word list is compiled into the binary via `build.rs` and loaded into Aho-Corasick automatons at startup. Pods are fungible — kill one, start another, identical state in milliseconds. Horizontal scaling is `replicas: N`.
2. **Immutable at runtime.** The word list only changes via redeploy. The image tag *is* the list version. This keeps the hot path lock-free and makes the running version trivially auditable.
3. **Hot path allocations are bounded.** One `Arc<AhoCorasick>` per language, shared across all request tasks. No per-request automaton construction. Per-request allocations are limited to JSON parser buffers, one offset map (`Vec<u32>` sized to the normalized text), and up to 256 short `String`s for `matched_text`. No unbounded or list-size-dependent allocation on the hot path.
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

1. Vendor the repo as a git submodule pinned at a specific commit SHA. That SHA is the authoritative list version surfaced via `X-List-Version`, `/readyz`, and the `bws_list_version_info` metric; reproducible builds depend on it.
2. Generate a `phf`-backed map `lang -> &[&str]` via `build.rs` (compile-time, zero-alloc lookup into the term tables).
3. At startup, build one `AhoCorasick` automaton per language and store them in an `Arc<HashMap<Lang, AhoCorasick>>`. The automaton map uses `HashMap` rather than `phf` because `AhoCorasick` is not const-constructible; it is populated once at startup and read-only thereafter. Automatons are immutable and shared across all request tasks.

Memory footprint estimate: LDNOOBW is ~5k terms total across languages; the combined DFAs are well under 10 MiB.

## Matching semantics

Both **strict** (whole-word) and **substring** matching are first-class in v1. Callers pick per request via the `mode` field; when `mode` is omitted, the server applies the per-language default listed below.

- **Normalization.** Input is NFKC-normalized, then lowercased via `caseless`, in both modes. NFKC (vs NFC) is chosen deliberately — it folds compatibility forms (fullwidth, ligatures, superscripts) that are otherwise trivial evasion vectors.
- **`mode: "strict"`.** A term matches only when both edges fall on a Unicode word boundary per UAX #29. Mitigates the common **Scunthorpe problem** (banned term appearing as a substring of a safe word) for space-delimited languages. Known residual cases not handled in v1: punctuation- or hyphen-joined obfuscations (e.g., `s-h-i-t`), zero-width-joiner insertions, and leetspeak substitutions. See "Deferred to v2".
- **`mode: "substring"`.** Any Aho-Corasick hit counts. Appropriate for scripts without reliable inter-word spaces (CJK, Thai) where UAX #29 boundaries under-match, and for callers who explicitly want aggressive matching.
- **Per-language default** (applied when `mode` is omitted): `strict` for space-delimited languages (en, es, fr, de, pt, it, nl, ru, …); `substring` for CJK and Thai (ja, ko, th, zh) — scripts without reliable inter-word spaces. The mode chosen per language is echoed back in `mode_used` so callers can audit. See IMPLEMENTATION_PLAN.md M3 item 4 for the concrete per-code table.
- **Explicit caller mode wins.** If a caller explicitly sends `mode: "strict"` with a substring-default language (CJK or Thai), the service honors it without rejection or silent clamping — UAX #29 boundaries under-match in practice on those scripts, but the caller's intent is preserved and `mode_used` reflects `"strict"`. The echo-back in `mode_used` is the audit trail; we do not second-guess an explicit choice.
- **Span semantics.** Match `start`/`end` are byte offsets into the caller's **original** request text, suitable for direct slicing (`text[start:end]`) for redaction or highlighting. The normalizer maintains an offset map alongside the normalized buffer in a single pass, and matches are translated back before serialization. Cost: one extra `Vec<u32>` allocation per request, dwarfed by JSON parsing.
- **Mapping across NFKC expansions.** When one source codepoint expands to multiple normalized codepoints (ligatures like `ﬁ` → `fi`, compatibility forms, fullwidth letters), a match covering any portion of the expanded output maps back to the **entire source codepoint's byte range**. For matches spanning multiple source codepoints, widening applies independently at each edge: the reported span is `[start-byte of the first source codepoint any part of whose expansion is touched, end-byte of the last such source codepoint]`. This is intentionally conservative: it guarantees `text[start:end]` always contains the offending content in full, at the cost of occasionally widening the reported span beyond the minimum. Spans are always on UTF-8 codepoint boundaries in the original text.

Both modes share the same automaton; the difference is a post-match boundary check, so there is no meaningful perf gap between them.

## API

### Authentication

All `/v1/*` endpoints require an API key presented as:

```
Authorization: Bearer <api-key>
```

Each candidate key in `BWS_API_KEYS` is compared to the presented key via `subtle::ConstantTimeEq`; the service iterates the full configured set regardless of where a match occurs, so the loop leaks only the configured key-count via timing, never key material. Missing or unrecognized keys return **401 `unauthorized`** before any body parse or matching work — unauthenticated traffic pays ~zero CPU. `/healthz`, `/readyz`, and `/metrics` are deliberately **not** auth-gated so Kubernetes probes and Prometheus scrapers work without key provisioning; those endpoints should be reachable only from the cluster/pod network.

Request logs record only `key_id` — the first 8 hex characters of the key's SHA-256 — never the key itself. This is enough to correlate traffic to a specific caller and to track rotation without leaking secrets into log aggregation. On 401, logs record only the failure `reason` (`missing` or `invalid`) — never the attempted key nor any hash prefix derived from it, so the endpoint cannot be used as an oracle to probe the key space.

`/v1/*` auth is uniform: `/v1/check` **and** `/v1/languages` both require a valid key. There is no metadata carve-out — the rule "every `/v1/*` request is authenticated" is easier to reason about and audit than any exemption would be.

### POST /v1/check

Request body (JSON, max 64 KiB — 413 `payload_too_large` above that; additionally, inputs whose NFKC-normalized form exceeds 192 KiB are rejected with the same 413, since Unicode compatibility expansion can grow text by up to ~3×):

```json
{
  "text": "some user input",
  "langs": ["en", "es"],
  "mode": "strict"
}
```

- `text` — string, required. Must be byte-length ≥ 1; whitespace-only is accepted (the emptiness check runs on the raw input before normalization).
- `langs` — array of loaded language codes, optional. Defaults to every loaded language. Codes are lowercase ASCII (ISO 639-1 where available, e.g. `en`, `ja`); inputs are case-folded before lookup so `"EN"` and `"en"` are equivalent. An empty array (`[]`) is rejected with 422 `empty_langs` — to scan every loaded language, omit the field rather than send `[]`, so the "none" vs. "all" distinction stays explicit.
- `mode` — `"strict"` | `"substring"`, optional. Omit to apply the per-language default.

**Forward compatibility.** Unknown top-level request fields are accepted and silently ignored (serde's `deny_unknown_fields` is deliberately **not** set). This lets v2 extend the schema without breaking v1 clients. The `overrides` key is reserved for future per-tenant allow/denylists; see "Deferred to v2".

Success response (200):

```json
{
  "banned": true,
  "mode_used": { "en": "strict", "ja": "substring" },
  "matches": [
    { "lang": "en", "term": "****", "matched_text": "****", "start": 12, "end": 16 }
  ],
  "truncated": false
}
```

- Each match carries both `term` — the canonical dictionary entry from LDNOOBW, stable across requests and useful for grouping, metrics, and deduplication — and `matched_text`, the exact slice of the caller's **original** input (`text[start:end]`). The two differ after NFKC folding and case folding: e.g. a term listed as `idiot` may surface with `matched_text: "Ｉｄｉｏｔ"`. `term` is a `&'static str` from the compiled table (zero cost); `matched_text` is a short heap allocation per match and is bounded by the 256-match cap. (The `****` placeholders in the example response above are documentation-side redaction, not service behavior — responses always contain the literal matched content.)
- When `banned: false`, `matches` is `[]`. On any 200 response, `mode_used` is populated with an entry for every requested (or defaulted) language; error responses (4xx/5xx) use the `{error, message}` shape and do not carry `mode_used`.
- Every `/v1/*` response — both `/v1/check` and `/v1/languages`, on success, 4xx (including fast-pathed 401), and 5xx — carries an `X-List-Version: <LDNOOBW commit SHA>` header. This lets each decision be audited against a specific list version without a round trip to `/readyz`, and survives proxies that rewrite bodies. The header is static per process (constant from startup), so attaching it on the 401 fast path is free.
- **Match ordering.** Within each language, matches are leftmost-longest and non-overlapping. Per-language groups are then concatenated in the order the caller supplied `langs` (or, when `langs` is omitted, alphabetical order by ISO code — the same order echoed by `/v1/languages`). All scanned languages contribute their full match set before truncation is applied; the cap does **not** short-circuit the scan. The concatenated list is then capped at the **first 256 in that order**; if more were found, `truncated` is `true` and the response is a strict prefix of the full list. This bounds response size on pathological input and keeps the truncation point deterministic under a fixed request.

Error response (4xx):

```json
{ "error": "invalid_mode", "message": "mode must be 'strict' or 'substring'" }
```

| HTTP | `error`             | Condition                                                    |
| ---- | ------------------- | ------------------------------------------------------------ |
| 401  | `unauthorized`      | Missing `Authorization` header, or key not in `BWS_API_KEYS`. |
| 400  | `bad_request`       | Malformed JSON, or `text` field missing entirely.            |
| 413  | `payload_too_large` | Request body exceeds 64 KiB.                                 |
| 422  | `invalid_mode`      | `mode` is present but not in the enum.                       |
| 422  | `unknown_language`  | A `langs` entry is not a loaded language code.               |
| 422  | `empty_text`        | `text` is present but empty (`""`).                          |
| 422  | `empty_langs`       | `langs` is present but empty (`[]`).                         |
| 500  | `internal`          | Unexpected server error. `message` is a short, fixed string; diagnostic detail goes to structured logs, never the response body. |
| 503  | `overloaded`        | In-flight request count is at `BWS_MAX_INFLIGHT`. Backpressure signal; retry with jitter. |

### Other endpoints

- `GET /healthz` — liveness.
- `GET /readyz` — readiness. Returns **200** with `{ "ready": true, "list_version": "<LDNOOBW commit SHA>", "languages": N }` once all automatons are built; returns **503** with `{ "ready": false }` during the startup window before automatons are live. The HTTP listener is bound only after automatons finish loading, so in practice the 503 state is observable only by a sidecar that races startup. The body lets operators and callers audit the exact list version the running binary was built against.
- `GET /metrics` — Prometheus scrape.
- `GET /v1/languages` — list of loaded language codes and each one's default mode, in alphabetical order by ISO code. This is the canonical order used for match concatenation when `langs` is omitted on `/v1/check`.

  ```json
  {
    "languages": [
      { "code": "de", "default_mode": "strict" },
      { "code": "en", "default_mode": "strict" },
      { "code": "ja", "default_mode": "substring" }
    ]
  }
  ```

### Metrics contract

`/metrics` exposes the Prometheus series below. Label cardinality is bounded by (languages loaded × mode × status × endpoint) across metrics, so total series count stays in the low hundreds across a realistic deployment.

| Metric                         | Type      | Labels                   | Purpose                                                                  |
| ------------------------------ | --------- | ------------------------ | ------------------------------------------------------------------------ |
| `bws_requests_total`           | counter   | `status` (2xx / 4xx / 5xx) | Request rate and error ratio — the R and E of RED. Fast-pathed 401s increment this under `status="4xx"` in addition to `bws_auth_failures_total`. |
| `bws_auth_failures_total`      | counter   | `reason` (missing / invalid) | Auth rejection rate; a sudden spike indicates a rotated key or probing. |
| `bws_request_duration_seconds` | histogram | `status`, `endpoint`     | End-to-end latency including JSON parse and serialize. `endpoint` is one of `/v1/check`, `/v1/languages`, `/healthz`, `/readyz`, `/metrics`; fast-pathed 401s are recorded here too. |
| `bws_match_duration_seconds`   | histogram | `lang`, `mode`           | Aho-Corasick scan time per (lang, mode), observed once per scanned language per request; isolates the hot path from HTTP/JSON overhead. |
| `bws_matches_per_request`      | histogram | —                        | Distribution of match counts per request; informs tuning of the 256 cap. |
| `bws_truncated_total`          | counter   | —                        | Count of responses with `truncated: true`; expected rare, alert-worthy if not. |
| `bws_input_bytes`              | histogram | —                        | Distribution of `text` lengths in bytes; detects outlier traffic.        |
| `bws_list_version_info`        | gauge     | `list_version`           | Constant 1; lets dashboards and alerts pivot on list version across a pod fleet. Label value is the same LDNOOBW commit SHA surfaced in `X-List-Version` and `/readyz`. During a rolling deploy this label briefly takes two values (old + new); treat that as expected, not cardinality growth. |
| `bws_languages_loaded`         | gauge     | —                        | Count of automatons live after startup; sanity check for `BWS_LANGS`.    |
| `bws_inflight`                 | gauge     | —                        | Current in-flight `/v1/check` request count (counts against `BWS_MAX_INFLIGHT`). Lets dashboards answer "how close are we to the cap?" without inferring it from 503 rates. |

Histogram buckets default to the `axum-prometheus` preset tuned for sub-millisecond latency; override via env for deployments with different SLO targets.

### Why return match spans

Callers often want to redact or highlight, not just know the boolean. Returning spans costs almost nothing (Aho-Corasick produces them natively) and avoids a second round trip.

## Performance plan

- Single shared `Arc<AhoCorasick>` per language; no per-request allocation for the automaton.
- Scan cost scales `O(N_langs × input_len)` — each automaton is scanned independently over the full normalized input. When `langs` is unset, every loaded language is scanned, so callers who know their expected language(s) should specify `langs` explicitly to avoid paying for unused automatons.
- Automatons built with leftmost-longest, non-overlapping match kind — bounds match cardinality without extra filtering logic.
- Reuse a `Vec<Match>` buffer per task via a small object pool if profiling shows allocations dominate.
- Criterion benchmarks committed alongside the code; regressions fail CI.
- Load test with `oha` or `vegeta` against a representative corpus before each release.

## Deployment

- Single stateless binary, horizontally scalable.
- Container image built via `cargo chef` for fast layer caching.
- Config via env vars:
  - `BWS_LISTEN_ADDR` — HTTP listen address. Defaults to `0.0.0.0:8080` when unset.
  - `BWS_API_KEYS` — **required**, comma-separated list of accepted API keys (e.g. `k_prod_ab12…,k_prod_cd34…`). Parsing: split on `,`, trim surrounding ASCII whitespace from each entry, reject empty entries, deduplicate. Keys cannot themselves contain `,` — rotate to comma-free keys if the existing set has any. If unset, empty, or if parsing yields zero keys, the service **refuses to start** with a clear error — there is no open/anonymous mode. Keys should be ≥32 bytes of cryptographically random data; the service warns (does not reject) on shorter keys so legacy tokens can be rotated in gracefully. Rotation: redeploy with the union of old + new keys, wait for callers to cut over, then redeploy with only the new set — no hot reload.
  - `BWS_LANGS` — optional comma-separated runtime allowlist (e.g. `en,es,fr`). Defaults to every language compiled into the binary. Parsing: split on `,`, trim surrounding ASCII whitespace from each entry, ASCII-lowercase, reject empty entries, deduplicate. Useful for slimming a single fat image down to a specific deployment's needs without rebuilding. A code not compiled into the binary is a **fatal startup error** (`unknown language in BWS_LANGS: xx; compiled: ...`); silent drops would mask deploy-config typos and let a pod come up serving fewer languages than the operator intended.
  - `BWS_MAX_INFLIGHT` — optional cap on concurrent in-flight `/v1/check` requests. Default `1024`. When at capacity, new requests are rejected with **503 `overloaded`** before body parse, bounding worst-case in-flight memory to roughly `BWS_MAX_INFLIGHT × (64 KiB body + 192 KiB normalized buffer + offset map)` — NFKC compatibility expansion is capped at 3× via the normalized-length check, and requests that would exceed 192 KiB normalized are rejected with 413 before the scan runs. This is a liveness safeguard, not a rate limit; callers should retry with jitter. Excluded from the cap: `/healthz`, `/readyz`, `/metrics`, and 401-fast-path rejections (all of which do no bounded-size work).
  - *(No `BWS_DEFAULT_MODE` — mode defaulting is per-language and defined in code, not config, so behavior is identical across deployments.)*
- **List updates ship via redeploy.** No hot reload, ever — it keeps the hot path lock-free and makes the running version trivially auditable (image tag = list version).

### Kubernetes deployment (DOKS)

- **Target.** Digital Ocean Kubernetes Service (DOKS), deployed into an **existing** shared cluster. Cluster provisioning is out of scope; this assumes standard `kubectl` access and an in-cluster Prometheus stack that scrapes `/metrics`.
- **Exposure: in-cluster only in v1.** A single `Service` of type `ClusterIP` fronts the pods on port 8080. **No Ingress, LoadBalancer, or NodePort** — the service is not reachable from outside the cluster, and the API key is defense-in-depth over that network boundary, not the sole perimeter. In-cluster consumers reach it at `banned-words-service.<namespace>.svc.cluster.local:8080`. Public exposure (behind a rate-limiting gateway / WAF) is a deliberate future step; see "Deferred to v2".
- **Config delivery.** `BWS_API_KEYS` lives in a Kubernetes `Secret`; `BWS_LANGS`, `BWS_MAX_INFLIGHT`, and `BWS_LISTEN_ADDR` live in a `ConfigMap`. Both mount as env vars on the pod. Rotating keys is a `Secret` edit plus rolling restart — no hot reload, matching the "immutable at runtime" principle.
- **Probes.** `livenessProbe` → `GET /healthz`; `readinessProbe` → `GET /readyz`. Both are unauthenticated per API §Authentication. The listener binds only after automatons finish loading, so in practice readiness flips to 200 as soon as the pod accepts TCP; the documented 503 window is effectively unobservable except by a sidecar that races startup.
- **Pod security context.** Non-root UID, `readOnlyRootFilesystem: true`, `allowPrivilegeEscalation: false`, all capabilities dropped, `seccompProfile: RuntimeDefault`. The binary needs no writable paths (list is compiled in; logs go to stdout).
- **Rollout.** Standard `RollingUpdate`. Pods are fungible and cold-start in milliseconds (automaton build ≪ image pull), so low `maxUnavailable` with small `maxSurge` is cheap. During a rolling deploy `X-List-Version` and the `bws_list_version_info` gauge briefly take two values across the fleet — expected, not cardinality growth.
- **Scaling.** Horizontal via replicas; HPA on CPU plus `bws_inflight` (via the cluster's Prometheus custom-metrics adapter, assumed already installed). No VPA — memory footprint is flat and bounded by `BWS_MAX_INFLIGHT × ~256 KiB` worst case.

## Threat model and abuse posture

The service authenticates every `/v1/*` request against the key set in `BWS_API_KEYS` (see "Authentication" under API). It does **not** perform per-caller rate limiting, quotas, or request signing beyond that — those belong in the gateway. It is expected to sit behind an authenticated gateway or inside a trusted internal network (in v1 this means in-cluster only on DOKS — `ClusterIP` Service, no Ingress; see Deployment §"Kubernetes deployment (DOKS)"); the API key is a second line of defense, not the only one.

**In-process defenses:**

- **401 fast path.** Missing or invalid keys are rejected before body parse and before the concurrency-cap gate, so unauthenticated traffic cannot force the service to allocate a parser, run a scan, or consume an in-flight slot.
- **64 KiB raw body cap and 192 KiB NFKC-normalized cap** — 413 above either.
- **256-match response cap** — `truncated: true` above that.
- **Concurrency cap** (`BWS_MAX_INFLIGHT`, default 1024) — 503 `overloaded` above that. Bounds worst-case in-flight memory independent of gateway behavior.
- **Bounded per-request work.** The Aho-Corasick scan is O(n) in input length with a constant factor independent of list size; no input can force superlinear CPU or memory. The 256-match cap bounds response size and per-match allocations, not scan cost — the scan always traverses the full input regardless of how many hits it produces.
- **Constant-time key comparison** and **hash-prefix-only logging of authenticated requests** (no key-derived data on auth failure) prevent key-material leaks via timing or log exfiltration.

The in-process concurrency cap is a liveness safeguard, not a rate limit: it prevents memory blow-up under load spikes but does not distinguish a credentialed abuser from legitimate traffic. If the service is ever exposed directly to the public internet, a gateway-level rate limit and request-size policy must still be added — the API key alone does not defend against a credentialed abuser. Multi-tenant rate limiting is deferred to v2 and is expected to live in the gateway regardless (see below).

## Deferred to v2

- **Per-tenant allowlist / denylist overrides.** The v1 request schema will silently accept (and ignore) an `overrides` field, so adding real semantics later is non-breaking.
- **Leetspeak / homoglyph normalization.** Requires careful false-positive analysis before shipping.
- **Multi-tenant rate limiting.** Likely belongs in the gateway, not this service — revisit if that assumption breaks.
- **Public ingress.** v1 runs as a `ClusterIP` Service with no Ingress — in-cluster traffic only. Making the service externally reachable requires adding an Ingress or gateway with rate limiting and request-size policy in front; the API key alone is not a substitute (see Threat model).

## Milestones

1. Scaffold crate, vendor LDNOOBW, build-time codegen of term tables.
2. `/v1/check` end-to-end with both `strict` and `substring` modes for `en`.
3. All LDNOOBW languages loaded; per-language mode override.
4. Metrics, health checks, container image.
5. Criterion benches + CI perf gates.
6. Load test report + v1.0 tag.
