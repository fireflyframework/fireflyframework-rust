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

//! Runtime configuration reload (pyfly `Config.reload_from_sources()` /
//! Spring Cloud `ContextRefresher` parity).
//!
//! [`ReloadableConfig<T>`] keeps the source chain alive after the initial
//! bind. [`reload`](ReloadableConfig::reload) replays the exact merge →
//! placeholder-resolution → bind pipeline, atomically swaps in the new
//! snapshot, and returns the **changed top-level keys** — the wire shape
//! Spring Cloud's `POST /actuator/refresh` reports. Readers either grab the
//! current snapshot with [`get`](ReloadableConfig::get) (an `Arc<T>` clone,
//! the refresh-scope idiom: re-read per call instead of bean eviction) or
//! await changes via [`subscribe`](ReloadableConfig::subscribe) (a
//! `tokio::sync::watch` receiver).
//!
//! The object-safe [`Refresher`] trait is the hook actuator-style endpoints
//! wire up: `Arc<ReloadableConfig<T>>` coerces to `Arc<dyn Refresher>`.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::de::DeserializeOwned;
use tokio::sync::watch;

use crate::binder::bind;
use crate::error::ConfigError;
use crate::events::{ApplicationEventBus, RefreshScopeRefreshedEvent};
use crate::placeholder::resolve_placeholders;
use crate::source::{merge, Source};

/// The reload hook actuator-style refresh endpoints call (Spring Cloud
/// `ContextRefresher` parity). Implemented by [`ReloadableConfig`]; an
/// `Arc<ReloadableConfig<T>>` coerces to `Arc<dyn Refresher>`.
pub trait Refresher: Send + Sync {
    /// Replays the configuration load and returns the changed top-level
    /// keys, sorted (empty when nothing changed).
    fn refresh(&self) -> Result<Vec<String>, ConfigError>;
}

/// A bound configuration that can be re-read from its sources at runtime.
///
/// Construction loads once via [`ReloadableConfig::load`]; afterwards the
/// chain is replayed on every [`reload`](ReloadableConfig::reload) and the
/// new snapshot atomically replaces the old one. Concurrent readers always
/// see a consistent snapshot (an `Arc<T>`), never a half-merged state.
///
/// ```
/// use firefly_config::{FlagSource, ReloadableConfig, Source};
/// use serde::Deserialize;
///
/// #[derive(Debug, Deserialize)]
/// struct Cfg { feature: String }
///
/// let flags = FlagSource::new();
/// flags.set("feature", "alpha");
/// let sources: Vec<Box<dyn Source>> = vec![Box::new(flags.clone())];
/// let cfg: ReloadableConfig<Cfg> = ReloadableConfig::load(sources)?;
/// assert_eq!(cfg.get().feature, "alpha");
///
/// flags.set("feature", "beta");
/// let changed = cfg.reload()?;
/// assert_eq!(changed, vec!["feature".to_string()]);
/// assert_eq!(cfg.get().feature, "beta");
/// # Ok::<(), firefly_config::ConfigError>(())
/// ```
pub struct ReloadableConfig<T> {
    sources: Vec<Box<dyn Source>>,
    tx: watch::Sender<Arc<T>>,
    flat: Mutex<HashMap<String, String>>,
}

