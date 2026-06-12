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

//! Advanced wait/compose primitives — `wait_all` (gather) and `wait_any`
//! (race) over a mix of named signals and timers, with an optional overall
//! timeout.
//!
//! The Rust spelling of pyfly's `WaitForAll` / `WaitForAny` step gates
//! (`pyfly.transactional.workflow.{annotations,executor}`):
//!
//! * `WaitForAll` — `asyncio.gather` every signal wait AND every timer
//!   ([`wait_all`]); the gate completes when all of them have completed.
//! * `WaitForAny` — `asyncio.wait(..., FIRST_COMPLETED)` racing signals and
//!   timers ([`wait_any`]); the first to fire wins, the losers are
//!   cancelled, and the winning signal's payload is returned.
//!
//! Both build on the existing [`SignalService`](crate::SignalService) and
//! [`TimerService`](crate::TimerService) and use tokio primitives
//! ([`tokio::select`], [`futures::future::join_all`]).

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::signal::SignalService;
use crate::timer::TimerService;

/// One participant in a [`wait_all`] / [`wait_any`] gate.
#[derive(Debug, Clone)]
pub enum WaitTarget {
    /// Wait for a named signal to be delivered to a correlation id —
    /// pyfly's `WaitForAll/Any(signals=...)`.
    Signal {
        /// The execution correlation id the signal is keyed on.
        correlation_id: String,
        /// The signal name to await.
        signal: String,
    },
    /// Wait for a timer of `delay` to elapse — pyfly's
    /// `WaitForAll/Any(timers=...)`.
    Timer {
        /// How long the timer runs.
        delay: Duration,
    },
}

impl WaitTarget {
    /// Convenience constructor for a signal target.
    pub fn signal(correlation_id: impl Into<String>, signal: impl Into<String>) -> Self {
        Self::Signal {
            correlation_id: correlation_id.into(),
            signal: signal.into(),
        }
    }

    /// Convenience constructor for a timer target.
    pub fn timer(delay: Duration) -> Self {
        Self::Timer { delay }
    }
}

/// Which target won a [`wait_any`] race.
#[derive(Debug, Clone, PartialEq)]
pub enum WaitOutcome {
    /// A signal fired first; carries the signal name and its delivered
    /// payload (pyfly captures it as a `signal:<name>` variable).
    Signal {
        /// The signal that won the race.
        signal: String,
        /// The payload it was delivered with.
        payload: Value,
    },
    /// A timer fired first; carries its index among the timer targets.
    Timer {
        /// Zero-based index of the winning timer among all timer targets.
        index: usize,
    },
}

/// Error produced by [`wait_all`] / [`wait_any`].
#[derive(Debug, thiserror::Error)]
pub enum WaitError {
    /// The gate did not complete within the supplied timeout — pyfly's
    /// `asyncio.TimeoutError` surfacing from `wait_for_*_timeout_ms`.
    #[error("wait gate timed out after {0:?}")]
    TimedOut(Duration),
    /// A waited signal's waiter was discarded (the execution was
    /// unregistered) before the signal arrived.
    #[error("wait gate cancelled: signal {signal:?} for {correlation_id:?}")]
    Cancelled {
        /// The execution that was waiting.
        correlation_id: String,
        /// The signal it was waiting for.
        signal: String,
    },
    /// The gate had no targets at all — nothing to wait for.
    #[error("wait gate has no targets")]
    Empty,
}

/// Waits for **every** target to complete (gather) — pyfly's `WaitForAll`.
///
/// Signals resolve when delivered through `signals`; timers resolve when
/// they elapse. When `timeout` is `Some`, the whole gather is bounded and a
/// breach returns [`WaitError::TimedOut`]. An empty target list returns
/// `Ok(())` immediately (a no-op gate), matching pyfly's `if awaitables`.
pub async fn wait_all(
    signals: &Arc<SignalService>,
    timers: &TimerService,
    targets: &[WaitTarget],
    timeout: Option<Duration>,
) -> Result<(), WaitError> {
    if targets.is_empty() {
        return Ok(());
    }

    let futures = targets.iter().map(|target| {
        let signals = Arc::clone(signals);
        let timers = *timers;
        let target = target.clone();
        async move {
            match target {
                WaitTarget::Signal {
                    correlation_id,
                    signal,
                } => signals
                    .wait_for(&correlation_id, &signal)
                    .await
                    .map(|_| ())
                    .map_err(|_| WaitError::Cancelled {
                        correlation_id,
                        signal,
                    }),
                WaitTarget::Timer { delay } => {
                    timers.sleep(delay).await;
                    Ok(())
                }
            }
        }
    });

    let gather = futures::future::try_join_all(futures);
    let bounded = run_bounded(gather, timeout).await?;
    bounded.map(|_| ())
}

/// Waits for the **first** target to complete (race) — pyfly's
/// `WaitForAny`. Returns the [`WaitOutcome`] describing the winner; losing
/// waiters are dropped (which unregisters their signal subscription).
///
/// When `timeout` is `Some`, the race is bounded and a breach returns
/// [`WaitError::TimedOut`]. An empty target list returns [`WaitError::Empty`].
pub async fn wait_any(
    signals: &Arc<SignalService>,
    timers: &TimerService,
    targets: &[WaitTarget],
    timeout: Option<Duration>,
) -> Result<WaitOutcome, WaitError> {
    if targets.is_empty() {
        return Err(WaitError::Empty);
    }

    let mut futures = Vec::with_capacity(targets.len());
    let mut timer_index = 0usize;
    for target in targets {
        let signals = Arc::clone(signals);
        let timers = *timers;
        let target = target.clone();
        let this_timer_index = timer_index;
        if matches!(target, WaitTarget::Timer { .. }) {
            timer_index += 1;
        }
        futures.push(Box::pin(async move {
            match target {
                WaitTarget::Signal {
                    correlation_id,
                    signal,
                } => match signals.wait_for(&correlation_id, &signal).await {
                    Ok(payload) => Ok(WaitOutcome::Signal { signal, payload }),
                    Err(_) => Err(WaitError::Cancelled {
                        correlation_id,
                        signal,
                    }),
                },
                WaitTarget::Timer { delay } => {
                    timers.sleep(delay).await;
                    Ok(WaitOutcome::Timer {
                        index: this_timer_index,
                    })
                }
            }
        }));
    }

    // `select_all` resolves with the first future to finish; the remaining
    // futures are dropped (their signal subscriptions are discarded),
    // mirroring pyfly cancelling the pending tasks.
    let race = async move {
        let (result, _index, _rest) = futures::future::select_all(futures).await;
        result
    };
    run_bounded(race, timeout).await?
}

