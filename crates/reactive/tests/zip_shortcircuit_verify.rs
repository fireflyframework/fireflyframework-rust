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

use firefly_kernel::FireflyError;
use firefly_reactive::Mono;
use std::future::pending;

#[tokio::test(start_paused = true)]
async fn zip_error_with_pending_should_not_hang() {
    // Left side errors immediately; right side never resolves.
    let boom = FireflyError::internal("boom");
    let left: Mono<i32> = Mono::error(boom);
    let right: Mono<i32> = Mono::from_future(async { pending::<i32>().await });

    let zipped = left.zip_with(right);

    // If zip short-circuits on error (Reactor semantics), this resolves with Err.
    // If it awaits both via join!, this hangs forever (even with paused clock,
    // because pending() never wakes). We wrap in a timeout to detect the hang.
    let res = tokio::time::timeout(std::time::Duration::from_secs(3600), zipped.block()).await;

    match res {
        Ok(inner) => {
            assert!(
                inner.is_err(),
                "expected error to short-circuit, got {:?}",
                inner.map(|o| o.is_some())
            );
            println!("RESULT: short-circuited with error (bug NOT present)");
        }
        Err(_) => {
            println!("RESULT: TIMED OUT — zip hung on error (bug PRESENT)");
            panic!("zip_with hung instead of short-circuiting on error");
        }
    }
}
