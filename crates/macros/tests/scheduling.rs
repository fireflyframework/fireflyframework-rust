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

//! Behavioral test for `#[scheduled]`: the generated helper registers the task
//! on a real `Scheduler` and the task actually fires.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use firefly::prelude::*;

static TICKS: AtomicU32 = AtomicU32::new(0);

#[scheduled(fixed_rate = "10ms")]
async fn fast_task() -> Result<(), std::io::Error> {
    TICKS.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[scheduled(cron = "0 0 * * *", zone = "America/New_York")]
async fn nightly() -> Result<(), std::io::Error> {
    Ok(())
}

#[tokio::test]
async fn scheduled_registers_and_fires() {
    let scheduler = Arc::new(Scheduler::new());

    // Both generated helpers register against the scheduler without error.
    schedule_fast_task(&scheduler);
    schedule_nightly(&scheduler);

    // The scheduler now holds two named tasks.
    let names: Vec<String> = scheduler.tasks().into_iter().map(|t| t.name).collect();
    assert!(
        names.contains(&"fast_task".to_string()),
        "fast_task registered"
    );
    assert!(names.contains(&"nightly".to_string()), "nightly registered");

    // Run the scheduler briefly so the fixed-rate task fires at least once.
    let runner = Arc::clone(&scheduler);
    let handle = tokio::spawn(async move { runner.start().await });
    tokio::time::sleep(Duration::from_millis(120)).await;
    scheduler.stop();
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;

    assert!(
        TICKS.load(Ordering::SeqCst) >= 1,
        "the fixed-rate task should have fired at least once"
    );
}
