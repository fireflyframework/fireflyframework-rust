# `firefly-integration-tests`

> **Tier:** Tests · **Status:** Full · **Go module:** `tests` · **Java original:** `firefly-it` (integration tests) · **.NET project:** `tests/FireflyFramework.Tests/`

## Overview

`firefly-integration-tests` holds the framework's **cross-module
integration tests** — the suite that proves several crates compose
end-to-end. The crate exports nothing (`src/lib.rs` is a doc-only
stub); the suite lives in two files:

* `tests/integration_test.rs` — the Go-parity scenarios (callbacks,
  webhooks, saga, health, correlation).
* `tests/pyfly_parity_test.rs` — the **pyfly-parity scenarios** added
  alongside the new crates, mirroring `fireflyframework-pyfly`'s
  cross-cutting suite (`tests/test_integration.py`,
  `tests/test_hexagonal.py`, `tests/integration/`).

Ports of the Go suite (named after their Go counterparts):

| Test                                | Go counterpart                  | What it exercises                                                                                  |
|-------------------------------------|---------------------------------|-----------------------------------------------------------------------------------------------------|
| `end_to_end_command_to_callback`    | `TestEndToEndCommandToCallback` | starter-core + cqrs + callbacks (HMAC-signed delivery + audit), re-verified by the webhooks validator |
| `webhook_ingestion_round_trip`      | `TestWebhookIngestionRoundTrip` | webhooks core + web (HMAC validation, 401 on mismatch, processor dispatch)                            |
| `saga_rolls_back_on_failure`        | `TestSagaRollsBackOnFailure`    | orchestration `Saga` compensation rollback in reverse order, with a `firefly-kernel` error as cause   |

Rust-specific seams (cross-module behavior the Go suite left to unit
tests, or that only exists at crate boundaries):

| Test                                                       | What it exercises                                                              |
|------------------------------------------------------------|--------------------------------------------------------------------------------|
| `saga_happy_path_completes`                                | The forward half of the saga roundtrip: all steps run, nothing compensates    |
| `cqrs_command_and_query_roundtrip_through_starter_core`    | Command write + query read through the pre-wired bus and validation middleware |
| `health_composite_over_starter_core`                       | Observability indicator bridged onto `/actuator/health` (DEGRADED rollup)      |
| `correlation_id_flows_from_http_request_to_callback_delivery` | One id end to end: kernel task-local → web middleware → callbacks → receiver |
| `webhook_processor_failure_dead_letters_and_returns_500`   | Pipeline DLQ capture + 500 surface on processor failure                        |
| `callback_retry_audit_trail_records_every_attempt`         | Retry budget consumption with one ordered audit row per attempt                |

Pyfly-parity seams (`tests/pyfly_parity_test.rs`) — each exercises the
new crates together, mirroring pyfly's cross-cutting tests:

| Test                                                              | What it exercises                                                                                              |
|-------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------|
| `web_cors_headers_csrf_and_metrics_compose_through_starter_core`  | web CORS + security-headers + CSRF + request-metrics layered over `Core::apply_middleware`, asserting headers + an actuator `MetricRegistry` counter |
| `jwks_bearer_gates_filter_chain_route_with_role_hierarchy`        | security JWKS verifier accepting an in-process-minted RS256 token, gating a `FilterChain` route via `RoleHierarchy` (`ADMIN > USER`) |
| `workflow_persistence_wait_for_signal_and_recovery`               | orchestration workflow with persistence + a `wait_for_signal` step driven to completion, plus `RecoveryService` repairing a stale run |
| `cqrs_authorization_via_context_and_eda_cache_invalidation`       | CQRS authorization middleware denying then allowing a command via `ExecutionContext`, plus an EDA event evicting a cached query |
| `eventsourcing_outbox_publishes_onto_eda_broker`                  | eventsourcing transactional outbox relaying an aggregate event onto an in-memory EDA broker via `EdaSink`     |
| `eda_subscribe_group_round_robin_and_wrap_listener_dlq`           | EDA `subscribe_group` round-robin competing consumers + `wrap_listener` dead-lettering an exhausted event     |
| `notifications_email_opt_out_and_template_precedence`             | `DefaultEmailService` opt-out suppression + local-template precedence over a `DummyEmailProvider`             |
| `config_placeholder_reload_and_property_source_masking`           | config `${...}` placeholder resolution + `ReloadableConfig` reload + masked `property_sources()` end to end   |

> The optional admin `mount()` smoke (scenario 9 in the brief) is
> deferred: `firefly-admin`'s public surface is still a version-stamp
> placeholder with no `mount()` yet, so there is nothing to oneshot. It
> will land here once admin ships its router (admin is a leaf crate, so
> it can be added as a dev-dependency without a cycle).

Per-crate unit tests live alongside their sources as `#[cfg(test)]`
modules (the Rust idiom mirroring Go's `_test.go` files). This crate is
reserved for tests that span three or more crates.

## Run

```bash
cargo test -p firefly-integration-tests
```

The suite has no external dependencies — every collaborator is wired
in-memory or on a loopback socket: the callback receiver is a real
`axum` server bound to `127.0.0.1:0` (the analog of the Go suite's
`httptest.NewServer`), webhook ingestion and the actuator are driven
in-process through `tower::ServiceExt::oneshot`, and the callback
store, DLQ, cache, and bus are the framework's in-memory
implementations.

## Adding a new integration test

* The test must exercise at least **three** crates (otherwise it
  belongs in the originating crate's `#[cfg(test)]` module).
* Prefer `tower::ServiceExt::oneshot` for in-process HTTP
  collaborators; bind a real `tokio::net::TcpListener` on port `0`
  only when an out-of-process HTTP client (e.g. the callbacks
  dispatcher's `reqwest`) must cross a socket — faster boot than
  spinning up a full `firefly_lifecycle::Application`.
* Use the `firefly-testkit` signers (`sign_hmac`, `sign_stripe`,
  `sign_github`, `sign_twilio`) — or the local `sign_sha256` helper —
  for signature generation; the wire shape is guaranteed identical to
  the framework's validators.
* Keep deliveries fast: configure `DispatcherConfig` with a 1 ms
  `initial_delay` so retry tests stay well under the suite's latency
  budget.
