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

//! A scheduled housekeeping task, declared with `#[scheduled]` (book
//! chapter 17, "Scheduling & Notifications").
//!
//! `#[scheduled(fixed_rate = "...")]` generates a
//! `schedule_<fn>(scheduler)` helper that registers the zero-argument
//! `async fn` on a [`Scheduler`](firefly::prelude::Scheduler). A real Lumen
//! deployment would, on this tick, sweep abandoned wallets or emit a daily
//! statement; the teaching version simply records that the heartbeat ran so a
//! reader sees the macro wired end to end.

use std::sync::atomic::{AtomicU64, Ordering};

use firefly::prelude::*;

/// The number of times the heartbeat has run — observable from a test (and, in
/// a real service, a counter you would surface on `/actuator/metrics`).
static HEARTBEAT_TICKS: AtomicU64 = AtomicU64::new(0);

/// A periodic housekeeping heartbeat. `#[scheduled(fixed_rate = "60s")]`
/// generates `schedule_ledger_heartbeat(scheduler)`; the framework calls this
/// on every tick after the initial delay.
#[scheduled(fixed_rate = "60s", initial_delay = "5s")]
pub async fn ledger_heartbeat() -> Result<(), std::io::Error> {
    HEARTBEAT_TICKS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Registers the heartbeat on a fresh scheduler and returns it — `main()`
/// starts it; tests assert it registered.
pub fn build_scheduler() -> std::sync::Arc<Scheduler> {
    let scheduler = std::sync::Arc::new(Scheduler::new());
    // `#[scheduled]` tasks are DISCOVERED and registered through the
    // inventory/DI registry — no manual `schedule_<fn>` calls.
    firefly::scheduling::register_discovered_scheduled(&scheduler);
    scheduler
}

/// How many heartbeat ticks have run so far.
pub fn heartbeat_ticks() -> u64 {
    HEARTBEAT_TICKS.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_task_registers() {
        let scheduler = build_scheduler();
        let names: Vec<String> = scheduler.tasks().into_iter().map(|t| t.name).collect();
        assert!(
            names.contains(&"ledger_heartbeat".to_string()),
            "the #[scheduled] task should be registered, got {names:?}"
        );
    }

    #[tokio::test]
    async fn heartbeat_runs() {
        let before = heartbeat_ticks();
        ledger_heartbeat().await.unwrap();
        assert_eq!(heartbeat_ticks(), before + 1);
    }
}
