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

//! The [`AmountRequest`] DTO.

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

/// `POST /…/deposit` & `/…/withdraw` body — a single minor-unit amount.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Schema)]
pub struct AmountRequest {
    /// The amount to move, in minor units (cents); must be `> 0`
    /// (enforced by the service, surfaced as `422`).
    pub amount: i64,
}
