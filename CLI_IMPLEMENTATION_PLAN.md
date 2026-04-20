# Vocab Veto CLI — Implementation Plan

A concrete, milestone-ordered plan for building `vv`, a command-line frontend
to the banned-words service specified in [DESIGN.md](./DESIGN.md). The CLI
reuses the matcher core verbatim so behavior mirrors the HTTP server exactly;
the differences are the transport (argv + stdio vs HTTP) and the trimmings
(no auth, no metrics, no concurrency gate, no listen socket).

This plan sits alongside [IMPLEMENTATION_PLAN.md](./IMPLEMENTATION_PLAN.md) —
the server plan — and assumes its milestones M1–M8 are complete (they are).
Do not duplicate matcher, normalizer, or codegen work here; reuse the library.

## Design premise

The value of this CLI is running the matcher with **no running service, no
network, and no runtime dependencies** — a single static binary that accepts
text on stdin or as a flag, emits JSON matching the server's response shape on
stdout, and exits non-zero when there are hits so it works as a shell filter.

Because the server already compiles the LDNOOBW list and matcher logic into
the binary via `build.rs`, the CLI can depend on the same library crate and
pick up new LDNOOBW pins for free on every rebuild. No duplication of the
generated term table, no drift from `DEFAULT_MODE`, no second normalization
path, no second way for the two binaries to disagree.

### What mirrors the server, and what does not

| Aspect                          | Server                                         | CLI                                                     | Note |
| ------------------------------- | ---------------------------------------------- | ------------------------------------------------------- | ---- |
| Matcher core                    | `matcher::{normalize, boundary, scan}`          | Same crate, same functions                              | No re-implementation. |
| LDNOOBW list                    | Compiled in via `build.rs`                     | Same compiled table                                     | `list_version` is the same SHA in both binaries. |
| `DEFAULT_MODE` table            | 27 entries, strict vs substring per language    | Same table                                              | Per-language defaults identical. |
| Explicit mode wins over default | Yes, no CJK clamping                           | Same                                                    | `strict` on `ja` is honored in both. |
| `mode_used` echo in response    | Every scanned language                         | JSON output: same. Plain output: dropped.               | Audit trail parity holds for `--output json`; `--output plain` is match-row-only by design (see CM4). |
| Alphabetical `langs` default    | Scan every loaded language alphabetically       | Same                                                    | Omitted `--lang` ⇒ all. |
| 256-match cap + `truncated`     | Enforced in `Engine::scan`                      | Same code path                                          | Same cap, same wire field. |
| Reserved `overrides` JSON key   | Silently accepted                              | Same, for `--json-input` mode                           | Schema forward-compatibility. |
| `list_version` in output        | `X-List-Version` header; `/readyz` body (not in `/v1/check` body) | Flat in `vv check` body; also in `vv version` body      | The CLI has no HTTP header transport, so the audit value lives in the JSON body. CLI `check` body is a strict superset of the server's `/v1/check` body — same fields plus `list_version`. |
| Bearer auth                     | Required on `/v1/*`                            | N/A                                                     | Local process; trust the invoker. |
| `VV_API_KEYS` / `VV_MAX_INFLIGHT` / `VV_LISTEN_ADDR` / `VV_HISTOGRAM_BUCKETS` / `VV_CONFIG_FILE` / `/etc/vv/config.toml` | Config-loaded | Ignored — CLI reads no env vars and no TOML | No server, no gate, no metrics, and the CLI is deliberately flag-driven. |
| `VV_LANGS` runtime allowlist    | Gates engine load at startup                   | Accepted as `--lang` flag on every invocation instead   | Flag-driven is the CLI idiom; env vars are not. |
| Languages-endpoint scope        | `/v1/languages` reflects the `VV_LANGS`-filtered loaded set | `vv languages` always lists every compiled language | CLI has no runtime allowlist, so the compiled set is the loaded set. |
| 64 KiB raw-body cap             | `RequestBodyLimitLayer`                         | No cap at input; 192 KiB post-normalization cap applies | CLI inputs are already process-bounded. |
| 413 `payload_too_large`         | Error response                                  | Exit 3 with message on stderr                           | Same underlying `NormalizeError::TooLarge`. |
| Observability (`tracing`, metrics) | JSON logs + Prometheus                       | `--verbose` stderr diagnostics only                     | No metrics, no persistent logs. |

