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

//! Tests for `#[derive(Builder)]` (Lombok `@Builder`) and `#[derive(Mapper)]`
//! (MapStruct `@Mapper`).

use firefly::prelude::*;

#[derive(Builder, Debug, PartialEq)]
struct OpenAccount {
    owner: String,
    #[builder(into)]
    note: String,
    #[builder(default)]
    overdraft: i64,
    #[builder(default_expr = "100")]
    limit: i64,
}

#[test]
fn builder_sets_fields_with_required_and_defaults() {
    let cmd = OpenAccount::builder()
        .owner("ada".to_string())
        .note("vip") // `into` setter accepts &str
        .build()
        .expect("all required fields set");
    assert_eq!(
        cmd,
        OpenAccount {
            owner: "ada".into(),
            note: "vip".into(),
            overdraft: 0,   // #[builder(default)]
            limit: 100,     // #[builder(default_expr = "100")]
        }
    );
}

#[test]
fn builder_errors_on_missing_required_field() {
    // `owner` and `note` are required.
    let err = OpenAccount::builder().owner("ada".to_string()).build();
    assert!(err.is_err());
    assert!(err.unwrap_err().contains("note"));
}

// ── Mapper (MapStruct) ──────────────────────────────────────────────────────

#[derive(Debug)]
struct AccountEntity {
    id: u64,
    owner_name: String,
    balance_cents: i64,
}

fn dollars(cents: i64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

#[derive(Mapper, Debug, PartialEq)]
#[firefly(from = "AccountEntity")]
struct AccountDto {
    id: u64,
    #[firefly(rename = "owner_name")]
    owner: String,
    #[firefly(rename = "balance_cents", with = "dollars")]
    balance: String,
    #[firefly(default)]
    note: String,
}

#[test]
fn mapper_generates_typed_from_impl() {
    let entity = AccountEntity {
        id: 7,
        owner_name: "ada".into(),
        balance_cents: 1234,
    };
    let dto: AccountDto = entity.into();
    assert_eq!(
        dto,
        AccountDto {
            id: 7,
            owner: "ada".into(),
            balance: "$12.34".into(),
            note: String::new(),
        }
    );
}
