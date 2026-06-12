.PHONY: help build test test-v clippy fmt fmt-check sample clean ci

VERSION := 26.6.1

# cargo may live outside the default make PATH (e.g. Homebrew installs).
export PATH := $(PATH):/opt/homebrew/bin:$(HOME)/.cargo/bin

help:
	@echo "Firefly Framework for Rust — v$(VERSION)"
	@echo ""
	@echo "Targets:"
	@echo "  build      cargo build --workspace"
	@echo "  test       cargo test --workspace"
	@echo "  test-v     cargo test --workspace -- --nocapture"
	@echo "  clippy     cargo clippy --workspace --all-targets -- -D warnings"
	@echo "  fmt        cargo fmt --all"
	@echo "  fmt-check  verify rustfmt cleanliness"
	@echo "  sample     run samples/orders"
	@echo "  clean      cargo clean"
	@echo "  ci         fmt-check + clippy + build + test"

build:
	cargo build --workspace

test:
	cargo test --workspace

test-v:
	cargo test --workspace -- --nocapture

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

sample:
	cargo run -p firefly-sample-orders

clean:
	cargo clean

ci: fmt-check clippy build test
