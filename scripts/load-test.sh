#!/usr/bin/env bash
# Vocab Veto — end-to-end load-test runner.
#
# Boots the release binary pinned to a single core, runs
# benches/load/oha-1kib-en.sh against it, writes the oha output to
# benches/load/reports/<timestamp>-<shortsha>.txt, and tears the
# server down. The milestone gate (IMPLEMENTATION_PLAN §M8 item 3,
# attached to the release per §M9 item 2) is p99 < 1 ms.
#
# Prereqs on PATH: cargo, oha, python3, taskset.
#
# Usage:
#   ./scripts/load-test.sh [DURATION] [CONCURRENCY]
#
# Defaults: 30s, c=64.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

DURATION="${1:-30s}"
CONCURRENCY="${2:-64}"

for cmd in cargo oha python3 taskset; do
    command -v "${cmd}" >/dev/null || {
        echo "error: ${cmd} not found on PATH" >&2
        exit 1
    }
done

LIST_SHA="$(git -C vendor/ldnoobw rev-parse --short HEAD 2>/dev/null || echo unknown)"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
REPORT_DIR="benches/load/reports"
REPORT_FILE="${REPORT_DIR}/${TS}-${LIST_SHA}.txt"
mkdir -p "${REPORT_DIR}"

# Build once so `cargo run --release` doesn't race the load test at startup.
cargo build --release --locked

API_KEY="load-test-$(openssl rand -hex 24)"
LOG_FILE="$(mktemp -t bws-load-server.XXXXXX.log)"
trap 'if [ -n "${SERVER_PID:-}" ] && kill -0 "${SERVER_PID}" 2>/dev/null; then kill "${SERVER_PID}" 2>/dev/null || true; wait "${SERVER_PID}" 2>/dev/null || true; fi; rm -f "${LOG_FILE}"' EXIT

BWS_API_KEYS="${API_KEY}" BWS_LISTEN_ADDR="127.0.0.1:8080" \
    taskset -c 0 ./target/release/banned-words-service \
    >"${LOG_FILE}" 2>&1 &
SERVER_PID=$!

# Poll /healthz so we don't race the listener bind.
for _ in $(seq 1 50); do
    if curl -fsS "http://127.0.0.1:8080/healthz" >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done
if ! curl -fsS "http://127.0.0.1:8080/healthz" >/dev/null 2>&1; then
    echo "error: server did not become ready; last 20 lines of server log:" >&2
    tail -n 20 "${LOG_FILE}" >&2 || true
    exit 1
fi

{
    echo "# Vocab Veto load-test report"
    echo "# timestamp: ${TS}"
    echo "# list_sha:  ${LIST_SHA}"
    echo "# revision:  $(git rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "# duration:  ${DURATION}"
    echo "# concurrency: ${CONCURRENCY}"
    echo "# cpu pin:   taskset -c 0"
    echo "# target:    p99 < 1 ms on 1 KiB en input (IMPLEMENTATION_PLAN §M8 item 3)"
    echo ""
} > "${REPORT_FILE}"

BWS_API_KEY="${API_KEY}" ./benches/load/oha-1kib-en.sh \
    "http://127.0.0.1:8080/v1/check" "${DURATION}" "${CONCURRENCY}" \
    | tee -a "${REPORT_FILE}"

echo ""
echo "report written to ${REPORT_FILE}"
