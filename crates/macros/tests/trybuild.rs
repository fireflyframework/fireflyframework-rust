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

//! UI tests: compile-pass cases prove the macros expand cleanly against the
//! facade; compile-fail cases pin the diagnostics for misuse (e.g. a
//! `#[scheduled]` with no trigger, a controller with no verb methods).

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass/*.rs");
    t.compile_fail("tests/ui/fail/*.rs");
    // The `#[http_client]` macro keeps its own pass/fail corpus so each return
    // shape and binding rule (and each locked diagnostic) is exercised in
    // isolation from the rest of the macro suite.
    t.pass("tests/ui/http_client/pass/*.rs");
    t.compile_fail("tests/ui/http_client/fail/*.rs");
}
