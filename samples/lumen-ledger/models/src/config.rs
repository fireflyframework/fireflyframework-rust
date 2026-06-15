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

//! The persistence `@Configuration` — declares the repository as an async bean.

mod wallet_persistence_config;

pub use wallet_persistence_config::WalletPersistenceConfig;
// The datasource bootstrap is crate-internal (the async bean + the model tests).
#[allow(unused_imports)]
pub(crate) use wallet_persistence_config::{connect_and_migrate, connect_and_migrate_url};
