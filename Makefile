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
