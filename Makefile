# Vocab Veto / banned-words-service — top-level Makefile.
#
# Targets are thin wrappers around cargo and a container CLI (podman by
# default, rootless) so humans and CI run identical invocations. Edit
# `make help` if you add or rename a target.

PREFIX ?= /usr/local
CARGO ?= cargo
# Container CLI. Defaults to podman (rootless by default). Override with
# `make docker CONTAINER=docker` on hosts that ship only Docker.
CONTAINER ?= podman

IMAGE_NAME ?= banned-words-service
LIST_SHA := $(shell git -C vendor/ldnoobw rev-parse HEAD 2>/dev/null)
REVISION := $(shell git rev-parse HEAD 2>/dev/null)

# A throwaway dev key for `make run`. Long enough to clear the 32-byte short-
# key warning. Never use this anywhere real.
DEV_API_KEY ?= dev-key-do-not-use-in-production-0000000000000000

.DEFAULT_GOAL := help

.PHONY: help build test bench lint docker run release-check

help: ## Show this help
	@awk 'BEGIN {FS = ":.*?## "; print "Vocab Veto — make targets\n"} \
	      /^[a-zA-Z_-]+:.*?## / {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo ""
	@echo "  CONTAINER=$(CONTAINER) (override with CONTAINER=docker)"

build: ## Build the release binary (cargo build --release --locked)
	$(CARGO) build --release --locked

test: ## Run the full test suite (cargo test --locked)
	$(CARGO) test --locked

bench: ## Compile benchmarks without running (cargo bench --no-run --locked)
	$(CARGO) bench --no-run --locked

lint: ## cargo fmt --check and cargo clippy -- -D warnings
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets --locked -- -D warnings

docker: ## Build the container image (rootless podman; see footer for override), tagged with the LDNOOBW SHA
	@if [ -z "$(LIST_SHA)" ]; then \
	  echo "error: could not read LDNOOBW SHA from vendor/ldnoobw; run: git submodule update --init --recursive" >&2; \
	  exit 1; \
	fi
	$(CONTAINER) build \
	  -f deploy/Dockerfile \
	  --build-arg LIST_VERSION=$(LIST_SHA) \
	  --build-arg REVISION=$(REVISION) \
	  -t $(IMAGE_NAME):$(LIST_SHA) \
	  -t $(IMAGE_NAME):latest \
	  .

run: ## Run locally via cargo run with a dev-only BWS_API_KEYS
	BWS_API_KEYS="$(DEV_API_KEY)" $(CARGO) run --release --locked

release-check: lint test bench docker ## Full pre-tag gate: lint, test, bench-compile, container image
	@echo ""
	@echo "release-check OK"
	@echo "  image: $(IMAGE_NAME):$(LIST_SHA)"
	@echo "  revision: $(REVISION)"
	@echo ""
	@echo "Next: follow RELEASE.md to capture a load-test report, tag v1.0.0, and push the image."
