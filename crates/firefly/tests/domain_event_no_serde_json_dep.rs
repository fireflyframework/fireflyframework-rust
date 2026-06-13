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

//! Regression test for the `#[derive(DomainEvent)]` one-dependency contract.
//!
//! The generated `to_domain_event(...)` used to JSON-encode the payload with a
//! bare `::serde_json::to_vec(self)`, which forced any user crate that depends
//! only on the `firefly` facade (+ `serde`) to ALSO add `serde_json` itself,
//! or the call would fail with `use of undeclared crate or module serde_json`.
//!
//! The fix routes the payload encoder through the facade-re-exported
//! `::firefly::__rt::serde_json`. This module deliberately imports ONLY
//! `firefly` (+ `serde` for the `Serialize`/`Deserialize` derives, the one
//! direct ecosystem crate a Firefly service is expected to write against) and
//! NEVER names the bare `serde_json` crate — so it compiles end-to-end purely
//! through the one facade dependency.

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, DomainEvent)]
struct AccountOpened {
    owner: String,
}

/// Deriving `DomainEvent` and calling the generated `to_domain_event(...)`
/// compiles and runs with no `serde_json` import in this crate — the payload
/// encoder resolves through `::firefly::__rt::serde_json`.
#[test]
fn to_domain_event_compiles_without_user_serde_json_dep() {
    let ev = AccountOpened {
        owner: "ada".into(),
    };

    let wire = ev.to_domain_event("acc-1", "Account", 1);
    assert_eq!(wire.aggregate_id, "acc-1");
    assert_eq!(wire.aggregate_type, "Account");
    assert_eq!(wire.event_type, "AccountOpened");
    assert_eq!(wire.version, 1);

    // The payload round-trips back — decoded through the SAME facade-routed
    // `serde_json`, proving the contract path is the only JSON dependency a
    // service needs.
    let decoded: AccountOpened =
        firefly::__rt::serde_json::from_slice(&wire.payload).expect("payload decodes");
    assert_eq!(decoded.owner, "ada");
}
