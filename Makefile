# Vocab Veto / banned-words-service — top-level Makefile.
#
# Targets are thin wrappers around cargo and a container CLI (podman by
# default, rootless) so humans and CI run identical invocations. Edit
# `make help` if you add or rename a target.

PREFIX ?= /usr/local
CARGO ?= cargo
# Container CLI. Defaults to podman (rootless by default). Override with
# `make podman CONTAINER=docker` on hosts that ship only Docker.
CONTAINER ?= podman

IMAGE_NAME ?= vocab-veto
LIST_SHA := $(shell git -C vendor/ldnoobw rev-parse HEAD 2>/dev/null)
REVISION := $(shell git rev-parse HEAD 2>/dev/null)

# Pinned dev-tool versions. `install-tools` installs each via
# `cargo install --locked --version $(VERSION) <crate>`, so rebuilds on
# a fresh host resolve to the same source (Cargo.lock is consulted) and
# the same version. Bumping a pin is a deliberate act.
OHA_VERSION ?= 1.14.0

# A throwaway dev key for `make run`. Long enough to clear the 32-byte short-
# key warning. Never use this anywhere real.
DEV_API_KEY ?= dev-key-do-not-use-in-production-0000000000000000

.DEFAULT_GOAL := help

.PHONY: help build test bench lint podman run release-check install-tools vv vv-static install

help: ## Show this help
	@awk 'BEGIN {FS = ":.*?## "; print "Vocab Veto — make targets\n"} \
	      /^[a-zA-Z_-]+:.*?## / {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo ""
	@echo "  CONTAINER=$(CONTAINER) (override with CONTAINER=docker)"
	@echo "  PREFIX=$(PREFIX) (override with PREFIX=/path for make install)"
	@echo ""
	@echo "  One-shot setup for vv-static: rustup target add x86_64-unknown-linux-musl"
	@echo "    plus a musl linker — 'musl-tools' on Debian/Ubuntu, 'musl-gcc' on Fedora."

build: ## Build the release server binary (cargo build --release --locked)
	$(CARGO) build --release --locked

vv: ## Build the release vv CLI binary (cargo build --release --bin vv --locked)
	$(CARGO) build --release --bin vv --locked

vv-static: ## Build a static musl vv for x86_64 Linux (see make help for host setup)
	$(CARGO) build --release --bin vv --locked --target x86_64-unknown-linux-musl

install: build vv ## Install banned-words-service and vv to $(PREFIX)/bin (default /usr/local/bin)
	install -d "$(DESTDIR)$(PREFIX)/bin"
	install -m 0755 target/release/banned-words-service "$(DESTDIR)$(PREFIX)/bin/banned-words-service"
	install -m 0755 target/release/vv "$(DESTDIR)$(PREFIX)/bin/vv"

test: ## Run the full test suite (cargo test --locked)
	$(CARGO) test --locked

bench: ## Compile benchmarks without running (cargo bench --no-run --locked)
	$(CARGO) bench --no-run --locked

lint: ## cargo fmt --check and cargo clippy -- -D warnings
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets --locked -- -D warnings

podman: ## Build the container image (rootless podman; see footer for override), tagged with the LDNOOBW SHA
	@if [ -z "$(LIST_SHA)" ]; then \
	  echo "error: could not read LDNOOBW SHA from vendor/ldnoobw; run: git submodule update --init --recursive" >&2; \
	  exit 1; \
	fi
	$(CONTAINER) build \
	  -f deploy/Containerfile \
	  --build-arg LIST_VERSION=$(LIST_SHA) \
	  --build-arg REVISION=$(REVISION) \
	  -t $(IMAGE_NAME):$(LIST_SHA) \
	  -t $(IMAGE_NAME):latest \
	  .

run: ## Run locally via cargo run with a dev-only VV_API_KEYS
	VV_API_KEYS="$(DEV_API_KEY)" $(CARGO) run --release --locked

install-tools: ## Install pinned dev tools (oha for load tests)
	$(CARGO) install --locked --version $(OHA_VERSION) oha

release-check: lint test bench podman ## Full pre-tag gate: lint, test, bench-compile, container image
	@echo ""
	@echo "release-check OK"
	@echo "  image: $(IMAGE_NAME):$(LIST_SHA)"
	@echo "  revision: $(REVISION)"
	@echo ""
	@echo "Next: follow RELEASE.md to capture a load-test report, tag v1.0.0, and push the image."
