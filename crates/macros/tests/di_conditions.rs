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

//! Bean-dependent conditional gating (pass 2) exercised through a real
//! `Container::scan()`. Isolated in its own test binary so the `inventory`
//! universe contains only these beans and the two-pass ordering is observable.

use firefly::prelude::*;

// A default implementation that registers only when the user did NOT provide
// their own — Spring `@ConditionalOnMissingBean`.
#[derive(Service, Default)]
#[firefly(condition_on_missing_bean = "CustomMailer")]
struct DefaultMailer;

// The user's own bean (always registered). Its presence must suppress the
// default above via the on_missing_bean pass-2 evaluation.
#[derive(Service, Default)]
struct CustomMailer;

// Only registers when a `CustomMailer` exists — `@ConditionalOnBean`.
#[derive(Service, Default)]
#[firefly(condition_on_bean = "CustomMailer")]
struct MailMetrics;

#[test]
fn on_missing_bean_and_on_bean_evaluate_in_pass_two() {
    let c = Container::new();
    let n = c.scan();
    assert!(n >= 2, "scanned {n} beans");

    // CustomMailer is unconditional → present.
    assert!(c.resolve::<CustomMailer>().is_ok());

    // DefaultMailer is suppressed because CustomMailer exists.
    assert!(
        c.resolve::<DefaultMailer>().is_err(),
        "on_missing_bean must suppress the default when CustomMailer is present"
    );

    // MailMetrics registers because CustomMailer exists (on_bean satisfied).
    assert!(
        c.resolve::<MailMetrics>().is_ok(),
        "on_bean must register MailMetrics when CustomMailer is present"
    );
}
