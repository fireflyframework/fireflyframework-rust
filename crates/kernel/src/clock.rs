//! Testable time: the [`Clock`] abstraction and its implementations.

use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

/// Abstracts the wall clock so tests can substitute a fixed or
/// programmable time source. Equivalent to the Java `Clock`, the .NET
/// `IClock`, and the Go `Clock` interface.
pub trait Clock: Send + Sync {
    /// Returns the current instant according to this clock.
    fn now(&self) -> DateTime<Utc>;
}

/// Returns the current wall-clock time via [`Utc::now`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Always returns the wrapped instant. Useful for deterministic tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// A thread-safe clock whose value can be advanced from tests. The
/// default value is the Unix epoch.
#[derive(Debug)]
pub struct MutableClock {
    t: Mutex<DateTime<Utc>>,
}

impl Default for MutableClock {
    fn default() -> Self {
        Self::new(DateTime::UNIX_EPOCH)
    }
}

impl MutableClock {
    /// Returns a mutable clock initialised to `t`.
    pub fn new(t: DateTime<Utc>) -> Self {
        Self { t: Mutex::new(t) }
    }

    /// Moves the clock forward by `d`.
    pub fn advance(&self, d: Duration) {
        let mut t = self.lock();
        *t += d;
    }

    /// Overwrites the clock value to `t`.
    pub fn set(&self, t: DateTime<Utc>) {
        *self.lock() = t;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DateTime<Utc>> {
        self.t
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Clock for MutableClock {
    fn now(&self) -> DateTime<Utc> {
        *self.lock()
    }
}
