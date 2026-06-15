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

//! The [`WalletNumberGenerator`] `@Component`.

use std::sync::atomic::{AtomicU64, Ordering};

use firefly::prelude::*;

/// A genuine `@Component`: hands out sequential, human-facing wallet account
/// numbers (`WAL-00001`, `WAL-00002`, …). Autowired into the service — a
/// plain framework-managed singleton with no configuration.
#[derive(Component)]
pub struct WalletNumberGenerator {
    counter: AtomicU64,
}

impl Default for WalletNumberGenerator {
    fn default() -> Self {
        Self {
            counter: AtomicU64::new(1),
        }
    }
}

impl WalletNumberGenerator {
    /// The next account number in sequence.
    pub fn next_number(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("WAL-{n:05}")
    }
}
