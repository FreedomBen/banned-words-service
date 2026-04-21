# Release checklist — Vocab Veto

The release procedure codified for IMPLEMENTATION_PLAN §M9. Each step is
manual on purpose: tagging and pushing images are one-way acts, so the
human running the release owns the signoff.

## 1. Pre-tag gate

Run from a clean checkout of `main`:

```bash
git status                       # must be clean
git submodule status vendor/ldnoobw   # must match the intended LDNOOBW SHA
make release-check               # fmt, clippy, tests, bench-compile, podman build
```

`make release-check` prints the image tag and revision it built on
success. The image is `vocab-veto:<LIST_SHA>` plus `:latest`.

## 2. Reproducibility verification (M9 item 1)

Reproducibility is checked across **both release artifacts**: the server
container image and the static `vv` CLI binary. Each is built twice
from the same commit + submodule SHA and expected to produce the same
output.

### 2a. Server image

```bash
make podman && podman image inspect vocab-veto:latest \
    --format '{{.Digest}} {{.Config.Labels.list_version}}'
podman image rm vocab-veto:latest vocab-veto:"$(git -C vendor/ldnoobw rev-parse HEAD)"
make podman && podman image inspect vocab-veto:latest \
    --format '{{.Digest}} {{.Config.Labels.list_version}}'
```

Both `list_version` label values must equal the submodule SHA. Digests
are expected to match; if they diverge investigate before tagging
(typically: a non-pinned dependency, a dirty working tree, or a cargo
registry cache miss that pulled a newer patch-level crate).

### 2b. Static vv binary

```bash
make vv-static && sha256sum ./target/x86_64-unknown-linux-musl/release/vv
cargo clean --release --target x86_64-unknown-linux-musl
make vv-static && sha256sum ./target/x86_64-unknown-linux-musl/release/vv
```

Both SHA-256s must match. `ldd ./target/x86_64-unknown-linux-musl/release/vv`
must print `not a dynamic executable` — the release workflow enforces
this, but a local check before tagging surfaces host-toolchain drift
(missing musl linker, stale `x86_64-unknown-linux-musl` target) earlier.

## 3. Load-test report (M9 item 2)

One-time per host: install the pinned load-test tool.

```bash
make install-tools                # cargo install --locked --version $(OHA_VERSION) oha
```

Then:

```bash
./scripts/load-test.sh            # 30s, c=1, port 18080, server on core 0
```

The script writes
`benches/load/reports/<timestamp>-<list_sha>.txt` containing the oha
latency histogram. Milestone gate (from §M8 item 3): **p99 < 1 ms on
1 KiB English input, single core.** c=1 is the service-time
measurement the gate is written against; higher concurrency on a
single-core server drifts into queue-dominated latency that doesn't
reflect matcher performance. Attach the report file to the release
notes. If p99 regresses above the gate, do not tag — open an issue
instead.

## 4. `list_version` sanity check (M9 item 3)

Both artifacts surface the LDNOOBW SHA; both must agree with the
submodule state at the commit being tagged. `build.rs` emits the SHA
at compile time, so a mismatch means the artifact was built from a
different submodule state — rebuild from a clean tree.

### 4a. Server — `X-List-Version` response header

With the service running from the image under test:

```bash
curl -is -H "Authorization: Bearer ${VV_API_KEYS}" \
    -d '{"text":"hello"}' \
    http://127.0.0.1:8080/v1/check \
    | grep -i '^x-list-version:'
```

### 4b. CLI — `vv version`

```bash
./target/x86_64-unknown-linux-musl/release/vv version \
    | jq -r '.list_version'
```

The header value and the CLI's `list_version` field must both equal
`git -C vendor/ldnoobw rev-parse HEAD`.

## 5. Tag and push (M9 item 4)

Operator-gated. Only run after steps 1–4 pass.

```bash
git tag -a v1.0.0 -m "v1.0.0"
git push origin v1.0.0
```

That is the entire operator step. `.github/workflows/release.yml`
fires on the `v*` tag push and produces both release artifacts:

- The server image is built and pushed to
  `ghcr.io/<owner>/vocab-veto:v1.0.0` and
  `ghcr.io/<owner>/vocab-veto:${LIST_SHA}` (the pair required
  by §M9 item 4).
- The static `vv` binary is built against
  `x86_64-unknown-linux-musl`, verified via `ldd` and a smoke check,
  and uploaded as a GitHub release asset on the same tag.

Watch the workflow from the GitHub Actions tab; do not touch `podman
push` manually — if the workflow fails, fix the underlying issue and
retag rather than bypassing the automation.

## 6. Release notes

Draft in the GitHub release UI. Include:

- Commit SHA, LDNOOBW SHA, and the resulting image digest.
- The load-test report file from step 3 (paste or attach).
- A link to DESIGN.md at the tagged revision for API contract
  reference.
