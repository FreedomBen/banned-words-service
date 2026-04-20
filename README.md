# Vocab Veto

A stateless, single-binary Rust HTTP service that answers one question across
many languages: *"Does this string contain a banned word?"*

The repository directory and crate name remain `banned-words-service`; **Vocab
Veto** is the product name. The `BWS_*` environment-variable prefix is part of
the binary's stable identity and is unchanged.

## Status

Under active development. See [IMPLEMENTATION_PLAN.md](./IMPLEMENTATION_PLAN.md)
for the milestone-by-milestone status. Authoritative documents:

| Document                                             | Purpose                                                                                       |
| ---------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| [DESIGN.md](./DESIGN.md)                             | Behavior spec, API surface, error table, metrics contract, threat model, deployment posture. |
| [IMPLEMENTATION_PLAN.md](./IMPLEMENTATION_PLAN.md)   | Milestone-ordered plan with explicit exit criteria per milestone.                             |
| [RELEASE.md](./RELEASE.md)                           | Human-owned release procedure: pre-tag gate, reproducibility check, load-test, tag and push. |
| [CLAUDE.md](./CLAUDE.md)                             | Agent-facing working notes and cross-cutting invariants.                                      |

## Build and run

Clone with submodules — the LDNOOBW word list is vendored and the build will
fail without it:

```sh
git clone --recurse-submodules <repo-url>
# or, in an existing checkout:
git submodule update --init --recursive
```

All common tasks run through the top-level `Makefile`:

| Target               | What it does                                                                                                            |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `make help`          | List targets (default).                                                                                                 |
| `make build`         | `cargo build --release --locked`.                                                                                       |
| `make test`          | `cargo test --locked` (unit + integration).                                                                             |
| `make bench`         | `cargo bench --no-run --locked` (compile-check the criterion suite).                                                    |
| `make lint`          | `cargo fmt --check` + `cargo clippy -- -D warnings`.                                                                    |
| `make podman`        | Build the distroless container image via rootless podman; override with `CONTAINER=docker`. Tagged with the LDNOOBW SHA. |
| `make run`           | Run locally with a dev-only `BWS_API_KEYS`.                                                                             |
| `make install-tools` | Install pinned dev tools (`oha` for load tests).                                                                        |
| `make release-check` | Pre-tag gate: `lint` + `test` + `bench` + `podman`. See [RELEASE.md](./RELEASE.md) for the rest of the release flow.    |

## Configuration

All runtime configuration is via environment variables (`BWS_*`) or an
optional TOML file. The authoritative list lives in [DESIGN.md §Deployment](./DESIGN.md#deployment);
the highlights are `BWS_API_KEYS` (required, comma-separated bearer keys),
`BWS_LANGS` (optional allowlist), `BWS_MAX_INFLIGHT` (default 1024), and
`BWS_LISTEN_ADDR` (default `0.0.0.0:8080`).

## Deployment

[`deploy/Containerfile`](./deploy/Containerfile) produces a `distroless/static:nonroot`
image via `cargo-chef` + musl, built with rootless podman. Kubernetes manifests for an in-cluster
(ClusterIP-only) deployment live under [`deploy/k8s/`](./deploy/k8s/). v1 is
in-cluster only — there is no Ingress or LoadBalancer; public exposure belongs
behind a separate gateway and is deferred to v2.

## What it does (in one paragraph)

One `Arc<AhoCorasick>` per language is built at startup from a word list
compiled into the binary at build time. Requests hit `POST /v1/check` with a
bearer API key; the service normalizes the input (NFKC + caseless), scans it
against the requested languages' automatons in a single pass, and returns
matches with byte spans that point back into the caller's original text. The
hot path is lock-free and bounded — no database, no disk, no hot reload. The
image tag is the list version.

## Word list attribution

The banned-words corpus is sourced from
**[LDNOOBW — List of Dirty, Naughty, Obscene, and Otherwise Bad Words](https://github.com/LDNOOBW/List-of-Dirty-Naughty-Obscene-and-Otherwise-Bad-Words)**
by Shutterstock and contributors. Vocab Veto vendors that repository as a git
submodule pinned at a specific commit SHA; that SHA is the list version
surfaced via the `X-List-Version` response header, the `/readyz` endpoint, and
the `bws_list_version_info` Prometheus metric.

All credit for the curation, translation, and ongoing maintenance of the word
list belongs to the LDNOOBW project and its contributors. Consult the upstream
repository for the canonical list and its licensing terms before redistributing
the corpus.

## License

This service's source code is licensed under the [MIT License](./LICENSE).
The MIT License covers the code in this repository only; it does **not** cover
the LDNOOBW word list, which is governed by its own upstream terms (see
attribution above).
