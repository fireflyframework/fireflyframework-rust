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

//! # Lumen Ledger — `-sdk`
//!
//! A typed outbound client for the wallet API (firefly-oss's `-sdk` module),
//! built over [`firefly_client::RestClient`]. It **reuses the `-interfaces`
//! DTOs** so a caller never re-declares the contract — the same
//! `CreateWalletRequest` / `WalletResponse` the server validates and returns.
//!
//! This is the hand-written equivalent of what `firefly openapi-client` emits
//! from the service's OpenAPI document; see the "Layered Microservices" chapter.

#![forbid(unsafe_code)]

pub mod client;

pub use client::WalletClient;