/// Bounds `fut` by `timeout` when present, mapping an elapsed deadline to
/// [`WaitError::TimedOut`].
async fn run_bounded<T>(
    fut: impl std::future::Future<Output = T>,
    timeout: Option<Duration>,
) -> Result<T, WaitError> {
    match timeout {
        Some(limit) => tokio::time::timeout(limit, fut)
            .await
            .map_err(|_| WaitError::TimedOut(limit)),
        None => Ok(fut.await),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    // Port of pyfly WaitForAll gather: the gate completes only once every
    // signal and every timer has fired.
    #[tokio::test]
    async fn wait_all_gathers_signals_and_timers() {
        let signals = Arc::new(SignalService::new());
        let timers = TimerService::new();
        let targets = vec![
            WaitTarget::signal("run-1", "a"),
            WaitTarget::signal("run-1", "b"),
            WaitTarget::timer(Duration::from_millis(10)),
        ];

        let deliverer = {
            let signals = Arc::clone(&signals);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(15)).await;
                // Deliver both signals; buffer handles deliver-before-wait.
                while !signals.deliver("run-1", "a", json!(1)) {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                while !signals.deliver("run-1", "b", json!(2)) {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
        };

        tokio::time::timeout(
            Duration::from_millis(500),
            wait_all(&signals, &timers, &targets, None),
        )
        .await
        .expect("must complete")
        .expect("gather ok");
        deliverer.await.expect("deliverer");
    }

    // Port of pyfly WaitForAny race: the timer fires first and wins.
    #[tokio::test]
    async fn wait_any_timer_wins_when_signal_never_arrives() {
        let signals = Arc::new(SignalService::new());
        let timers = TimerService::new();
        let targets = vec![
            WaitTarget::signal("run-2", "never"),
            WaitTarget::timer(Duration::from_millis(10)),
        ];
        let outcome = wait_any(&signals, &timers, &targets, None)
            .await
            .expect("race ok");
        assert_eq!(outcome, WaitOutcome::Timer { index: 0 });
        // The losing signal waiter dropped its receiver: a subsequent deliver
        // finds no *live* waiter (the dead sender is swept) and buffers the
        // payload instead — pyfly cancelling the pending signal task.
        assert!(!signals.deliver("run-2", "never", json!("late")));
    }

    // Port of pyfly WaitForAny: a delivered signal wins and its payload is
    // surfaced (pyfly captures it as a `signal:<name>` variable).
    #[tokio::test]
    async fn wait_any_signal_wins_with_payload() {
        let signals = Arc::new(SignalService::new());
        let timers = TimerService::new();
        // Pre-deliver so the signal wins the race immediately.
        signals.deliver("run-3", "go", json!({"ok": true}));
        let targets = vec![
            WaitTarget::signal("run-3", "go"),
            WaitTarget::timer(Duration::from_millis(500)),
        ];
        let outcome = wait_any(&signals, &timers, &targets, None)
            .await
            .expect("race ok");
        match outcome {
            WaitOutcome::Signal { signal, payload } => {
                assert_eq!(signal, "go");
                assert_eq!(payload, json!({"ok": true}));
            }
            other => panic!("expected signal win, got {other:?}"),
        }
    }

    // Port of pyfly wait_for_*_timeout_ms: a gate that never completes
    // surfaces a timeout rather than hanging.
    #[tokio::test]
    async fn wait_all_times_out() {
        let signals = Arc::new(SignalService::new());
        let timers = TimerService::new();
        let targets = vec![WaitTarget::signal("run-4", "never")];
        let err = wait_all(&signals, &timers, &targets, Some(Duration::from_millis(20)))
            .await
            .expect_err("must time out");
        assert!(matches!(err, WaitError::TimedOut(_)));
    }

    #[tokio::test]
    async fn wait_any_times_out() {
        let signals = Arc::new(SignalService::new());
        let timers = TimerService::new();
        let targets = vec![WaitTarget::signal("run-5", "never")];
        let err = wait_any(&signals, &timers, &targets, Some(Duration::from_millis(20)))
            .await
            .expect_err("must time out");
        assert!(matches!(err, WaitError::TimedOut(_)));
    }

    // An empty wait-all is a no-op (pyfly's `if awaitables` guard).
    #[tokio::test]
    async fn wait_all_empty_is_noop() {
        let signals = Arc::new(SignalService::new());
        let timers = TimerService::new();
        let started = Instant::now();
        wait_all(&signals, &timers, &[], None)
            .await
            .expect("empty ok");
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    // An empty wait-any has nothing to race.
    #[tokio::test]
    async fn wait_any_empty_errors() {
        let signals = Arc::new(SignalService::new());
        let timers = TimerService::new();
        let err = wait_any(&signals, &timers, &[], None)
            .await
            .expect_err("empty errors");
        assert!(matches!(err, WaitError::Empty));
    }
}