### Non-goals (v1)

- **Remote HTTP client mode.** Deferred to v2. The primary value is
  offline; HTTP piping of `curl | jq` already works against the server and
  the CLI output shape matches `/v1/check`'s body byte-for-byte.
- **Custom dictionaries / user-supplied overrides.** The reserved
  `overrides` key stays reserved in both the server and the CLI — parsed,
  silently discarded. Actual override support lands when the server gets
  it, and reuses the same schema change.
- **Interactive REPL / `--watch` modes.** Shell loops (`while read`) handle
  streaming fine; dedicated modes add surface area without new capability.
- **Shell completions.** `clap_complete` is trivial to add later if
  users ask; not on the critical path.
- **Static Windows / macOS binaries.** Linux-musl is the only target in
  scope; the server ships the same target. Other platforms can be added
  once there is demand.

## Progress

- [ ] **CM1 — Library split and binary scaffold.** `src/lib.rs` re-exports
  the matcher and model types the CLI needs; `src/bin/vv.rs` is a thin
  entry point; `Cargo.toml` grows a `[[bin]] name = "vv"` stanza and a
  single new dep (`clap` with `derive`); `cargo build --release --bin vv`
  produces a runnable binary that prints `--help` and exits.
- [ ] **CM2 — `check` subcommand (local matcher).** End-to-end matching
  with flag-driven text input; JSON output on stdout; exit code reflects
  whether any matches were found.
- [ ] **CM3 — `languages` subcommand and `version` subcommand.** Read-only
  introspection endpoints the server exposes, reproduced as subcommands.
- [ ] **CM4 — Input variants, output formats, error rails.** stdin and
  file input for `check`; `--output plain` and `--output json`; one exit
  code per server error row, documented in the CLI help.
- [ ] **CM5 — Static musl binary, CI, docs.** `make vv` and
  `make vv-static` targets; CI builds and smoke-tests the CLI binary;
  README gets a `## CLI usage` section; `make install` installs both
  binaries by default.

## Repository layout (target delta)

```
banned-words-service/
├── src/
│   ├── bin/
│   │   └── vv.rs                   # NEW — thin CLI entry, calls src/cli
│   ├── cli.rs                       # NEW — arg parsing, I/O, formatting
│   ├── lib.rs                       # EDIT — `pub mod cli;` + re-export matcher::{Engine, Match, Mode, ScanResult, DEFAULT_MODE, NormalizeError, MAX_NORMALIZED_BYTES, LIST_VERSION} and model::*
│   └── main.rs                      # unchanged — still the server binary
├── tests/
│   └── cli.rs                       # NEW — integration tests invoking compiled `vv`
├── Cargo.toml                       # EDIT — add [[bin]] vv, add clap = { version = "4", features = ["derive"] }
├── Makefile                         # EDIT — add vv, vv-static, install targets
├── README.md                        # EDIT — add CLI usage section
└── CLI_IMPLEMENTATION_PLAN.md       # this file
```

Notes on the split:

- `src/cli.rs` holds the logic and is unit-testable against hand-crafted
  arg vectors; `src/bin/vv.rs` is ten lines that call into it. Mirrors
  how `src/main.rs` is thin and `routes/` / `matcher/` hold the weight.
- No workspace conversion. Single crate, two binaries, shared library.
  If we later need to ship the matcher as a published crate, that is the
  moment to split; not before.

## Milestone CM1 — Library split and binary scaffold

**Goal.** `cargo build --release --bin vv` produces a binary that prints
`--help` text covering the intended subcommand surface.

1. Add `pub mod cli;` to `src/lib.rs` so the binary can dispatch into it
   and the existing `cargo test --lib` job picks up CLI unit tests
   (CM5 CI item 2 depends on this). Extend the re-exports the binary
   needs: `pub use matcher::{Engine, Match, Mode, ScanResult,
   DEFAULT_MODE, NormalizeError, MAX_NORMALIZED_BYTES, LIST_VERSION};`
   (the `matcher` module is already `pub`, so these are shorthand for
   callers, not a visibility widening) and `pub use model::*;` for the
   request/response DTOs. Existing re-exports are left in place — this
   change is additive only, so `main.rs` and the server integration
   suite are unaffected.
