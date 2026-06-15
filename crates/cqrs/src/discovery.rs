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

//! Link-time CQRS handler discovery — the Rust analog of Spring's
//! component-scan wiring every `@CommandHandler` / `@QueryHandler` onto the bus.
//!
//! Each `#[command_handler]` / `#[query_handler]` submits a
//! [`HandlerRegistration`] via [`inventory`]; [`register_discovered_handlers`]
//! collects them across the whole crate graph and installs each on a [`Bus`] —
//! so a service never hand-maintains a `register(&bus)` list. Because
//! [`Bus::register`](crate::Bus::register) overwrites, calling the drain after
//! (or instead of) the generated `register_<fn>` helpers is always safe.

use firefly_container::Container;

use crate::Bus;

/// One link-time handler-registration thunk, `inventory::submit!`-ted once per
/// `#[command_handler]` / `#[query_handler]`. [`register`](Self::register) is the
/// generated `register_<fn>(bus)` helper.
pub struct HandlerRegistration {
    /// Installs this handler on the bus (the generated `register_<fn>`).
    pub register: fn(&Bus),
}

inventory::collect!(HandlerRegistration);

/// One link-time **bean** handler-registration thunk, `inventory::submit!`-ted
/// once per `#[command_handler]` / `#[query_handler]` method inside a
/// `#[handlers]` bean impl — the Rust analog of Spring scanning a `@Component`'s
/// `@CommandHandler` methods.
///
/// Unlike [`HandlerRegistration`] (a free `fn(&Bus)`), this resolves the handler
/// **bean** from the DI container and installs a closure that captures the bean,
/// so the handler reaches its collaborators through ordinary `#[autowired]`
/// fields instead of a process-global.
pub struct BeanHandlerRegistration {
    /// Resolves the handler bean from `container` and installs its handler on
    /// `bus`.
    pub register: fn(&Bus, &Container),
}

inventory::collect!(BeanHandlerRegistration);

/// Installs every discovered (`inventory`-submitted) command/query handler on
/// `bus` — the turnkey replacement for hand-calling each generated
/// `register_<fn>(&bus)`. Returns the number of handlers registered.
pub fn register_discovered_handlers(bus: &Bus) -> usize {
    let mut count = 0;
    for reg in inventory::iter::<HandlerRegistration> {
        (reg.register)(bus);
        count += 1;
    }
    count
}

/// The number of `#[command_handler]` / `#[query_handler]` handlers discovered
/// across the crate graph — for the startup report and tests.
#[must_use]
pub fn discovered_handler_count() -> usize {
    inventory::iter::<HandlerRegistration>.into_iter().count()
}

/// Installs every discovered **bean** handler on `bus`, resolving each handler
/// bean from `container` — the turnkey wiring `FireflyApplication` runs for
/// `#[handlers]` beans. Returns the number registered.
pub fn register_discovered_handler_beans(bus: &Bus, container: &Container) -> usize {
    let mut count = 0;
    for reg in inventory::iter::<BeanHandlerRegistration> {
        (reg.register)(bus, container);
        count += 1;
    }
    count
}

/// The number of `#[handlers]`-bean command/query handlers discovered across the
/// crate graph — for the startup report and tests.
#[must_use]
pub fn discovered_handler_bean_count() -> usize {
    inventory::iter::<BeanHandlerRegistration>
        .into_iter()
        .count()
}
