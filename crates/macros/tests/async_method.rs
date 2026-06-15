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

//! Behavioral test for `#[async_method]` against `firefly-scheduling` directly
//! (no facade): the rewritten non-async method hands its body to the executor
//! and returns a `TaskHandle` that joins to the original value.
//!
//! The macro routes runtime types through `#facade::__rt::firefly_scheduling`.
//! To exercise it without depending on the heavy `firefly` facade, this test
//! supplies a local facade shim: a crate-root `__rt` module re-exporting
//! `firefly_scheduling`, selected with `#[async_method(crate = "crate")]`.

use std::sync::Arc;

/// Minimal facade shim so the macro's `#facade::__rt::firefly_scheduling` path
/// resolves against this test crate (selected via `crate = "crate"`).
#[doc(hidden)]
pub mod __rt {
    pub use firefly_scheduling;
}

use firefly_scheduling::{register_task_executor, TaskExecutor, TaskHandle, TaskJoinError};

struct Reports {
    base: u64,
}

impl Reports {
    /// Rewritten by `#[async_method]` to
    /// `fn rebuild(self: Arc<Self>, factor: u64) -> TaskHandle<u64>`.
    #[firefly_macros::async_method(crate = "crate")]
    async fn rebuild(self: Arc<Self>, factor: u64) -> u64 {
        // A real await point inside the spawned future.
        tokio::task::yield_now().await;
        self.base * factor
    }

    /// Spawns on an explicitly named executor instead of the global one.
    #[firefly_macros::async_method(crate = "crate", executor = "self.executor()")]
    async fn doubled(self: Arc<Self>) -> u64 {
        self.base * 2
    }

    fn executor(&self) -> TaskExecutor {
        TaskExecutor::new(2)
    }
}

#[tokio::test]
async fn async_method_returns_joinable_handle() {
    let reports = Arc::new(Reports { base: 21 });

    // The rewritten method is non-async and returns a `TaskHandle<u64>`.
    let handle: TaskHandle<u64> = Arc::clone(&reports).rebuild(2);

    // The handle joins to the value the original async body produced.
    let value: Result<u64, TaskJoinError> = handle.join().await;
    assert_eq!(value.unwrap(), 42);
}

#[tokio::test]
async fn async_method_handle_is_awaitable() {
    let reports = Arc::new(Reports { base: 5 });
    // The handle is itself a Future, so `.await` resolves it.
    let value = Arc::clone(&reports).rebuild(3).await.unwrap();
    assert_eq!(value, 15);
}

#[tokio::test]
async fn async_method_uses_explicit_executor() {
    let reports = Arc::new(Reports { base: 9 });
    let value = reports.doubled().await.unwrap();
    assert_eq!(value, 18);
}

#[tokio::test]
async fn async_method_uses_registered_global_executor() {
    // Registering the process executor is first-wins and never panics; the
    // rewritten method then spawns on whatever `task_executor()` returns.
    let _ = register_task_executor(Arc::new(TaskExecutor::new(4)));
    let reports = Arc::new(Reports { base: 7 });
    let value = reports.rebuild(6).await.unwrap();
    assert_eq!(value, 42);
}
