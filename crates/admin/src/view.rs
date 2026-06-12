//! Custom dashboard-view plugin point — the Rust rendering of pyfly's
//! `AdminViewExtension` protocol + `AdminViewRegistry`.
//!
//! A consumer implements [`AdminView`] and hands instances to
//! [`AdminDeps`](crate::AdminDeps); the router collects them into an
//! [`AdminViewRegistry`], exposes their identity on `GET /admin/api/views`,
//! and serves each view's payload on `GET /admin/api/views/{id}`.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

/// A custom admin dashboard view contributed by an application — the trait
/// form of pyfly's `AdminViewExtension` protocol (`view_id` / `display_name`
/// / `icon` / `get_data`).
///
/// Implementations are registered through
/// [`AdminDeps::views`](crate::AdminDeps::views) and surfaced under the
/// `/admin/api/views` route set. The trait is object-safe so views compose
/// behind `Arc<dyn AdminView>`.
#[async_trait]
pub trait AdminView: Send + Sync {
    /// Stable identifier (the registry key and the `/views/{id}` path
    /// segment), e.g. `"feature-flags"`.
    fn view_id(&self) -> &str;

    /// Human-readable label rendered in the SPA navigation.
    fn display_name(&self) -> &str;

    /// Icon name / glyph the SPA renders next to the label.
    fn icon(&self) -> &str;

    /// Produces the view's JSON payload, served on
    /// `GET /admin/api/views/{id}`.
    async fn data(&self) -> Value;
}

/// Collects the registered [`AdminView`] extensions, keyed by
/// [`view_id`](AdminView::view_id) — the Rust rendering of pyfly's
/// `AdminViewRegistry`. Built once from
/// [`AdminDeps::views`](crate::AdminDeps::views); last registration under a
/// given id wins (pyfly keeps the first, but the Rust wiring is explicit so
/// order is the caller's).
#[derive(Default, Clone)]
pub struct AdminViewRegistry {
    views: BTreeMap<String, Arc<dyn AdminView>>,
}

impl AdminViewRegistry {
    /// Returns an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers (or replaces) a view under its
    /// [`view_id`](AdminView::view_id).
    pub fn register(&mut self, view: Arc<dyn AdminView>) {
        self.views.insert(view.view_id().to_string(), view);
    }

    /// Builds a registry from a list of views (the wiring path used by the
    /// router).
    pub fn from_views(views: Vec<Arc<dyn AdminView>>) -> Self {
        let mut registry = Self::new();
        for view in views {
            registry.register(view);
        }
        registry
    }

    /// Looks up a view by id.
    pub fn get(&self, view_id: &str) -> Option<&Arc<dyn AdminView>> {
        self.views.get(view_id)
    }

    /// The registered views, sorted by id.
    pub fn views(&self) -> impl Iterator<Item = &Arc<dyn AdminView>> {
        self.views.values()
    }

    /// The `{id, name, icon}` summary rows served on `GET /admin/api/views`
    /// (pyfly's `_handle_views`).
    pub fn summaries(&self) -> Value {
        let views: Vec<Value> = self
            .views
            .values()
            .map(|v| {
                serde_json::json!({
                    "id": v.view_id(),
                    "name": v.display_name(),
                    "icon": v.icon(),
                })
            })
            .collect();
        serde_json::json!({ "views": views })
    }

    /// Number of registered views.
    pub fn len(&self) -> usize {
        self.views.len()
    }

    /// Whether no views are registered.
    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DemoView;

    #[async_trait]
    impl AdminView for DemoView {
        fn view_id(&self) -> &str {
            "demo"
        }
        fn display_name(&self) -> &str {
            "Demo"
        }
        fn icon(&self) -> &str {
            "star"
        }
        async fn data(&self) -> Value {
            serde_json::json!({ "ok": true })
        }
    }

    #[test]
    fn registry_lists_summaries() {
        let registry = AdminViewRegistry::from_views(vec![Arc::new(DemoView)]);
        let summaries = registry.summaries();
        assert_eq!(summaries["views"][0]["id"], "demo");
        assert_eq!(summaries["views"][0]["name"], "Demo");
        assert_eq!(summaries["views"][0]["icon"], "star");
    }

    #[tokio::test]
    async fn registry_resolves_data() {
        let registry = AdminViewRegistry::from_views(vec![Arc::new(DemoView)]);
        let view = registry.get("demo").expect("registered");
        assert_eq!(view.data().await, serde_json::json!({ "ok": true }));
        assert!(registry.get("missing").is_none());
        assert_eq!(registry.len(), 1);
    }
}
