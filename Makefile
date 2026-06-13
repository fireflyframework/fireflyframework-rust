.PHONY: help build test test-v clippy fmt fmt-check sample cli cli-install clean ci \
        infra-up infra-down infra-status test-integration book book-serve \
        book-pdf book-epub book-dist

VERSION := 26.6.2

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
	@echo "  cli        run the firefly developer CLI (make cli ARGS='info')"
	@echo "  cli-install install the firefly binary into ~/.cargo/bin"
	@echo "  clean      cargo clean"
	@echo "  ci         fmt-check + clippy + build + test (whole workspace)"
	@echo ""
	@echo "Integration (real backing services via Docker):"
	@echo "  infra-up          start postgres/redis/rabbitmq/kafka/keycloak/s3/blob/smtp"
	@echo "  infra-down        stop and remove the stack"
	@echo "  infra-status      show service health"
	@echo "  test-integration  run the env-gated real-infra tests against the stack"
	@echo ""
	@echo "Docs:"
	@echo "  book        build the mdBook documentation site (docs/book)"
	@echo "  book-serve  serve the book locally with live reload"
	@echo "  book-pdf    render the book to docs/book/dist/*.pdf  (pandoc + tectonic)"
	@echo "  book-epub   render the book to docs/book/dist/*.epub (pandoc)"
	@echo "  book-dist   render both the PDF and the EPUB"

# ---- Integration test infrastructure -------------------------------------
# Host ports (chosen to avoid collisions with other local services):
#   postgres 5442, redis 6379, rabbitmq 5672, kafka 9092, keycloak 8095,
#   s3/localstack 4566, azurite blob 10000, mailhog smtp 1026 (api 8026).
INTEGRATION_ENV := \
	FIREFLY_TEST_POSTGRES_URL=postgres://firefly:firefly@localhost:5442/firefly \
	FIREFLY_TEST_REDIS_URL=redis://localhost:6379 \
	FIREFLY_TEST_RABBITMQ_URL=amqp://guest:guest@localhost:5672/%2f \
	FIREFLY_TEST_KAFKA_BROKERS=localhost:9092 \
	FIREFLY_TEST_KEYCLOAK_URL=http://localhost:8095 \
	FIREFLY_TEST_S3_ENDPOINT=http://localhost:4566 \
	FIREFLY_TEST_AZURITE_URL=http://localhost:10000 \
	FIREFLY_TEST_SMTP_ADDR=localhost:1026 \
	FIREFLY_TEST_MAILHOG_API=http://localhost:8026

INTEGRATION_CRATES := \
	-p firefly-cache-postgres -p firefly-eda-postgres -p firefly-scheduling -p firefly-security \
	-p firefly-cache-redis -p firefly-eda-redis -p firefly-eda-kafka -p firefly-eda-rabbitmq \
	-p firefly-idp-keycloak -p firefly-notifications-smtp -p firefly-ecm-storage-aws \
	-p firefly-ecm-storage-azure -p firefly-data -p firefly-sample-reactive-banking

infra-up:
	docker compose up -d --wait
	@echo "Configuring Keycloak master realm for HTTP (dev only)..."
	@docker compose exec -T keycloak /opt/keycloak/bin/kcadm.sh config credentials \
		--server http://localhost:8095 --realm master --user admin --password admin >/dev/null 2>&1 || true
	@docker compose exec -T keycloak /opt/keycloak/bin/kcadm.sh update realms/master \
		-s sslRequired=NONE >/dev/null 2>&1 || true
	@echo "Infra ready. Run: make test-integration"

infra-down:
	docker compose down -v

infra-status:
	docker compose ps

test-integration:
	$(INTEGRATION_ENV) cargo test --no-fail-fast $(INTEGRATION_CRATES)

book:
	mdbook build docs/book

book-serve:
	mdbook serve docs/book --open

# Polished PDF/EPUB editions rendered from the mdBook chapters via
# pandoc + tectonic. Artifacts land in docs/book/dist/ and are committed.
book-pdf:
	bash docs/book/build-book.sh --pdf

book-epub:
	bash docs/book/build-book.sh --epub

book-dist:
	bash docs/book/build-book.sh

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

# Run the `firefly` developer CLI: `make cli ARGS="doctor"`, `make cli ARGS="info"`.
cli:
	cargo run -p firefly-cli --bin firefly -- $(ARGS)

# Install the `firefly` binary into ~/.cargo/bin.
cli-install:
	cargo install --path crates/cli

clean:
	cargo clean

# `--workspace` covers every member, so `ci` scales with the workspace.
ci: fmt-check clippy build test
