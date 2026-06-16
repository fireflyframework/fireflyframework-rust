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

//! # Lumen Ledger — `-interfaces`
//!
//! The **public API contract** of the wallet service: the request/response DTOs
//! and the domain enums every other module (and every external SDK consumer)
//! shares. This is the Rust analog of firefly-oss's `…-interfaces` Maven module
//! — it depends on nothing but the framework, so the contract can be published
//! and reused without dragging in persistence or web code.
//!
//! Types are organised by `<domain>/v1` exactly like the Java reference
//! (`dtos::wallet::v1`, `enums::wallet::v1`), one public type per file. Every
//! DTO carries `#[derive(Schema)]` (so the OpenAPI generator emits its
//! `#/components/schemas/*` with no runtime reflection) and the write-side DTOs
//! add `#[derive(Validate)]` (JSR-380 bean validation, enforced at the web edge
//! by the `Valid<T>` extractor).
//!
//! Convenience flat re-exports are provided at the crate root.

#![forbid(unsafe_code)]

pub mod dtos;
pub mod enums;

// ---- convenience flat re-exports ------------------------------------------
pub use dtos::wallet::v1::{
    AmountRequest, CreateWalletRequest, TransferRequest, WalletFilter, WalletResponse,
};
pub use enums::wallet::v1::WalletStatus;

#[cfg(test)]
mod tests {
    use super::*;
    use firefly::prelude::Validate;

    #[test]
    fn create_request_requires_owner_and_currency() {
        let blank = CreateWalletRequest::default();
        assert!(blank.validate().is_err(), "blank owner/currency must fail");

        let ok = CreateWalletRequest {
            owner: "ada".into(),
            currency: "EUR".into(),
            opening_balance: 100,
        };
        assert!(ok.validate().is_ok(), "a complete request validates");
    }

    #[test]
    fn status_round_trips_through_its_token() {
        for s in [
            WalletStatus::Active,
            WalletStatus::Frozen,
            WalletStatus::Closed,
        ] {
            assert_eq!(WalletStatus::from_token(s.as_str()), s);
        }
        // Forward-compatible: an unknown token reads as Active, never panics.
        assert_eq!(WalletStatus::from_token("mystery"), WalletStatus::Active);
    }

    #[test]
    fn response_serialises_with_camel_case_account_number() {
        let json = serde_json::to_string(&WalletResponse {
            id: "id".into(),
            account_number: "WAL-1".into(),
            owner: "ada".into(),
            balance: 100,
            currency: "EUR".into(),
            status: WalletStatus::Active,
            version: 1,
        })
        .unwrap();
        assert!(json.contains("\"accountNumber\":\"WAL-1\""));
        assert!(json.contains("\"status\":\"active\""));
    }
}
