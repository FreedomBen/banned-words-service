# Vocab Veto API

HTTP API for the Vocab Veto banned-word detection service. This document is derived from [openapi.yaml](./openapi.yaml) — the YAML spec is authoritative; if the two disagree, the YAML wins. For behavioral rationale (normalization semantics, span mapping, threat model) see [DESIGN.md](./DESIGN.md).

- **Version:** `1.0.0`
- **Base URL (in-cluster):** `http://banned-words-service.<namespace>.svc.cluster.local:8080`
- **Content-Type:** `application/json` for all `/v1/*` endpoints
- **Transport:** HTTP/1.1 and HTTP/2, cleartext within the pod network. Public ingress is out of scope for v1.

## Table of contents

- [Authentication](#authentication)
- [Response headers](#response-headers)
- [Endpoints](#endpoints)
  - [POST /v1/check](#post-v1check)
  - [GET /v1/languages](#get-v1languages)
  - [GET /healthz](#get-healthz)
  - [GET /readyz](#get-readyz)
  - [GET /metrics](#get-metrics)
- [Schemas](#schemas)
- [Error responses](#error-responses)
- [Forward-compatibility notes](#forward-compatibility-notes)

## Authentication

All `/v1/*` endpoints require a bearer token:

```
Authorization: Bearer <api-key>
```

- Keys are compared against the configured `VV_API_KEYS` set via `subtle::ConstantTimeEq`; the comparison loop does **not** short-circuit, so timing is independent of which (if any) key matches.
- The service **refuses to start with no keys configured.** There is no anonymous mode.
- `/healthz`, `/readyz`, and `/metrics` are intentionally **unauthenticated** so Kubernetes probes and Prometheus scrapers work without key provisioning. These endpoints should be reachable only from the pod network in the documented deployment.
- A missing or non-matching key returns **401** on a fast path that runs before JSON parsing and before the in-flight gate. The 401 response carries both `X-List-Version` and a constant `WWW-Authenticate: Bearer` challenge (RFC 6750 §3, no `realm`/`scope`/`error` parameters — the body `{error, message}` carries that detail).

## Response headers

| Header            | Where                                                          | Description                                                                                                                  |
| ----------------- | -------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `X-List-Version`  | Every `/v1/*` response (including 401 and 5xx)                 | LDNOOBW commit SHA this binary was built against. Constant per process.                                                      |
| `WWW-Authenticate`| 401 responses                                                  | Constant value `Bearer`. No parameters.                                                                                      |
| `Content-Type`    | JSON responses / `/metrics`                                    | `application/json` for `/v1/*` and `/readyz`; `text/plain; version=0.0.4` for `/metrics`.                                    |

## Endpoints

### POST /v1/check

Scan text for banned words.

Scans the supplied text against one or more loaded languages and returns all matches. The matching mode defaults per language (`strict` for space-delimited languages, `substring` for CJK and Thai); an explicit caller mode wins and is echoed in `mode_used`.

**Request body:** [`CheckRequest`](#checkrequest)

```json
{
  "text": "some user input",
  "langs": ["en", "ja"],
  "mode": "strict"
}
```

Minimal form — defaults everywhere:

```json
{ "text": "some user input" }
```

**Responses**

| Status | Body                                  | Notes                                                                                            |
| ------ | ------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `200`  | [`CheckResponse`](#checkresponse)     | Successful scan. `X-List-Version` header set.                                                    |
| `400`  | [`Error`](#error)                     | Malformed JSON or `text` field entirely missing.                                                 |
| `401`  | [`Error`](#error)                     | Missing/invalid bearer token. Fast path before body parse.                                       |
| `413`  | [`Error`](#error)                     | Raw body exceeds 64 KiB, or NFKC-normalized `text` exceeds 192 KiB.                              |
| `422`  | [`Error`](#error)                     | Field-level rule violation (`invalid_mode`, `unknown_language`, `empty_text`, `empty_langs`).    |
| `500`  | [`Error`](#error)                     | Unexpected server error. Body is a fixed short string; detail goes to structured logs.           |
| `503`  | [`Error`](#error)                     | In-flight `/v1/check` count is at `VV_MAX_INFLIGHT`. Retry with jitter.                          |

**200 examples**

Hit:

```json
{
  "banned": true,
  "mode_used": { "en": "strict", "ja": "substring" },
  "matches": [
    {
      "lang": "en",
      "term": "idiot",
      "matched_text": "Ｉｄｉｏｔ",
      "start": 12,
      "end": 27
    }
  ],
  "truncated": false
}
```

Clean:

```json
{
  "banned": false,
  "mode_used": { "en": "strict" },
  "matches": [],
  "truncated": false
}
```

Truncated (more than 256 matches; response is a strict prefix of the full list in the documented concatenation order):

```json
{
  "banned": true,
  "mode_used": { "en": "strict" },
  "matches": [
    { "lang": "en", "term": "idiot", "matched_text": "idiot", "start": 0, "end": 5 }
  ],
  "truncated": true
}
```

### GET /v1/languages

List loaded languages and their default modes.

Returns every language compiled into this binary (optionally filtered by `VV_LANGS`) in alphabetical order by ISO code. This is the canonical concatenation order `/v1/check` uses when `langs` is omitted.

**Responses**

| Status | Body                                     | Notes                         |
| ------ | ---------------------------------------- | ----------------------------- |
| `200`  | [`LanguagesResponse`](#languagesresponse)| `X-List-Version` header set.  |
| `401`  | [`Error`](#error)                        |                               |
| `500`  | [`Error`](#error)                        |                               |

**200 example**

```json
{
  "languages": [
    { "code": "de", "default_mode": "strict" },
    { "code": "en", "default_mode": "strict" },
    { "code": "ja", "default_mode": "substring" }
  ]
}
```

### GET /healthz

Liveness probe. **Unauthenticated.**

Returns **200** once the HTTP listener is bound. Intended for Kubernetes `livenessProbe`. Body is unspecified.

### GET /readyz

Readiness probe. **Unauthenticated.**

Returns **200** once all Aho-Corasick automatons are built. Returns **503** during the startup window — observable only by a sidecar that races startup, since the listener binds after automatons finish loading.

**200 body**

```json
{
  "ready": true,
  "list_version": "5faf2ba1f5f1f0f7a7b8c9d0e1f2a3b4c5d6e7f8",
  "languages": 12
}
```

**503 body**

```json
{ "ready": false }
```

### GET /metrics

Prometheus scrape endpoint. **Unauthenticated; should be reachable only from the cluster/pod network.**

Content-Type: `text/plain; version=0.0.4` (Prometheus exposition format).

Series exposed:

- `vv_requests_total`
- `vv_auth_failures_total`
- `vv_request_duration_seconds`
- `vv_match_duration_seconds`
- `vv_matches_per_request`
- `vv_truncated_total`
- `vv_input_bytes`
- `vv_list_version_info`
- `vv_languages_loaded`
- `vv_inflight`

A **503** with body `metrics recorder not installed\n` is only reachable in embedded/test configurations that build the router without calling `observability::install_recorder`; production binaries install the recorder at startup, so this status does not occur on a normally deployed pod.

## Schemas

### CheckRequest

| Field    | Type                          | Required | Notes                                                                                                                                                                                                   |
| -------- | ----------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `text`   | string                        | yes      | Must be **byte-length** `>= 1`; whitespace-only is accepted. Raw request body capped at **64 KiB**; NFKC-normalized form capped at **192 KiB**. The emptiness check runs on raw input before normalization. |
| `langs`  | array of [`LanguageCode`](#languagecode) | no       | Languages to scan. Omit to scan every loaded language. Empty array is rejected with **422 `empty_langs`** — the "all" vs "none" distinction is deliberately explicit. `minItems: 1` when provided.      |
| `mode`   | [`Mode`](#mode)               | no       | When omitted, per-language defaults apply.                                                                                                                                                              |

**Unknown top-level fields are silently accepted and ignored.** `serde`'s `deny_unknown_fields` is deliberately not set so v2 can extend the schema without breaking v1 clients. The reserved `overrides` key is the concrete example.

### CheckResponse

| Field       | Type                                                | Notes                                                                                                                                        |
| ----------- | --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `banned`    | boolean                                             | True iff at least one match was found.                                                                                                       |
| `mode_used` | object (`LanguageCode` → [`Mode`](#mode))           | Mode actually applied per scanned language. One entry for every language scanned; absent from error responses.                               |
| `matches`   | array of [`Match`](#match), max 256 items           | Leftmost-longest, non-overlapping matches within each language, concatenated per-language in the order `langs` was supplied (or alphabetical by ISO code when `langs` is omitted). Capped at 256; see `truncated`. |
| `truncated` | boolean                                             | True iff the full match list was strictly longer than 256 and this response is a prefix.                                                     |

### Match

| Field          | Type                              | Notes                                                                                                                                                                                   |
| -------------- | --------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `lang`         | [`LanguageCode`](#languagecode)   |                                                                                                                                                                                         |
| `term`         | string                            | Canonical dictionary entry from LDNOOBW. Stable across requests; useful for grouping, metrics, and deduplication.                                                                       |
| `matched_text` | string                            | Exact slice of the caller's original input at `text[start:end]`. May differ from `term` after NFKC and case folding (e.g. `term: "idiot"`, `matched_text: "Ｉｄｉｏｔ"`).                |
| `start`        | integer `>= 0`                    | Inclusive byte offset into the original input. Always on a UTF-8 codepoint boundary. **Conservatively widened across NFKC expansions** so `text[start:end]` always contains the offending content in full. |
| `end`          | integer `>= 0`                    | Exclusive byte offset into the original input. Always on a UTF-8 codepoint boundary.                                                                                                    |

### LanguagesResponse

| Field       | Type                                                         |
| ----------- | ------------------------------------------------------------ |
| `languages` | array of `{ code: LanguageCode, default_mode: Mode }`        |

### ReadyResponse

| Field          | Type                        | Notes                                            |
| -------------- | --------------------------- | ------------------------------------------------ |
| `ready`        | boolean (const `true`)      |                                                  |
| `list_version` | string                      | LDNOOBW commit SHA the binary was built against. |
| `languages`    | integer `>= 0`              | Count of automatons loaded.                      |

### NotReadyResponse

| Field   | Type                     |
| ------- | ------------------------ |
| `ready` | boolean (const `false`)  |

### LanguageCode

ASCII language code matching `^[A-Za-z]{2,3}$` (ISO 639-1 where available, e.g. `en`, `ja`). Inputs are case-folded before lookup, so `"EN"` and `"en"` are equivalent; codes emitted by the server (e.g. in `/v1/languages` and `mode_used`) are always lowercase.

### Mode

Enum of:

| Value       | Semantics                                                                                                            |
| ----------- | -------------------------------------------------------------------------------------------------------------------- |
| `strict`    | Requires UAX #29 word boundaries on both edges of the match. Default for space-delimited languages.                  |
| `substring` | Accepts any Aho-Corasick hit. Default for CJK (`ja`, `zh`, `ko`) and Thai.                                           |

When omitted on a request, the per-language default applies.

### Error

| Field     | Type   | Notes                                                                                                                                                                                                     |
| --------- | ------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `error`   | string | Stable machine-readable code: `bad_request`, `unauthorized`, `payload_too_large`, `invalid_mode`, `unknown_language`, `empty_text`, `empty_langs`, `internal`, `overloaded`.                              |
| `message` | string | Short human-readable description. For `internal`, a fixed string — diagnostic detail goes to structured logs, never the response body.                                                                    |

## Error responses

| Status | `error` codes                                                               | Trigger                                                                                                      |
| ------ | --------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `400`  | `bad_request`                                                               | Malformed JSON or `text` field entirely missing.                                                             |
| `401`  | `unauthorized`                                                              | Missing `Authorization` header or key not in `VV_API_KEYS`. Rejected before body parse and in-flight gate.   |
| `413`  | `payload_too_large`                                                         | Raw body exceeds 64 KiB **or** NFKC-normalized `text` exceeds 192 KiB. Both cases share one code/message; logs record which fired. |
| `422`  | `invalid_mode`, `unknown_language`, `empty_text`, `empty_langs`             | Well-formed request violating a field-level rule.                                                            |
| `500`  | `internal`                                                                  | Unexpected server error. Body is a fixed string.                                                             |
| `503`  | `overloaded`                                                                | In-flight `/v1/check` count is at `VV_MAX_INFLIGHT`. Backpressure signal — retry with jitter.                |

Every error on `/v1/*` carries the `X-List-Version` header. 401s additionally carry `WWW-Authenticate: Bearer`.

**Example**

```json
{ "error": "unknown_language", "message": "unknown language: xx" }
```

## Forward-compatibility notes

- Unknown top-level request fields on `/v1/check` are silently ignored — do not rely on strict rejection. The `overrides` key is reserved for v2 per-tenant allow/denylists.
- `X-List-Version` is constant per process; cache-bust downstream artifacts on change.
- Public ingress (rate limits, request-size policy, auth federation) is deferred to v2 and requires a gateway in front.
