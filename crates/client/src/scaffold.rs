//! SOAP / gRPC / WebSocket placeholder builders.
//!
//! The framework ships only the REST transport in-tree; the other
//! builders share the same `new_*(endpoint)` shape and currently fail
//! with [`ClientError::TransportNotRegistered`] until the dedicated
//! transport modules (planned: `client-soap`, `client-grpc`,
//! `client-ws`) are wired into the application — exactly as in the Go
//! port's `NewSOAP` / `NewGRPC` / `NewWebSocket` stubs.

use crate::error::ClientError;

/// Placeholder SOAP client type. Never constructed by this crate —
/// [`new_soap`] fails with [`ClientError::TransportNotRegistered`]
/// until the dedicated SOAP transport module is registered.
#[derive(Debug)]
#[non_exhaustive]
pub struct SoapClient;

/// Placeholder gRPC client type. Never constructed by this crate —
/// [`new_grpc`] fails with [`ClientError::TransportNotRegistered`]
/// until the dedicated gRPC transport module is registered.
#[derive(Debug)]
#[non_exhaustive]
pub struct GrpcClient;

/// Placeholder WebSocket client type. Never constructed by this crate —
/// [`new_websocket`] fails with [`ClientError::TransportNotRegistered`]
/// until the dedicated WebSocket transport module is registered.
#[derive(Debug)]
#[non_exhaustive]
pub struct WebSocketClient;

/// Placeholder builder for SOAP — always returns
/// [`ClientError::TransportNotRegistered`], mirroring Go's `NewSOAP`.
pub fn new_soap(_endpoint: &str) -> Result<SoapClient, ClientError> {
    Err(ClientError::TransportNotRegistered)
}

/// Placeholder builder for gRPC — always returns
/// [`ClientError::TransportNotRegistered`], mirroring Go's `NewGRPC`.
pub fn new_grpc(_target: &str) -> Result<GrpcClient, ClientError> {
    Err(ClientError::TransportNotRegistered)
}

/// Placeholder builder for WebSocket — always returns
/// [`ClientError::TransportNotRegistered`], mirroring Go's
/// `NewWebSocket`.
pub fn new_websocket(_url: &str) -> Result<WebSocketClient, ClientError> {
    Err(ClientError::TransportNotRegistered)
}
