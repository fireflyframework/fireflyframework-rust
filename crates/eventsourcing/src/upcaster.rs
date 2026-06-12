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

//! Event upcasting — schema migration applied when reading old events.
//!
//! Ports pyfly's `eventsourcing.upcaster` module. An [`EventUpcaster`]
//! transforms a stored [`DomainEvent`] from version *N* to *N+1*; the read
//! paths of an [`EventStore`](crate::EventStore) run every loaded event
//! through the configured upcaster chain so consumers always observe
//! current-schema events. Write paths are never touched — only what is read
//! back is upcast.

use crate::aggregate::DomainEvent;

/// Transforms a stored [`DomainEvent`] from one schema version to the next.
///
/// The Rust analog of pyfly's `EventUpcaster` protocol: [`applies_to`]
/// guards whether this upcaster handles a given event, and [`upcast`]
/// produces the migrated event. Implementations should be pure — they take
/// a borrowed event and return an owned, transformed copy — so the same
/// event can be funnelled through several upcasters in sequence.
///
/// [`applies_to`]: EventUpcaster::applies_to
/// [`upcast`]: EventUpcaster::upcast
///
/// # Example
///
/// ```
/// use firefly_eventsourcing::{DomainEvent, EventUpcaster};
///
/// /// Renames the legacy `account.opened` event type to `AccountOpened`.
/// struct RenameUpcaster;
/// impl EventUpcaster for RenameUpcaster {
///     fn applies_to(&self, event: &DomainEvent) -> bool {
///         event.event_type == "account.opened"
///     }
///     fn upcast(&self, mut event: DomainEvent) -> DomainEvent {
///         event.event_type = "AccountOpened".into();
///         event
///     }
/// }
/// ```
pub trait EventUpcaster: Send + Sync {
    /// Whether this upcaster should transform `event`. Returning `false`
    /// leaves the event untouched by this upcaster.
    fn applies_to(&self, event: &DomainEvent) -> bool;

    /// Transforms `event` into its next-version form. Only called when
    /// [`applies_to`](EventUpcaster::applies_to) returned `true`.
    fn upcast(&self, event: DomainEvent) -> DomainEvent;
}

/// The default upcaster — leaves every event unchanged.
///
/// Mirrors pyfly's `NoOpUpcaster`: [`applies_to`](EventUpcaster::applies_to)
/// always returns `false`, so registering it in a chain is a no-op.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOpUpcaster;

impl EventUpcaster for NoOpUpcaster {
    fn applies_to(&self, _event: &DomainEvent) -> bool {
        false
    }

    fn upcast(&self, event: DomainEvent) -> DomainEvent {
        event
    }
}

/// Applies each upcaster (in order) that handles `event` and returns the
/// migrated event. With an empty chain this is the identity function, so an
/// event store configured without upcasters returns stored events verbatim.
pub(crate) fn apply_upcasters(
    mut event: DomainEvent,
    upcasters: &[std::sync::Arc<dyn EventUpcaster>],
) -> DomainEvent {
    for upcaster in upcasters {
        if upcaster.applies_to(&event) {
            event = upcaster.upcast(event);
        }
    }
    event
}
