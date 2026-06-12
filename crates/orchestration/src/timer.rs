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

//! Workflow timer service — sleeps for a delay, then resumes the
//! workflow. The Rust spelling of pyfly's `TimerService`
//! (`pyfly.transactional.workflow.timer_service`) over
//! [`tokio::time::sleep`].

use std::time::Duration;

use crate::workflow::Node;

/// Simple in-process timer — pyfly's `TimerService`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TimerService;

impl TimerService {
    /// Returns the timer service. It is stateless; the unit struct exists
    /// so engines can hold the same named dependency as the Python port.
    pub fn new() -> Self {
        Self
    }

    /// Sleeps for `delay_ms` milliseconds; `0` returns immediately.
    pub async fn sleep_ms(&self, delay_ms: u64) {
        if delay_ms == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }

    /// Sleeps for `delay`; a zero duration returns immediately.
    pub async fn sleep(&self, delay: Duration) {
        if delay.is_zero() {
            return;
        }
        tokio::time::sleep(delay).await;
    }
}

impl Node {
    /// Builds a workflow node that sleeps for `delay` and then completes —
    /// the engine spelling of pyfly's `@wait_for_timer(delay_ms=…)` step
    /// decorator.
    pub fn timer(name: impl Into<String>, delay: Duration) -> Node {
        Node::new(name, move || async move {
            TimerService::new().sleep(delay).await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workflow;
    use std::time::Instant;

    // Port of pyfly TestTimer::test_wait_for_timer_pauses_briefly.
    #[tokio::test]
    async fn timer_node_pauses_briefly_then_completes() {
        let workflow = Workflow::new("delayed")
            .node(Node::timer("warmup", Duration::from_millis(20)))
            .node(crate::Node::new("done", || async { Ok(()) }).depends_on(["warmup"]));
        let started = Instant::now();
        tokio::time::timeout(Duration::from_millis(200), workflow.run())
            .await
            .expect("must finish quickly")
            .expect("workflow should complete");
        assert!(
            started.elapsed() >= Duration::from_millis(20),
            "timer must actually pause"
        );
    }

    // Port of pyfly TimerService.sleep_ms(0) fast path.
    #[tokio::test]
    async fn zero_delay_returns_immediately() {
        let started = Instant::now();
        TimerService::new().sleep_ms(0).await;
        TimerService::new().sleep(Duration::ZERO).await;
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn sleep_ms_sleeps_at_least_the_delay() {
        let started = Instant::now();
        TimerService::new().sleep_ms(10).await;
        assert!(started.elapsed() >= Duration::from_millis(10));
    }
}
