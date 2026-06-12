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

//! Ported from pyfly `tests/shell/test_models.py` — `ShellParam` and
//! `CommandResult` data models.

use firefly_shell::{CommandResult, ShellParam, Value, ValueType};

// ---------------------------------------------------------------------------
// ShellParam
// ---------------------------------------------------------------------------

#[test]
fn positional_param() {
    let p = ShellParam::arg("name", ValueType::Str);
    assert_eq!(p.name, "name");
    assert_eq!(p.value_type, ValueType::Str);
    assert!(!p.is_option);
    // pyfly: default is MISSING -> Rust: None.
    assert!(p.default.is_none());
}

#[test]
fn flag_param() {
    let p = ShellParam::flag("verbose");
    assert!(p.is_flag);
    assert!(p.is_option);
}

#[test]
fn option_with_default() {
    let p = ShellParam::option("count", ValueType::Int).with_default(Value::Int(10));
    assert_eq!(p.default, Some(Value::Int(10)));
}

#[test]
fn param_with_choices() {
    let p = ShellParam::option("color", ValueType::Str).with_choices(["red", "green", "blue"]);
    assert_eq!(
        p.choices,
        Some(vec![
            "red".to_string(),
            "green".to_string(),
            "blue".to_string()
        ])
    );
}

// ---------------------------------------------------------------------------
// CommandResult
// ---------------------------------------------------------------------------

#[test]
fn success_by_default() {
    let r = CommandResult::default();
    assert_eq!(r.exit_code, 0);
    assert!(r.is_success());
}

#[test]
fn failure() {
    let r = CommandResult::new("boom", 1);
    assert!(!r.is_success());
    assert_eq!(r.output, "boom");
}

#[test]
fn default_exit_code_is_zero() {
    let r = CommandResult::ok("ok");
    assert_eq!(r.exit_code, 0);
}
