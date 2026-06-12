//! Legacy SOAP / gRPC / WebSocket placeholder builders (Go-parity).
//!
//! These predate the typed protocol clients in [`graphql`](crate::graphql),
//! [`soap`](crate::soap), [`websocket`](crate::websocket), and
//! [`grpc`](crate::grpc). They are retained verbatim for backward
//! compatibility: the `new_*(endpoint)` free functions still fail with
//! [`ClientError::TransportNotRegistered`], exactly as in the Go port's
//! `NewSOAP` / `NewGRPC` / `NewWebSocket` stubs.
//!
//! New code should use the feature-gated builders
//! ([`SoapBuilder`](crate::SoapBuilder),
//! [`WsBuilder`](crate::WsBuilder), [`GrpcBuilder`](crate::GrpcBuilder),
//! [`GraphQlBuilder`](crate::GraphQlBuilder)) instead.

use crate::error::ClientError;

/// Placeholder handle returned by the legacy [`new_soap`] sentinel.
/// Never constructed — kept only so the function's signature is stable.
#[derive(Debug)]
#[non_exhaustive]
pub struct SoapPlaceholder;

/// Placeholder handle returned by the legacy [`new_grpc`] sentinel.
/// Never constructed — kept only so the function's signature is stable.
#[derive(Debug)]
#[non_exhaustive]
pub struct GrpcPlaceholder;

/// Placeholder handle returned by the legacy [`new_websocket`] sentinel.
/// Never constructed — kept only so the function's signature is stable.
#[derive(Debug)]
#[non_exhaustive]
pub struct WebSocketPlaceholder;

/// Legacy SOAP entry point — always returns
/// [`ClientError::TransportNotRegistered`], mirroring Go's `NewSOAP`.
/// Use [`SoapBuilder`](crate::SoapBuilder) for the real client.
pub fn new_soap(_endpoint: &str) -> Result<SoapPlaceholder, ClientError> {
    Err(ClientError::TransportNotRegistered)
}

/// Legacy gRPC entry point — always returns
/// [`ClientError::TransportNotRegistered`], mirroring Go's `NewGRPC`.
/// Use `GrpcBuilder` (feature `grpc`) for the real channel factory.
pub fn new_grpc(_target: &str) -> Result<GrpcPlaceholder, ClientError> {
    Err(ClientError::TransportNotRegistered)
}

/// Legacy WebSocket entry point — always returns
/// [`ClientError::TransportNotRegistered`], mirroring Go's
/// `NewWebSocket`. Use `WsBuilder` (feature `websocket`) for the real
/// client.
pub fn new_websocket(_url: &str) -> Result<WebSocketPlaceholder, ClientError> {
    Err(ClientError::TransportNotRegistered)
}
