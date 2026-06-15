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

//! Link-time `@Scheduled` discovery — the Rust analog of Spring's
//! `ScheduledAnnotationBeanPostProcessor` registering every `@Scheduled` method.
//!
//! Each `#[scheduled]` submits a [`ScheduledRegistration`] via [`inventory`];
//! [`register_discovered_scheduled`] collects them across the crate graph and
//! registers each task on a [`Scheduler`] — so a service never hand-maintains a
//! list of `schedule_<fn>(&scheduler)` calls.

use firefly_container::Container;

use crate::Scheduler;

/// One link-time scheduled-task thunk, `inventory::submit!`-ted once per
/// `#[scheduled]`. [`schedule`](Self::schedule) is the generated
/// `schedule_<fn>(scheduler)` helper.
pub struct ScheduledRegistration {
    /// Registers this task on the scheduler (the generated `schedule_<fn>`).
    pub schedule: fn(&Scheduler),
}

inventory::collect!(ScheduledRegistration);

/// One link-time **bean** scheduled-task thunk, `inventory::submit!`-ted once per
/// `#[scheduled]` method inside a `#[handlers]` bean impl — the Rust analog of
/// Spring scanning a `@Component`'s `@Scheduled` methods.
///
/// Unlike [`ScheduledRegistration`], [`schedule`](Self::schedule) resolves the
/// task's **bean** from the DI container and registers a closure that captures
/// it, so the task reaches its collaborators through ordinary `#[autowired]`
/// fields instead of a process-global.
pub struct BeanScheduledRegistration {
    /// Resolves the task's bean from `container` and registers it on `scheduler`.
    pub schedule: fn(&Scheduler, &Container),
}

inventory::collect!(BeanScheduledRegistration);

/// Registers every discovered (`inventory`-submitted) `#[scheduled]` task on
/// `scheduler` — the turnkey replacement for hand-calling each generated
/// `schedule_<fn>(&scheduler)`. Returns the number of tasks registered.
pub fn register_discovered_scheduled(scheduler: &Scheduler) -> usize {
    let mut count = 0;
    for reg in inventory::iter::<ScheduledRegistration> {
        (reg.schedule)(scheduler);
        count += 1;
    }
    count
}

/// The number of `#[scheduled]` tasks discovered across the crate graph — for
/// the startup report and tests.
#[must_use]
pub fn discovered_scheduled_count() -> usize {
    inventory::iter::<ScheduledRegistration>.into_iter().count()
}

/// Registers every discovered **bean** scheduled task on `scheduler`, resolving
/// each task's bean from `container` — the turnkey wiring `FireflyApplication`
/// runs for `#[handlers]` beans. Returns the number registered.
pub fn register_discovered_scheduled_beans(scheduler: &Scheduler, container: &Container) -> usize {
    let mut count = 0;
    for reg in inventory::iter::<BeanScheduledRegistration> {
        (reg.schedule)(scheduler, container);
        count += 1;
    }
    count
}

/// The number of `#[handlers]`-bean scheduled tasks discovered across the crate
/// graph — for the startup report and tests.
#[must_use]
pub fn discovered_scheduled_bean_count() -> usize {
    inventory::iter::<BeanScheduledRegistration>
        .into_iter()
        .count()
}