impl<T: DeserializeOwned + Send + Sync + 'static> ReloadableConfig<T> {
    /// Merges the sources, resolves `${...}` placeholders, binds the first
    /// snapshot, and keeps the chain for later [`reload`](Self::reload)s.
    pub fn load(sources: Vec<Box<dyn Source>>) -> Result<Self, ConfigError> {
        let flat = merged_resolved(&sources)?;
        let value: T = bind(&flat)?;
        let (tx, _rx) = watch::channel(Arc::new(value));
        Ok(ReloadableConfig {
            sources,
            tx,
            flat: Mutex::new(flat),
        })
    }

    /// The current snapshot. Cheap (`Arc` clone) — refresh-scoped consumers
    /// call this per use instead of caching the inner value.
    pub fn get(&self) -> Arc<T> {
        self.tx.borrow().clone()
    }

    /// A watch receiver that observes every successful reload; the borrowed
    /// value is the latest snapshot.
    pub fn subscribe(&self) -> watch::Receiver<Arc<T>> {
        self.tx.subscribe()
    }

    /// Replays the exact source merge (+ placeholder resolution + bind) and
    /// atomically swaps in the result, returning the sorted top-level keys
    /// whose effective values changed (added, removed, or modified).
    ///
    /// Errors leave the previous snapshot in place; concurrent `reload`
    /// calls serialize on an internal lock.
    pub fn reload(&self) -> Result<Vec<String>, ConfigError> {
        let mut old = lock(&self.flat);
        let new_flat = merged_resolved(&self.sources)?;
        let value: T = bind(&new_flat)?;
        let changed = changed_top_level_keys(&old, &new_flat);
        *old = new_flat;
        self.tx.send_replace(Arc::new(value));
        Ok(changed)
    }

    /// Replays the source chain (like [`reload`](Self::reload)) and, on
    /// success, publishes a [`RefreshScopeRefreshedEvent`] carrying the
    /// changed top-level keys onto `bus`, then returns those keys — the
    /// Rust port of pyfly's `ContextRefresher.refresh()`, which reloads the
    /// config sources and publishes a `RefreshScopeRefreshedEvent` to the
    /// `ApplicationEventBus` before returning the evicted keys.
    ///
    /// The event is published on **every** successful reload, including one
    /// that changes nothing (the event then carries an empty `refreshed`
    /// list) — matching pyfly, which always publishes after a refresh. A
    /// failed reload propagates the [`ConfigError`] and publishes nothing,
    /// leaving the previous snapshot in place.
    ///
    /// [`ApplicationEventBus`] is single-threaded (`!Send`), so this method
    /// is the bridge between the `Send + Sync` [`ReloadableConfig`] and the
    /// in-process event bus: call it from the thread that owns the bus
    /// (typically the actuator `/refresh` handler), not across threads.
    ///
    /// ```
    /// use firefly_config::{
    ///     ApplicationEventBus, FlagSource, RefreshScopeRefreshedEvent, ReloadableConfig, Source,
    /// };
    /// use serde::Deserialize;
    /// use std::cell::RefCell;
    /// use std::rc::Rc;
    ///
    /// #[derive(Debug, Deserialize)]
    /// struct Cfg { feature: String }
    ///
    /// let flags = FlagSource::new();
    /// flags.set("feature", "alpha");
    /// let sources: Vec<Box<dyn Source>> = vec![Box::new(flags.clone())];
    /// let cfg: ReloadableConfig<Cfg> = ReloadableConfig::load(sources)?;
    ///
    /// let bus = ApplicationEventBus::new();
    /// let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    /// let s = seen.clone();
    /// bus.subscribe::<RefreshScopeRefreshedEvent, _>(move |e| {
    ///     *s.borrow_mut() = e.refreshed.clone();
    /// });
    ///
    /// flags.set("feature", "beta");
    /// let changed = cfg.reload_and_publish(&bus)?;
    /// assert_eq!(changed, vec!["feature".to_string()]);
    /// assert_eq!(*seen.borrow(), vec!["feature".to_string()]);
    /// # Ok::<(), firefly_config::ConfigError>(())
    /// ```
    pub fn reload_and_publish(
        &self,
        bus: &ApplicationEventBus,
    ) -> Result<Vec<String>, ConfigError> {
        let changed = self.reload()?;
        bus.publish(&RefreshScopeRefreshedEvent::new(changed.clone()));
        Ok(changed)
    }
}

impl<T: DeserializeOwned + Send + Sync + 'static> Refresher for ReloadableConfig<T> {
    fn refresh(&self) -> Result<Vec<String>, ConfigError> {
        self.reload()
    }
}

/// Merge + placeholder resolution — the pipeline both `load` and `reload`
/// replay.
fn merged_resolved(sources: &[Box<dyn Source>]) -> Result<HashMap<String, String>, ConfigError> {
    let flat = merge(sources)?;
    resolve_placeholders(&flat)
}

/// Sorted, de-duplicated top-level segments of every key whose value
/// differs between the two maps.
fn changed_top_level_keys(
    old: &HashMap<String, String>,
    new: &HashMap<String, String>,
) -> Vec<String> {
    let mut changed = BTreeSet::new();
    for key in old.keys().chain(new.keys()) {
        if old.get(key) != new.get(key) {
            let top = key.split('.').next().unwrap_or(key);
            changed.insert(top.to_string());
        }
    }
    changed.into_iter().collect()
}