2. Add to `Cargo.toml`:
   ```toml
   [[bin]]
   name = "vv"
   path = "src/bin/vv.rs"

   [dependencies]
   clap = { version = "4", features = ["derive"] }
   ```
   `clap` is the only new runtime dep. Rationale: hand-rolled argv
   parsing costs less code now but costs more when we add `--help`,
   `--version`, subcommand dispatch, and per-flag validation. The binary
   size hit is minor and well-precedented (ripgrep, fd, bat all ship
   with clap).
3. Create `src/cli.rs` with a `Cli` struct using clap-derive. Subcommands:
   `Check`, `Languages`, `Version`. Every subcommand can be parsed and
   dispatched to a stub returning `Ok(ExitCode::SUCCESS)` in this
   milestone.
4. Create `src/bin/vv.rs`:
   ```rust
   use banned_words_service::cli;
   fn main() -> std::process::ExitCode { cli::run() }
   ```
5. Unit tests in `src/cli.rs` covering clap's generated parser: each
   subcommand parses; unknown subcommand errors; the full mutex set on
   `check` errors before dispatch — `--text` + `--file`, `--text` +
   `--stdin`, `--file` + `--stdin`, and `--json-input` + any of
   `--text` / `--file` / `--stdin` / `--lang` / `--mode`.

**Exit criteria.** `cargo build --release --bin vv` green;
`./target/release/vv --help` lists all three subcommands; `cargo test
--lib` still passes with the added re-exports.

## Milestone CM2 — `check` subcommand (local matcher)

**Goal.** `echo "text" | vv check --lang en` runs the same matcher the
server runs and writes the same JSON body to stdout.

