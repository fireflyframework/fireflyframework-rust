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

//! The [`ServiceError`] type.

/// The failures a wallet use case can surface. The web layer maps
/// these onto RFC 9457 problem responses (404 / 422 / 500).
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// No wallet with that id.
    #[error("wallet not found")]
    NotFound,
    /// A business-rule violation (non-positive amount, insufficient
    /// funds, inactive wallet).
    #[error("{0}")]
    Validation(String),
    /// A persistence failure.
    #[error("{0}")]
    Backend(String),
}
