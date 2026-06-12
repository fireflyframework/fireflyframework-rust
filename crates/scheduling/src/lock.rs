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

//! Distributed locks for scheduled tasks — pyfly `pyfly.scheduling.lock`
//! parity (itself a ShedLock / Spring `@SchedulerLock` port).
//!
//! When a [`Task`](crate::Task) declares a lock name, the scheduler acquires
//! the lock before each run and **skips the tick** when it is held elsewhere
//! — so in a cluster only one instance runs the job at a time. The default
//! [`LocalLock`] always acquires (single-instance behaviour is unchanged);
//! install an [`InProcessLock`], [`RedisLock`](crate::RedisLock), or
//! [`PostgresAdvisoryLock`](crate::PostgresAdvisoryLock) via
//! [`Scheduler::with_lock`](crate::Scheduler::with_lock) to coordinate.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;

/// Boxed error a lock backend may surface (network failure, protocol error).
///
/// pyfly's lock protocol raises; here backend failures are `Err` — the
/// scheduler treats them as "not acquired" and skips the tick, logging a
/// warning.
pub type LockError = Box<dyn std::error::Error + Send + Sync>;

/// A best-effort, TTL-bounded named lock — the async port of pyfly's
/// `DistributedLock` protocol.
///
/// Implementations must be cheap to share (`Send + Sync`); the scheduler
/// holds one provider behind an `Arc` and consults it on every locked tick.
#[async_trait]
pub trait DistributedLock: Send + Sync {
    /// Attempts to acquire `name` for up to `ttl`. Returns whether acquired.
    ///
    /// The TTL is the crash-safety valve: a holder that dies without
    /// releasing must not wedge the job forever.
    async fn try_acquire(&self, name: &str, ttl: Duration) -> Result<bool, LockError>;

    /// Releases `name` (a no-op when not held by this instance).
    async fn release(&self, name: &str) -> Result<(), LockError>;
}

/// No-op lock that always acquires — the single-instance default (no
/// coordination), pyfly's `LocalLock`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalLock;

#[async_trait]
impl DistributedLock for LocalLock {
    async fn try_acquire(&self, _name: &str, _ttl: Duration) -> Result<bool, LockError> {
        Ok(true)
    }

    async fn release(&self, _name: &str) -> Result<(), LockError> {
        Ok(())
    }
}

/// Real mutual exclusion **within one process** (not cross-process) with TTL
/// self-heal — pyfly's `InProcessDistributedLock`.
///
/// Prevents a slow job tick from overlapping its next tick in the same
/// process; for true multi-instance coordination use
/// [`RedisLock`](crate::RedisLock) or
/// [`PostgresAdvisoryLock`](crate::PostgresAdvisoryLock). A held name
/// auto-frees after its TTL so a crashed/never-released lock recovers.
#[derive(Debug, Default)]
pub struct InProcessLock {
    /// name → monotonic expiry instant.
    held: Mutex<HashMap<String, Instant>>,
}

impl InProcessLock {
    /// Returns an empty lock table.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl DistributedLock for InProcessLock {
    async fn try_acquire(&self, name: &str, ttl: Duration) -> Result<bool, LockError> {
        let mut held = self.held.lock().expect("in-process lock poisoned");
        let now = Instant::now();
        if let Some(expiry) = held.get(name) {
            if *expiry > now {
                return Ok(false);
            }
        }
        held.insert(name.to_string(), now + ttl);
        Ok(true)
    }

    async fn release(&self, name: &str) -> Result<(), LockError> {
        self.held
            .lock()
            .expect("in-process lock poisoned")
            .remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Port of pyfly test_local_lock_always_acquires.
    #[tokio::test]
    async fn local_lock_always_acquires() {
        let lock = LocalLock;
        assert!(lock.try_acquire("x", Duration::from_secs(1)).await.unwrap());
        lock.release("x").await.unwrap(); // no-op
        let _dyn_ok: Arc<dyn DistributedLock> = Arc::new(lock); // protocol parity
    }

    // Port of pyfly test_inprocess_lock_mutual_exclusion.
    #[tokio::test]
    async fn inprocess_lock_mutual_exclusion() {
        let lock = InProcessLock::new();
        let ttl = Duration::from_secs(30);
        assert!(lock.try_acquire("j", ttl).await.unwrap());
        assert!(!lock.try_acquire("j", ttl).await.unwrap());
        lock.release("j").await.unwrap();
        assert!(lock.try_acquire("j", ttl).await.unwrap());
        let _dyn_ok: Arc<dyn DistributedLock> = Arc::new(lock);
    }

    // TTL self-heal: an expired holder no longer blocks acquisition.
    #[tokio::test]
    async fn inprocess_lock_ttl_self_heal() {
        let lock = InProcessLock::new();
        // Zero TTL expires immediately — no sleeping needed.
        assert!(lock.try_acquire("j", Duration::ZERO).await.unwrap());
        assert!(lock
            .try_acquire("j", Duration::from_secs(30))
            .await
            .unwrap());
        // And a live TTL still excludes.
        assert!(!lock
            .try_acquire("j", Duration::from_secs(30))
            .await
            .unwrap());
    }

    // Distinct names do not contend.
    #[tokio::test]
    async fn inprocess_lock_names_are_independent() {
        let lock = InProcessLock::new();
        let ttl = Duration::from_secs(30);
        assert!(lock.try_acquire("a", ttl).await.unwrap());
        assert!(lock.try_acquire("b", ttl).await.unwrap());
    }
}