1. `vv check` flags:
   - `--text <STR>` — inline text. Mutually exclusive with `--file` and
     `--stdin`.
   - `--file <PATH>` — read text from file. `-` is a synonym for stdin.
   - `--stdin` — read text from stdin. If none of `--text`/`--file`/
     `--stdin`/`--json-input` is given and stdin is not a TTY, default
     to `--stdin`; if it is a TTY, error (exit 2) with a usage hint.
     `--json-input` counts as an input source for this check — it can
     stand alone without `--stdin`/`--text`/`--file`. Matches `jq`/`rg`.
   - `--lang <CODE>` (repeatable; also accepts comma-separated) — the
     `langs` field on the request body. Parsing splits on `,` and
     trims surrounding ASCII whitespace per entry (argv-only
     plumbing — the server doesn't need either step because JSON
     already pre-parses into an array of strings), then ASCII-
     lowercases each entry and checks membership against loaded
     codes. **No deduplication**: a repeated code scans the same
     language twice, exactly mirroring what the server does when a
     caller sends `{"langs":["en","en"]}` on the body path. (This
     is a deliberate choice for parity with `--json-input`, which
     passes `langs` through to the server's body-side handling
     untouched; dedup would make argv and JSON paths diverge.) An
     unknown code exits 2 with a stderr message listing the
     compiled codes (same phrasing as the server's
     `UnknownLangsError`). **Ordering** is preserved: repeated flags
     appear in invocation order, and within a single occurrence
     comma-separated entries expand left-to-right. This is the order
     echoed into match concatenation, matching the server's
     preservation of `langs[]` order. Omitted ⇒ scan every compiled
     language alphabetically, matching the server default. An empty
     list after parsing is rejected with exit 2 (the CLI analog of
     the server's `422 empty_langs`); this state is only reachable
     via `--json-input` carrying `{"langs": []}`. An empty argv
     value like `--lang ""` is not the empty-list case — it produces
     a one-entry list containing the empty string, which falls
     through to the unknown-language rail with the empty string as
     its (bogus) code.
   - `--mode <strict|substring>` — the `mode` field. Omitted ⇒
     per-language default from `DEFAULT_MODE`, same as the server.
   - `--json-input <PATH>` — alternative to the individual flags: parse
     the file (or `-` for stdin) as a full `CheckRequest` JSON body,
     exactly the shape the server accepts. Unknown fields including
     `overrides` are silently ignored, matching the server's
     forward-compat stance. Mutually exclusive with `--text`/`--file`/
     `--stdin`/`--lang`/`--mode`. The DTO mirrors the server's M3
     shape — `mode` typed as `Option<String>` so an unrecognized value
     reaches handler-side validation instead of aborting at serde.
     Malformed JSON surfaces a stderr message of the form `"invalid
     JSON: ..."`; an otherwise-valid body with a bad field produces a
     matching message (`"invalid mode: ..."`, `"unknown language: ..."`,
     `"empty text"`, `"empty langs"`). All collapse to exit 2 per CM4,
     but the stderr distinction preserves parity with the server's
     400/422 rows.
2. Dispatch calls `Engine::scan(text, &scan_langs, mode)` directly.
   `Engine::scan` normalizes the raw text internally and returns
   `Result<ScanResult, NormalizeError>`, matching the pipeline
   `routes/check.rs` drives on the server side. The raw ≥1-byte
   check runs uniformly on the text regardless of whether it arrived
   via `--text` / `--file` / `--stdin` or the `text` field of
   `--json-input`. A `NormalizeError::TooLarge` (post-normalization
   > 192 KiB) maps to exit 3 with a short stderr message; other
   user-input failures (empty text, unknown language, invalid mode)
   map to exit 2 (see CM4 for the full table). On
   success, serialize a CLI-side wrapper DTO defined in `src/cli.rs`:
   ```rust
   #[derive(Serialize)]
   struct CliCheckResponse {
       #[serde(flatten)] inner: CheckResponse,
       list_version: &'static str,
   }
   ```
   via `serde_json::to_writer(stdout())`. The `#[serde(flatten)]`
   means the `banned`/`mode_used`/`matches`/`truncated` keys sit at
   the top level byte-identical to the server's `/v1/check` body;
   `list_version` is the only added key. Rationale: the server
   surfaces `list_version` via the `X-List-Version` header, which the
   CLI has no transport for — embedding it in the body is the CLI's
   single-invocation audit-trail analog, so a caller that already has
   `vv check` output never needs to also call `vv version` to know
   which list SHA produced the decision.
3. Exit-code policy for `check`:
   - `0` — no matches, `truncated: false`. Scriptable as "the text is
     clean."
   - `1` — one or more matches, or `truncated: true`. Scriptable as
     "there is at least one hit."
   - Errors use `2`+ per CM4.
   The exit-code bifurcation is intentional: it makes the CLI usable as
   a pre-commit hook without parsing JSON (`vv check --text "$MSG"
   --lang en && commit || reject`).
4. Integration tests in `tests/cli.rs` spawn the compiled binary via
   `assert_cmd` (dev-dep, test-only, not a runtime dep) and cover: the
   Scunthorpe case under strict en (exit 0), explicit strict on CJK is
   honored (exit code reflects hits), fullwidth evasion under
   substring CJK produces matches, `--json-input` with unknown fields
   including `overrides` succeeds, omitted `--lang` scans everything.
   Hardcoded input strings (Scunthorpe, fullwidth-evasion corpus, CJK
   samples) may be copied from `tests/http.rs`; any actual helper
   functions worth sharing lift into `tests/common/mod.rs`, which
   both integration suites then `mod common;`-import (Rust's
   integration-test files are separate compilation units, so this is
   the only cross-test sharing mechanism).

**Exit criteria.** `vv check --text "Scunthorpe" --lang en` exits `0`
with an empty matches array; `vv check --text "badword" --lang en`
exits `1` with a non-empty matches array; the JSON body is the
server's `CheckResponse` plus a `list_version` field — i.e.
`jq 'del(.list_version)'` on the CLI output equals the server's
`/v1/check` body for the same inputs byte-for-byte (modulo
whitespace).

## Milestone CM3 — `languages` and `version` subcommands

**Goal.** Introspection parity with the server's read-only endpoints.

1. `vv languages` emits the same `LanguagesResponse` shape the server
   serves on `GET /v1/languages`: an alphabetical array of `{code,
   default_mode}` objects for every compiled language. Reuses
   `model::LanguagesResponse` directly. Exit 0.
2. `vv version` emits a small JSON object: `{"crate_version": "<from
   CARGO_PKG_VERSION>", "list_version": "<LDNOOBW SHA>", "languages":
   <N>}`. Shape is `/readyz`'s body minus `ready` (always true for a
   local binary — if the binary ran, the engine built) plus
   `crate_version` — the one CLI-only addition, since the server
   surfaces the build identity via the image tag and has no need to
   echo it in a response. Rationale:
   the server surfaces `list_version` via `X-List-Version` and `/readyz`;
   the CLI has no HTTP response to hang it on, so it gets its own
   subcommand. Exit 0. A plain `vv --version` (clap's built-in) still
   works and prints the crate version; `vv version` is the scriptable
   JSON form.
3. Integration tests assert that `vv languages | jq '.languages | length'`
   equals the compiled language count, and that every `default_mode` is
   one of `"strict"` or `"substring"`.

**Exit criteria.** `vv languages` and `vv version` produce valid JSON
that round-trips through `serde_json::from_str::<LanguagesResponse>` /
an ad-hoc version struct respectively.

## Milestone CM4 — Input variants, output formats, error rails

**Goal.** Every documented server error row has a CLI exit code, and
humans can read the output without `jq`.

1. `--output <json|plain>`, default `json`. `plain` for `check`:
   ```
   <lang>\t<start>-<end>\t<term>\t<matched_text>
   ```
   one TSV row per match, nothing when there are zero matches. Row
   order follows DESIGN §"Match ordering": per-language
   leftmost-longest non-overlapping, concatenated in the
   caller-supplied `--lang` order (alphabetical when `--lang` is
   omitted). When the response would have `truncated: true`, emit a
   final `# truncated` sentinel line after exactly 256 match rows
   (the server's hard cap per DESIGN §"Match ordering"); `#` is a
   comment prefix for awk/`cut` consumers, and consumers that don't
   recognize it still see the row count equal the cap.
   `mode_used` is **not** emitted in plain output (JSON-only by
   design, per the mirror-table caveat); callers who need the per-lang
   mode echo should use `--output json`. Plain output for other
   subcommands: `languages` emits one `<code>\t<default_mode>` row per
   language; `version` emits `<crate_version>\t<list_version>\t<languages>`.
2. Exit-code table, published in `vv check --help`:

   | Exit | Meaning                             | Corresponds to server |
   | ---- | ----------------------------------- | --------------------- |
   | `0`  | success, no matches                 | 200 OK, empty matches |
   | `1`  | success, matches found or truncated | 200 OK, hits          |
   | `2`  | invalid usage / malformed JSON       | 400 `bad_request`, 422 `invalid_mode`, 422 `empty_text`, 422 `empty_langs`, 422 `unknown_language` — all collapsed to a single "user error" code on the CLI because argv is one rail, not several |
   | `3`  | input too large (post-normalization) | 413 `payload_too_large` via `NormalizeError::TooLarge` |
   | `64` | I/O error — file unreadable, stdin closed early, or raw-text input bytes are not valid UTF-8 (invalid UTF-8 inside `--json-input` surfaces as "invalid JSON" and exits `2`) | no server equivalent — CLI-specific |
   | `70` | internal error — a `std::panic::catch_unwind` wrapper at the top of `cli::run` caught a panic (should not happen) | 500 `internal` |
   | —    | unreachable in the CLI              | 401 `unauthorized` — no auth layer in a local process; 503 `overloaded` — no in-flight gate, so the condition cannot arise |

   The collapse of 400/422 into a single `2` is deliberate: users
   interpret CLI exit codes coarsely, and stderr carries the specific
   message (`"unknown language: xx"`, etc.). Scripts that need the
   precise reason can parse stderr or use `--output json` with a server
   instead.
3. `--verbose` / `-v` is a flag on `check` only — `languages` and
   `version` have no per-invocation computation worth narrating — and
   emits tracing-style single-line diagnostics on stderr: input
   length, normalized length, mode resolution, per-lang match counts.
   No `tracing_subscriber` dependency — this is direct `eprintln!`
   with a consistent prefix. Metrics parity is not a goal.
   Stream split is fixed: stdout carries only the normal output (JSON
   or plain per `--output`); `-v` lines always go to stderr regardless
   of `--output`, so `vv check | jq` is unaffected by verbosity.
4. Integration tests cover every exit-code row with a triggering
   invocation, plus plain-output parity against hand-asserted bytes.
   Exception: exit 70 is unit-tested against `cli::run_inner` (the
   un-wrapped body that `cli::run` calls inside its
   `catch_unwind`) with an injected panic — staging a deterministic
   crash on the spawned binary would need a hidden test-only flag
   that isn't worth the surface area.

**Exit criteria.** `vv check --help` documents the exit-code table;
every row is triggered by a test in `tests/cli.rs`.

## Milestone CM5 — Static musl binary, CI, docs

**Goal.** A downloadable `vv` binary that runs anywhere x86_64 Linux
runs, with no dynamic linkage.

1. `Makefile` targets:
   - `make vv` — `cargo build --release --bin vv --locked`.
   - `make vv-static` — `cargo build --release --bin vv --locked
     --target x86_64-unknown-linux-musl`. Host prerequisite: `rustup
     target add x86_64-unknown-linux-musl` plus a musl linker
     (`musl-tools` on Debian/Ubuntu, `musl-gcc` on Fedora). `make help`
     documents the one-shot setup. This is a host build, independent
     of `deploy/Containerfile`'s cargo-chef builder — the container
     uses the same target internally but its toolchain is not reused
     by the host target.
   - `make install` — install both `banned-words-service` and `vv` to
     `$(PREFIX)/bin` (default `/usr/local/bin` per the global PREFIX
     convention). No `install` target exists in the Makefile today
     (M7 item 5 lists `help`, `build`, `test`, `bench`, `lint`,
     `podman`, `run` only); this milestone adds it.
   - Update `make help` so the new targets appear in the same table
     format as the rest.
2. CI (`.github/workflows/ci.yml`) gains a `cli` job:
   - `cargo build --release --bin vv --locked`
   - `cargo test --test cli --locked` (the integration suite from CM2+)
   - Smoke test: `./target/release/vv check --text "hello" --lang en`
     must exit 0.
   The existing `cargo test --locked` job already covers `src/cli.rs`
   unit tests since they live in the library.
3. README gets a `## CLI usage` section with: one-line intro, two or
   three `vv check` examples showing `--text`, `--file`, and
   `--json-input`, a pointer to `vv --help` for the full surface, and
   an explicit note that the CLI is feature-parity with `/v1/check` and
   `/v1/languages` except for auth, metrics, and the concurrency gate.
   The existing Makefile-targets table in the README gets the new rows.
4. New `.github/workflows/release.yml` — triggered on `v*` tag
   pushes — handles both release artifacts in one place: builds and
   pushes the server image to the registry at `:$TAG` and
   `:$LIST_SHA`, builds the musl `vv` binary via `make vv-static`,
   and uploads the binary as a GitHub release asset on the tag.
   This automates the server IP M9 item 4 image-cut step, which is
   currently described as a manual `podman push`; that milestone
   warrants a one-line update to reference this workflow, landed in
   the same commit as CM5. RELEASE.md is amended to document the
   two-artifact story: reproducibility double-build and
   list-version sanity check apply to both, the verify step is
   `./vv version` against the expected LDNOOBW SHA, and the
   human-owned procedure now ends at "push tag, watch workflow"
   rather than a manual image push.

**Exit criteria.** `ldd ./target/x86_64-unknown-linux-musl/release/vv`
prints "not a dynamic executable"; `./vv check --text "..." --lang en`
on a fresh Debian slim container (no Rust, no glibc add-ons) runs
without error.

## Open questions to resolve before CM2

- **Binary name.** Decided: `vv`, after the product name Vocab Veto. Short,
  easy to type, and keeps the CLI recognisable as a separate identity from
  the crate (`banned-words-service`, unchanged) and the env-var prefix
  (`VV_*`, renamed from the old `BWS_*` when the project was rebranded as
  Vocab Veto). The two-letter name does not collide with any common POSIX
  utility on a default install.
- **Should `vv check --json-input` accept NDJSON for streaming many
  records in one invocation?** Attractive for pipelines, not on the
  server's API surface so strictly a CLI-only feature. Default: no,
  defer to user request; one-request-per-invocation keeps the CLI a
  transparent analog of one HTTP call.
- **Should `vv check` respect a `VV_LANGS` env var as a default for
  `--lang` when neither the flag nor the input JSON sets it?** Server
  parity would say yes (it is the compile-time allowlist surface).
  Default: no — the CLI's idiom is flag-driven, and the server default
  of "scan all compiled languages" already matches what most scripts
  want. Revisit if users ask.
