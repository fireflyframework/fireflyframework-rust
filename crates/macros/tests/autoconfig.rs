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

//! Spring-Boot-parity DI tests for the pieces that make starters "just work":
//!
//! - `#[bean]` factory methods are **auto-registered by `Container::scan()`** —
//!   no manual `firefly_register_beans` call.
//! - `#[derive(AutoConfiguration)]` + `#[bean(condition_on_missing_bean = …)]`
//!   contributes defaults that **yield to any user-defined bean** of the same
//!   type (the `@ConditionalOnMissingBean` rule).
//! - `Container::scan_packages([..])` restricts discovery to base packages
//!   (Spring's `@ComponentScan(basePackages = …)`).
//!
//! `inventory` is per-test-binary, so every type defined in this file is part of
//! one deterministic scan set.

use firefly::prelude::*;

// ===========================================================================
// 1. scan() auto-registers #[bean] factory methods (no manual call)
// ===========================================================================

#[derive(Configuration, Default)]
struct ClockConfig;

#[derive(Debug, PartialEq, Eq)]
struct RealClock(u64);

#[firefly::bean]
impl ClockConfig {
    #[bean]
    fn clock(&self) -> RealClock {
        RealClock(7)
    }
}

#[test]
fn scan_auto_registers_bean_methods() {
    let c = Container::new();
    // No `ClockConfig::firefly_register_beans(&c)` — scan() must find the bean.
    let registered = c.scan();
    assert!(
        registered >= 2,
        "scan should register the config holder + its bean"
    );

    let clock = c
        .resolve::<RealClock>()
        .expect("#[bean] method must be auto-registered by scan()");
    assert_eq!(*clock, RealClock(7));
}

// ===========================================================================
// 2a. AutoConfiguration fills a gap when no user bean exists
// ===========================================================================

#[derive(Debug, PartialEq, Eq)]
struct CacheClient {
    kind: &'static str,
}

#[derive(AutoConfiguration, Default)]
struct CacheAutoConfiguration;

#[firefly::bean]
impl CacheAutoConfiguration {
    // No user CacheClient is defined anywhere → this default is contributed.
    #[bean(condition_on_missing_bean = "CacheClient")]
    fn cache_client(&self) -> CacheClient {
        CacheClient { kind: "default" }
    }
}

#[test]
fn autoconfiguration_provides_default_when_bean_absent() {
    let c = Container::new();
    c.scan();
    let cache = c
        .resolve::<CacheClient>()
        .expect("auto-configuration must supply a default CacheClient");
    assert_eq!(cache.kind, "default");
}

// ===========================================================================
// 2b. A user bean wins over the auto-configuration default
// ===========================================================================

#[derive(Debug, PartialEq, Eq)]
struct Mailer {
    kind: &'static str,
}

// The user's own configuration (unconditional → registers in the first pass).
#[derive(Configuration, Default)]
struct UserMailConfig;

#[firefly::bean]
impl UserMailConfig {
    #[bean]
    fn mailer(&self) -> Mailer {
        Mailer { kind: "user" }
    }
}

// The framework's auto-configuration default (deferred → only fills a gap).
#[derive(AutoConfiguration, Default)]
struct MailAutoConfiguration;

#[firefly::bean]
impl MailAutoConfiguration {
    #[bean(condition_on_missing_bean = "Mailer")]
    fn mailer(&self) -> Mailer {
        Mailer { kind: "auto" }
    }
}

#[test]
fn user_bean_wins_over_autoconfiguration() {
    let c = Container::new();
    c.scan();
    let mailer = c.resolve::<Mailer>().expect("a Mailer must be registered");
    assert_eq!(
        mailer.kind, "user",
        "the user-defined bean must take precedence over the auto-configuration default"
    );
}

// ===========================================================================
// 3. scan_packages() restricts discovery to base packages
// ===========================================================================

mod widgets {
    use firefly::prelude::*;

    #[derive(Component, Default)]
    pub struct WidgetX;
}

#[derive(Component, Default)]
struct TopLevelComponent;

#[test]
fn scan_packages_restricts_to_base_packages() {
    let c = Container::new();
    // Only scan the `widgets` submodule.
    c.scan_packages(&["autoconfig::widgets"]);

    assert!(
        c.resolve::<widgets::WidgetX>().is_ok(),
        "a component in the scanned package must be registered"
    );
    assert!(
        c.resolve::<TopLevelComponent>().is_err(),
        "a component outside the scanned package must NOT be registered"
    );
}
