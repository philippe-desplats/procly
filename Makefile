# One entry point for local checks and CI, so both run exactly the same commands.
# `make check` is what to run before pushing.

CARGO ?= cargo
BUN ?= bun
APP ?= apps/desktop

.PHONY: check build fmt fmt-check lint test docs

check: fmt-check lint test

build:
	$(CARGO) build --workspace --all-targets --locked

fmt:
	$(CARGO) fmt --all
	cd $(APP) && $(BUN) run fmt

fmt-check:
	$(CARGO) fmt --all --check
	cd $(APP) && $(BUN) run fmt:check

lint:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --workspace --no-deps --locked
	cd $(APP) && $(BUN) run lint

# `cargo test` exports the TypeScript types of the protocol (ADR 0015), so a protocol change the frontend has not
# followed shows up as a diff here, a new type as an untracked file.
test:
	$(CARGO) test --workspace --locked
	cd $(APP) && $(BUN) run test
	git diff --exit-code $(APP)/src/protocol
	test -z "$$(git ls-files --others --exclude-standard $(APP)/src/protocol)" || { echo "untracked protocol types, commit them"; exit 1; }

docs:
	python3 scripts/vendor-docs.py

# The app embeds the CLI as a sidecar (`Procly.app/Contents/MacOS/procly`). The sidecar is declared in
# `tauri.bundle.conf.json`, not in `tauri.conf.json`: Tauri copies a declared sidecar over `target/<profile>/procly`
# at every build of the app, which is the CLI the tests run. Everything that builds a bundle goes through `bundle`.
# `PROFILE=release` embeds an optimised CLI, `TAURI_ARGS` goes to `tauri build` (`--bundles app`, `--features e2e`).
PROFILE ?= debug
TRIPLE := $(shell rustc --print host-tuple)
SIDECAR := $(APP)/src-tauri/binaries/procly-$(TRIPLE)
TAURI_ARGS ?=

.PHONY: sidecar bundle

sidecar:
	$(CARGO) build --package procly --locked $(if $(filter release,$(PROFILE)),--release)
	mkdir -p $(dir $(SIDECAR))
	cp target/$(PROFILE)/procly $(SIDECAR)

bundle: sidecar
	cd $(APP) && $(BUN) run tauri build $(if $(filter debug,$(PROFILE)),--debug) --config src-tauri/tauri.bundle.conf.json $(TAURI_ARGS)

# The end-to-end tests (ADR 0016) drive the real app, built with the `e2e` feature, against a real Daemon. They are not
# part of `check`. The runner isolates the Daemon in temporary directories and the app finds its `procly` beside its
# executable, so the build goes through `bundle` like every build that embeds the CLI. A macOS bundle is an `.app`,
# Linux has no `app` bundle type and runs the executable the build leaves in `target`.
ifeq ($(shell uname -s),Darwin)
E2E_BUNDLES := --bundles app
E2E_APP := $(CURDIR)/target/debug/bundle/macos/Procly.app/Contents/MacOS/procly-desktop
else
E2E_BUNDLES := --no-bundle
E2E_APP := $(CURDIR)/target/debug/procly-desktop
endif

.PHONY: e2e

e2e:
	$(MAKE) bundle TAURI_ARGS="--features e2e $(E2E_BUNDLES)"
	cd $(APP) && PROCLY_E2E_APP="$(E2E_APP)" PROCLY_E2E_TEST_SERVER="$(CURDIR)/target/debug/procly-test-server" $(BUN) run e2e
