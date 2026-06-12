# `firefly-integration-tests`

> **Tier:** Tests · **Status:** Full · **Go module:** `tests` · **Java original:** `firefly-it` (integration tests) · **.NET project:** `tests/FireflyFramework.Tests/`

## Overview

`firefly-integration-tests` holds the framework's **cross-module
integration tests** — the suite that proves several crates compose
end-to-end. The crate exports nothing (`src/lib.rs` is a doc-only
stub); the suite lives in `tests/integration_test.rs`.

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
