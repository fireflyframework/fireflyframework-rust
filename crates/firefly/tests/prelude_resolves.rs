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

//! The defining test for the facade: `use firefly::prelude::*;` must compile
//! and every name in the high-frequency surface must resolve to its type.
//! If any prelude re-export breaks, this file fails to compile.

use firefly::prelude::*;

/// Every prelude name resolves to a real type/value. The function bodies are
/// type-checked at compile time, which is the actual assertion; the test
/// running at all proves the glob import compiles.
#[test]
fn prelude_names_resolve() {
    // CQRS: Bus + Message trait + CqrsError.
    let _bus: Bus = Bus::new();
    fn _takes_message<M: Message>(_m: M) {}
    fn _make_cqrs_err() -> CqrsError {
        CqrsError::Validation("x".into())
    }

    // DI container + scopes.
    let _container: Container = Container::new();
    let _scope: Scope = Scope::Singleton;

    // Scheduler (task scheduler, not the reactive one).
    let _sched: Scheduler = Scheduler::new();

    // Orchestration: Saga + Step are nameable types.
    fn _takes_saga(_s: Saga) {}
    fn _takes_step(_s: Step) {}

    // Lifecycle handles are nameable.
    fn _takes_app(_a: Application) {}
    fn _takes_shutdown(_h: ShutdownHandle) {}

    // Core wiring struct + its config default.
    let _cfg: CoreConfig = CoreConfig::default();
    fn _takes_core(_c: Core) {}

    // Web result/error + the problem-response helper symbol.
    fn _takes_web_result(_r: WebResult<()>) {}
    fn _takes_web_error(_e: WebError) {}
    // `problem_response(&ProblemDetail) -> Response` — name resolves through the prelude.
    let _problem_fn = problem_response;

    // Reactive primitives.
    let _mono: Mono<i32> = Mono::just(1);
    let _flux: Flux<i32> = Flux::from_iter(vec![1, 2, 3]);
}

/// The framework error/result types from the prelude compose as expected.
#[test]
fn firefly_result_composes() {
    fn ok() -> FireflyResult<i32> {
        Ok(42)
    }
    fn boom() -> FireflyResult<i32> {
        Err(FireflyError::internal("boom"))
    }
    assert_eq!(ok().unwrap(), 42);
    assert!(boom().is_err());
}

/// A tiny end-to-end: build the wiring `Core` purely through the prelude — no
/// `firefly_starter_core::…` path in sight. This is the one-dependency promise.
#[test]
fn core_builds_from_prelude_only() {
    let core = Core::new(CoreConfig {
        app_name: "facade-test".into(),
        app_version: "0.0.1".into(),
        ..CoreConfig::default()
    });
    assert_eq!(core.app_name, "facade-test");
    assert_eq!(core.app_version, "0.0.1");
}

/// The ergonomic module aliases resolve to the same types as the prelude.
#[test]
fn module_aliases_resolve() {
    let _bus: firefly::cqrs::Bus = firefly::cqrs::Bus::new();
    let _container: firefly::container::Container = firefly::container::Container::new();
    let _sched: firefly::scheduling::Scheduler = firefly::scheduling::Scheduler::new();
    // The `__rt` contract path reaches the same crate.
    let _bus2: firefly::__rt::firefly_cqrs::Bus = firefly::__rt::firefly_cqrs::Bus::new();
}

/// The facade version stamp matches the kernel's.
#[test]
fn version_matches_kernel() {
    assert_eq!(firefly::VERSION, firefly::kernel::VERSION);
}
