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

//! # Lumen Ledger — `-core`
//!
//! The business layer (firefly-oss's `-core` Maven module): the
//! [`WalletService`] (`@Service`) that orchestrates the use cases, the
//! [`WalletMapper`] (`@Mapper`) that translates between the `-interfaces` DTOs
//! and the `-models` entity, and a genuine [`WalletNumberGenerator`]
//! (`@Component`) collaborator. Every type here is a **DI bean** discovered by
//! `container.scan()`; the service autowires its collaborators and programs
//! against the repository's
//! [`ReactiveCrudRepository`](firefly::data::ReactiveCrudRepository) trait +
//! derived queries.

#![forbid(unsafe_code)]

pub mod components;
pub mod mappers;
pub mod services;

// ---- convenience flat re-exports ------------------------------------------
pub use components::WalletNumberGenerator;
pub use mappers::wallet::v1::WalletMapper;
pub use services::wallet::v1::{ServiceError, WalletService, WalletServiceImpl};
