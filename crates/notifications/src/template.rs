//! Notification template engine port and built-in implementations (pyfly parity).
//!
//! The Rust counterpart of `pyfly.notifications.template`. A
//! [`TemplateEngine`] renders a template id + data map to a string; the default
//! email service writes the result to `body_html`.
//!
//! # Precedence (with [`DefaultEmailService`](crate::DefaultEmailService))
//!
//! 1. **Engine present + `template_id` set** — the engine renders the template
//!    locally and the result is stored as `body_html`; `template_id` /
//!    `template_data` are cleared so provider-native routing is **not** also
//!    triggered.
//! 2. **No engine** (default) — `template_id` / `template_data` are forwarded to
//!    the provider untouched, enabling provider-native templates (e.g. SendGrid
//!    Dynamic Templates).
//!
//! Built-ins: [`NoOpTemplateEngine`] (always errors — the safe default) and,
//! behind the `minijinja` feature (on by default),
//! [`MiniJinjaTemplateEngine`] — a Jinja-compatible engine with HTML
//! autoescaping, the Rust counterpart of pyfly's `Jinja2TemplateEngine`.

use std::collections::HashMap;

use async_trait::async_trait;

/// Errors raised while rendering a notification template.
///
/// Mirrors pyfly's two failure modes: an unknown template id (Python raises
/// `KeyError`) and a no-op engine being asked to render (Python raises
/// `NotImplementedError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// The requested template id is not registered.
    #[error("unknown template_id: {0:?}")]
    UnknownTemplate(String),
    /// The [`NoOpTemplateEngine`] was asked to render a template.
    #[error("NoOpTemplateEngine cannot render {0:?}; inject a MiniJinjaTemplateEngine or configure pyfly.notifications.template.engine=minijinja")]
    NotImplemented(String),
    /// The template source failed to render (syntax/runtime error).
    #[error("template render error: {0}")]
    Render(String),
}

/// Port for rendering a notification template to a string.
///
/// Equivalent to pyfly's `NotificationTemplateEngine` protocol.
#[async_trait]
pub trait TemplateEngine: Send + Sync {
    /// Renders `template_id` with `data` and returns the rendered string.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::UnknownTemplate`] when the id is not registered,
    /// or [`TemplateError::Render`] when the template fails to render.
    async fn render(
        &self,
        template_id: &str,
        data: &HashMap<String, serde_json::Value>,
    ) -> Result<String, TemplateError>;
}

/// A template engine that always errors.
///
/// Equivalent to pyfly's `NoOpTemplateEngine`. This is the safe default for
/// contexts where no engine is configured: any render attempt fails loudly with
/// [`TemplateError::NotImplemented`] rather than silently sending empty bodies.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoOpTemplateEngine;

impl NoOpTemplateEngine {
    /// Returns a new no-op engine.
    pub fn new() -> Self {
        NoOpTemplateEngine
    }
}

#[async_trait]
impl TemplateEngine for NoOpTemplateEngine {
    async fn render(
        &self,
        template_id: &str,
        _data: &HashMap<String, serde_json::Value>,
    ) -> Result<String, TemplateError> {
        Err(TemplateError::NotImplemented(template_id.to_string()))
    }
}

#[cfg(feature = "minijinja")]
mod minijinja_engine {
    use super::*;
    use minijinja::{AutoEscape, Environment};

    /// A Jinja-compatible template engine backed by [`minijinja`].
    ///
    /// The Rust counterpart of pyfly's `Jinja2TemplateEngine`. Templates are
    /// registered up-front as raw source strings keyed by template id. HTML
    /// autoescaping is **always on** (matching Jinja2's `autoescape=True`), so
    /// HTML in template variables is escaped by default.
    ///
    /// # Example
    ///
    /// ```
    /// use std::collections::HashMap;
    ///
    /// use firefly_notifications::{MiniJinjaTemplateEngine, TemplateEngine};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let engine = MiniJinjaTemplateEngine::new([(
    ///     "welcome".to_string(),
    ///     "<h1>Hello, {{ name }}!</h1>".to_string(),
    /// )]);
    /// let mut data = HashMap::new();
    /// data.insert("name".to_string(), serde_json::json!("Alice"));
    /// assert_eq!(
    ///     engine.render("welcome", &data).await.unwrap(),
    ///     "<h1>Hello, Alice!</h1>"
    /// );
    /// # }
    /// ```
    pub struct MiniJinjaTemplateEngine {
        env: Environment<'static>,
        template_ids: Vec<String>,
    }

    impl MiniJinjaTemplateEngine {
        /// Builds an engine from an iterator of `(template_id, source)` pairs.
        ///
        /// # Panics
        ///
        /// Panics if a template source fails to compile — register only valid
        /// Jinja sources (parity with pyfly, which compiles lazily on render
        /// but raises the same way).
        pub fn new(templates: impl IntoIterator<Item = (String, String)>) -> Self {
            let mut env = Environment::new();
            // Always HTML-autoescape, matching Jinja2 `autoescape=True`.
            env.set_auto_escape_callback(|_name| AutoEscape::Html);
            let mut template_ids = Vec::new();
            for (id, source) in templates {
                env.add_template_owned(id.clone(), source)
                    .expect("template source must compile");
                template_ids.push(id);
            }
            MiniJinjaTemplateEngine { env, template_ids }
        }

        /// Builds an engine with no templates registered.
        pub fn empty() -> Self {
            Self::new([])
        }
    }

    #[async_trait]
    impl TemplateEngine for MiniJinjaTemplateEngine {
        async fn render(
            &self,
            template_id: &str,
            data: &HashMap<String, serde_json::Value>,
        ) -> Result<String, TemplateError> {
            let tmpl = self
                .env
                .get_template(template_id)
                .map_err(|_| TemplateError::UnknownTemplate(template_id.to_string()))?;
            tmpl.render(data)
                .map_err(|e| TemplateError::Render(e.to_string()))
        }
    }

    impl MiniJinjaTemplateEngine {
        /// Returns the registered template ids, sorted (used in diagnostics).
        pub fn template_ids(&self) -> Vec<String> {
            let mut ids = self.template_ids.clone();
            ids.sort();
            ids
        }
    }
}

#[cfg(feature = "minijinja")]
pub use minijinja_engine::MiniJinjaTemplateEngine;
