# One entry point for local checks and CI, so both run exactly the same commands.
# `make check` is what to run before pushing.

CARGO ?= cargo

.PHONY: check build fmt fmt-check lint test docs

check: fmt-check lint test

build:
	$(CARGO) build --workspace --all-targets --locked

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

lint:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --workspace --no-deps --locked

test:
	$(CARGO) test --workspace --locked

docs:
	python3 scripts/vendor-docs.py