fn lock<'a>(mutex: &'a Mutex<HashMap<String, String>>) -> MutexGuard<'a, HashMap<String, String>> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{FlagSource, StaticSource};
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Cfg {
        feature: String,
        port: u16,
    }

    fn entries(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn reload_swaps_snapshot_and_reports_changed_keys() {
        let flags = FlagSource::new();
        flags.set("feature", "alpha");
        flags.set("port", "1");
        let sources: Vec<Box<dyn Source>> = vec![Box::new(flags.clone())];
        let cfg: ReloadableConfig<Cfg> = ReloadableConfig::load(sources).unwrap();
        assert_eq!(cfg.get().feature, "alpha");

        flags.set("feature", "beta");
        let changed = cfg.reload().unwrap();
        assert_eq!(changed, vec!["feature".to_string()]);
        assert_eq!(cfg.get().feature, "beta");
        assert_eq!(cfg.get().port, 1);
    }

    #[test]
    fn reload_with_no_changes_returns_empty() {
        let sources: Vec<Box<dyn Source>> = vec![Box::new(StaticSource::new(
            "static",
            entries(&[("feature", "x"), ("port", "2")]),
        ))];
        let cfg: ReloadableConfig<Cfg> = ReloadableConfig::load(sources).unwrap();
        assert!(cfg.reload().unwrap().is_empty());
        assert_eq!(cfg.get().feature, "x");
    }

    #[test]
    fn changed_keys_collapse_to_top_level_and_sort() {
        let old = entries(&[("web.port", "1"), ("web.host", "h"), ("cache.ttl", "5")]);
        let new = entries(&[("web.port", "2"), ("web.host", "h"), ("app.name", "n")]);
        assert_eq!(
            changed_top_level_keys(&old, &new),
            vec!["app".to_string(), "cache".to_string(), "web".to_string()]
        );
    }

    #[test]
    fn failed_reload_keeps_previous_snapshot() {
        let flags = FlagSource::new();
        flags.set("feature", "alpha");
        flags.set("port", "8");
        let sources: Vec<Box<dyn Source>> = vec![Box::new(flags.clone())];
        let cfg: ReloadableConfig<Cfg> = ReloadableConfig::load(sources).unwrap();

        flags.set("port", "not-a-number");
        assert!(cfg.reload().is_err());
        assert_eq!(cfg.get().port, 8, "snapshot must survive a failed reload");
    }

    #[tokio::test]
    async fn subscribers_observe_reloads() {
        let flags = FlagSource::new();
        flags.set("feature", "alpha");
        flags.set("port", "1");
        let sources: Vec<Box<dyn Source>> = vec![Box::new(flags.clone())];
        let cfg: ReloadableConfig<Cfg> = ReloadableConfig::load(sources).unwrap();

        let mut rx = cfg.subscribe();
        flags.set("feature", "beta");
        cfg.reload().unwrap();
        rx.changed().await.unwrap();
        assert_eq!(rx.borrow().feature, "beta");
    }

    #[test]
    fn reload_and_publish_fires_event_with_changed_keys() {
        use crate::events::{ApplicationEventBus, RefreshScopeRefreshedEvent};
        use std::cell::RefCell;
        use std::rc::Rc;

        let flags = FlagSource::new();
        flags.set("feature", "alpha");
        flags.set("port", "1");
        let sources: Vec<Box<dyn Source>> = vec![Box::new(flags.clone())];
        let cfg: ReloadableConfig<Cfg> = ReloadableConfig::load(sources).unwrap();

        let bus = ApplicationEventBus::new();
        let seen: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        bus.subscribe::<RefreshScopeRefreshedEvent, _>(move |e| {
            s.borrow_mut().push(e.refreshed.clone());
        });

        flags.set("feature", "beta");
        let changed = cfg.reload_and_publish(&bus).unwrap();
        assert_eq!(changed, vec!["feature".to_string()]);
        // The listener saw exactly one event carrying the changed keys.
        assert_eq!(*seen.borrow(), vec![vec!["feature".to_string()]]);
    }

    #[test]
    fn reload_and_publish_fires_empty_event_when_nothing_changed() {
        use crate::events::{ApplicationEventBus, RefreshScopeRefreshedEvent};
        use std::cell::RefCell;
        use std::rc::Rc;

        let sources: Vec<Box<dyn Source>> = vec![Box::new(StaticSource::new(
            "static",
            entries(&[("feature", "x"), ("port", "2")]),
        ))];
        let cfg: ReloadableConfig<Cfg> = ReloadableConfig::load(sources).unwrap();

        let bus = ApplicationEventBus::new();
        let count = Rc::new(RefCell::new(0usize));
        let c = count.clone();
        bus.subscribe::<RefreshScopeRefreshedEvent, _>(move |e| {
            assert!(e.refreshed.is_empty());
            *c.borrow_mut() += 1;
        });

        // pyfly always publishes after a refresh, even a no-op one.
        assert!(cfg.reload_and_publish(&bus).unwrap().is_empty());
        assert_eq!(*count.borrow(), 1);
    }

    #[test]
    fn reload_and_publish_does_not_fire_on_failure() {
        use crate::events::{ApplicationEventBus, RefreshScopeRefreshedEvent};
        use std::cell::RefCell;
        use std::rc::Rc;

        let flags = FlagSource::new();
        flags.set("feature", "alpha");
        flags.set("port", "8");
        let sources: Vec<Box<dyn Source>> = vec![Box::new(flags.clone())];
        let cfg: ReloadableConfig<Cfg> = ReloadableConfig::load(sources).unwrap();

        let bus = ApplicationEventBus::new();
        let count = Rc::new(RefCell::new(0usize));
        let c = count.clone();
        bus.subscribe::<RefreshScopeRefreshedEvent, _>(move |_| *c.borrow_mut() += 1);

        flags.set("port", "not-a-number");
        assert!(cfg.reload_and_publish(&bus).is_err());
        assert_eq!(*count.borrow(), 0, "no event on a failed reload");
        assert_eq!(cfg.get().port, 8, "snapshot survives the failed reload");
    }

    #[test]
    fn reloadable_config_coerces_to_dyn_refresher() {
        let sources: Vec<Box<dyn Source>> = vec![Box::new(StaticSource::new(
            "static",
            entries(&[("feature", "x"), ("port", "2")]),
        ))];
        let cfg: Arc<ReloadableConfig<Cfg>> = Arc::new(ReloadableConfig::load(sources).unwrap());
        let refresher: Arc<dyn Refresher> = cfg;
        assert!(refresher.refresh().unwrap().is_empty());
    }
}
