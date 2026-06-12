// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! gRPC channel-builder tests, adapted from pyfly's
//! `test_protocols.py::test_grpc_*`. Per the porting brief these are
//! builder-only — no server, no codegen: they exercise target
//! validation, option chaining, and lazy channel construction (which
//! performs no network I/O). Requires the `grpc` feature.
#![cfg(feature = "grpc")]

use std::time::Duration;

use firefly_client::{GrpcBuilder, GrpcError};

// --- pyfly: test_grpc_builder_requires_target -----------------------------

#[test]
fn grpc_builder_requires_target() {
    let err = GrpcBuilder::new("").endpoint().expect_err("empty target");
    assert!(matches!(err, GrpcError::MissingTarget));
    assert_eq!(err.to_string(), "grpc: builder requires a target");
}

// --- pyfly: test_grpc_builder_chains_options ------------------------------

#[test]
fn grpc_builder_chains_target_and_secure_flag() {
    let builder = GrpcBuilder::new("localhost:50051")
        .secured(false)
        .with_connect_timeout(Duration::from_secs(5));
    assert_eq!(builder.target(), "localhost:50051");
    assert!(!builder.is_secured());
}

// `connect_lazy` builds a hyper connector that needs the Tokio reactor,
// so these run on a runtime even though they perform no network I/O.
#[tokio::test]
async fn grpc_lazy_channel_builds_without_network() {
    // connect_lazy performs no I/O, so it succeeds for any well-formed
    // target even with nothing listening — the channel-construction
    // path the brief scopes the gRPC test to.
    let channel = GrpcBuilder::new("http://127.0.0.1:50051")
        .connect_lazy()
        .expect("lazy channel");
    // A tonic Channel is cheap to clone; just prove we got one.
    let _clone = channel.clone();
}

#[tokio::test]
async fn grpc_bare_host_port_target_is_normalized() {
    // A bare host:port (no scheme) must be accepted and defaulted to
    // http:// so callers can pass the same string pyfly uses.
    let channel = GrpcBuilder::new("127.0.0.1:50051")
        .connect_lazy()
        .expect("lazy channel from bare target");
    let _ = channel;
}

#[test]
fn grpc_invalid_target_is_rejected() {
    let err = GrpcBuilder::new("http://[::bad uri")
        .endpoint()
        .expect_err("invalid target");
    assert!(matches!(err, GrpcError::InvalidTarget(_)));
}

#[cfg(not(feature = "grpc-tls"))]
#[test]
fn grpc_secured_without_tls_feature_is_unsupported() {
    let err = GrpcBuilder::new("https://127.0.0.1:50051")
        .secured(true)
        .endpoint()
        .expect_err("secured without tls feature");
    assert!(matches!(err, GrpcError::TlsUnsupported));
}
