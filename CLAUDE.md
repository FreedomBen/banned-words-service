# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Agent Instructions

- Commit after making code changes, but do not push

## Repository state

This repository is **pre-code**. The only tracked artifacts today are design documents:

- [DESIGN.md](./DESIGN.md) — authoritative spec for the service's behavior, API surface, error table, metrics contract, threat model, and deployment story. Treat it as the source of truth when implementing; do not drift without updating it in the same commit.
- [IMPLEMENTATION_PLAN.md](./IMPLEMENTATION_PLAN.md) — milestone-ordered plan derived from DESIGN.md. Each milestone has explicit exit criteria; prefer working one milestone at a time rather than smearing work across several.
- `repo.txt` — a single URL pointing at [LDNOOBW](https://github.com/LDNOOBW/List-of-Dirty-Naughty-Obscene-and-Otherwise-Bad-Words), the authoritative word list the service will vendor as a git submodule.

No `Cargo.toml`, `Makefile`, `Containerfile`, or source tree exists yet. Build/test/lint commands will be established during Milestone 1 of the implementation plan; update this file with real commands once they exist.

## What this service is (big-picture)

The project is named **Vocab Veto**. The repo directory, crate name, and installed binary name remain `banned-words-service`; the **container image** is `vocab-veto` (matches the product name). The env-var prefix is `VV_*` (renamed from the old `BWS_*` when the project was rebranded). Kubernetes resource names and the in-cluster DNS (`banned-words-service.<namespace>.svc.cluster.local`) are also unchanged — callers depend on that hostname.

A stateless, single-binary Rust HTTP service that answers *"does this string contain a banned word?"* across many languages, backed by `aho-corasick` automatons built from the LDNOOBW list. The entire list is compiled into the binary at build time; there is no hot reload, no database, no external state. The image tag is the list version.

Key cross-cutting invariants from DESIGN.md that any change should respect:

- **Hot path is lock-free and bounded.** One `Arc<AhoCorasick>` per language, shared across all request tasks. Per-request allocations are bounded (JSON buffers, one offset map, ≤256 short match strings).
- **Authentication is uniform and fail-closed.** Every `/v1/*` endpoint requires a bearer API key; keys are compared via `subtle::ConstantTimeEq`, iterating the full configured set every time. 401 is a fast path that runs before body parse and before the in-flight gate. The service refuses to start with no keys configured.
- **`X-List-Version` on every `/v1/*` response.** Success, 4xx (including 401 fast path), and 5xx alike. It is a constant per process (the pinned LDNOOBW submodule SHA), so attaching it is free.
- **Span semantics point into the caller's original text.** Normalization (NFKC + `caseless`) runs alongside an offset map so reported `start`/`end` can be used directly for `text[start:end]` slicing. Match spans are conservatively widened across NFKC expansions — see the "Mapping across NFKC expansions" section in DESIGN.md before touching the normalizer.
- **Per-language mode defaults live in code, not config.** `strict` for space-delimited languages, `substring` for CJK (`ja`, `zh`, `ko`). Explicit caller mode wins without clamping; `mode_used` is the audit trail.
- **Unknown request fields are silently accepted.** `serde`'s `deny_unknown_fields` is deliberately **not** set so v2 can extend the schema without breaking v1 clients. The reserved `overrides` key is the concrete example.

If a change conflicts with any of these, update DESIGN.md in the same commit and call the change out explicitly — don't silently drift.

## Working conventions specific to this repo

- **Do not read `TODO.md`.** It is for humans only (inherited from global instructions); agents should work from DESIGN.md and IMPLEMENTATION_PLAN.md instead.
- **LDNOOBW is vendored via git submodule at a pinned SHA.** That SHA is the list version surfaced in `X-List-Version`, `/readyz`, and the `vv_list_version_info` metric. Bumping the SHA is a deliberate act — treat it as a release, not a routine update.
- **Docs track code.** Per the user's global rule, documentation updates land in the same commit as the code change that makes them necessary. For this repo that most often means DESIGN.md (behavior changes) or IMPLEMENTATION_PLAN.md (milestone exit criteria).
- **Commits.** Don't prefix subject lines with `feat:` / `fix:` / etc.; just describe the change. Don't include Claude as a co-author. Only run git commands when asked.
