# Release checklist — Vocab Veto

The release procedure codified for IMPLEMENTATION_PLAN §M9. Each step is
manual on purpose: tagging and pushing images are one-way acts, so the
human running the release owns the signoff.

## 1. Pre-tag gate

Run from a clean checkout of `main`:

```bash
git status                       # must be clean
git submodule status vendor/ldnoobw   # must match the intended LDNOOBW SHA
make release-check               # fmt, clippy, tests, bench-compile, docker build
```

`make release-check` prints the image tag and revision it built on
success. The image is `banned-words-service:<LIST_SHA>` plus `:latest`.

## 2. Reproducibility verification (M9 item 1)

Build twice from the same commit + submodule SHA and confirm the image
manifest digest is stable (modulo the base layer's build timestamp):

```bash
make docker && podman image inspect banned-words-service:latest \
    --format '{{.Digest}} {{.Config.Labels.list_version}}'
podman image rm banned-words-service:latest banned-words-service:"$(git -C vendor/ldnoobw rev-parse HEAD)"
make docker && podman image inspect banned-words-service:latest \
    --format '{{.Digest}} {{.Config.Labels.list_version}}'
```

Both `list_version` label values must equal the submodule SHA. Digests
are expected to match; if they diverge investigate before tagging
(typically: a non-pinned dependency, a dirty working tree, or a cargo
registry cache miss that pulled a newer patch-level crate).

## 3. Load-test report (M9 item 2)

On a host with `oha`, `python3`, and `taskset`:

```bash
./scripts/load-test.sh            # 30s, c=64 against pinned core 0
```

The script writes
`benches/load/reports/<timestamp>-<list_sha>.txt` containing the oha
latency histogram. Milestone gate (from §M8 item 3): **p99 < 1 ms on
1 KiB English input, single core.** Attach the report file to the
release notes. If p99 regresses above the gate, do not tag — open
an issue instead.

## 4. `X-List-Version` sanity check (M9 item 3)

With the service running from the image under test:

```bash
curl -is -H "Authorization: Bearer ${BWS_API_KEYS}" \
    -d '{"text":"hello"}' \
    http://127.0.0.1:8080/v1/check \
    | grep -i '^x-list-version:'
```

The header value must equal `git -C vendor/ldnoobw rev-parse HEAD` at
the commit being tagged. `build.rs` emits this at compile time, so a
mismatch means the image was built from a different submodule state
than the tag — rebuild from a clean tree.

## 5. Tag and push (M9 item 4)

Operator-gated. Only run after steps 1–4 pass.

```bash
git tag -a v1.0.0 -m "v1.0.0"
git push origin v1.0.0

LIST_SHA="$(git -C vendor/ldnoobw rev-parse HEAD)"
podman tag banned-words-service:"${LIST_SHA}" \
    ghcr.io/<org>/banned-words-service:v1.0.0
podman tag banned-words-service:"${LIST_SHA}" \
    ghcr.io/<org>/banned-words-service:"${LIST_SHA}"
podman push ghcr.io/<org>/banned-words-service:v1.0.0
podman push ghcr.io/<org>/banned-words-service:"${LIST_SHA}"
```

Replace `<org>` with the target GHCR namespace. The two image tags
(`v1.0.0` and `:$LIST_SHA`) are both required by §M9 item 4.

## 6. Release notes

Draft in the GitHub release UI. Include:

- Commit SHA, LDNOOBW SHA, and the resulting image digest.
- The load-test report file from step 3 (paste or attach).
- A link to DESIGN.md at the tagged revision for API contract
  reference.
