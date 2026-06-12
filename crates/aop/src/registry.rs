//! [`AspectRegistry`] — collects ordered aspect bindings and queries them by
//! qualified name.
//!
//! pyfly's `AspectRegistry.register(instance)` reflects over the aspect's
//! decorated methods and stores one [`AdviceBinding`] per advice method, each
//! carrying the method's own `advice_type` + `pointcut` and the *aspect's*
//! `@order`. Bindings are kept globally sorted by order.
//!
//! The Rust port keeps the same shape with one idiom change: an aspect is a
//! single [`Aspect`] implementation (its five hooks correspond to pyfly's five
//! advice kinds), and its pointcut + order are supplied explicitly at
//! registration time — `register(aspect, pointcut, order)` — because Rust has
//! no decorator reflection. Each `register` call therefore produces exactly one
//! binding, and bindings stay globally sorted by `order` (ascending: lower
//! order runs first, matching pyfly).

use std::cmp::Ordering;
use std::sync::Arc;

use crate::aspect::Aspect;
use crate::pointcut::Pointcut;

/// A single aspect bound to a pointcut, with an ordering value.
///
/// This is the Rust counterpart of pyfly's `AdviceBinding`. Where pyfly's
/// binding names a single `advice_type` + `handler`, the Rust binding holds the
/// whole [`Aspect`] (all five hooks) since one trait impl bundles them — the
/// chain executor invokes the relevant hooks of every matching binding in the
/// pyfly ordering.
#[derive(Clone)]
pub struct AdviceBinding {
    /// The bound aspect (shared so the same instance can match many sites).
    pub aspect: Arc<dyn Aspect>,
    /// The compiled pointcut the aspect is bound to.
    pub pointcut: Pointcut,
    /// The ordering value (pyfly: `aspect_order`). Lower runs first.
    pub order: i32,
    /// Monotonic registration index, used as a stable tie-breaker so equal
    /// `order` values preserve first-registered-first (mirroring Python's
    /// stable `list.sort`).
    seq: u64,
}

impl AdviceBinding {
    /// The pointcut pattern string this binding matches against.
    #[must_use]
    pub fn pattern(&self) -> &str {
        self.pointcut.pattern()
    }
}

impl std::fmt::Debug for AdviceBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdviceBinding")
            .field("pointcut", &self.pointcut.pattern())
            .field("order", &self.order)
            .field("seq", &self.seq)
            .finish()
    }
}

/// Registry that collects aspect bindings and provides advice lookups.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use firefly_aop::{AspectRegistry, NoopAspect};
///
/// let mut registry = AspectRegistry::new();
/// registry.register(Arc::new(NoopAspect), "service.*.*", 0);
/// registry.register(Arc::new(NoopAspect), "service.*.create", 10);
///
/// let matches = registry.get_matching("service.OrderService.create");
/// assert_eq!(matches.len(), 2);
/// // delete only matches the broad pointcut
/// assert_eq!(registry.get_matching("service.OrderService.delete").len(), 1);
/// ```
#[derive(Clone, Default)]
pub struct AspectRegistry {
    bindings: Vec<AdviceBinding>,
    next_seq: u64,
}

impl std::fmt::Debug for AspectRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AspectRegistry")
            .field("bindings", &self.bindings.len())
            .finish()
    }
}

impl AspectRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `aspect` bound to `pointcut` with the given `order`.
    ///
    /// The pointcut is compiled once here and reused for all future matches.
    /// Bindings are kept globally sorted by `order` (ascending); equal orders
    /// preserve registration sequence, so the first-registered aspect is
    /// outermost — exactly pyfly's behaviour.
    pub fn register(&mut self, aspect: Arc<dyn Aspect>, pointcut: &str, order: i32) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.bindings.push(AdviceBinding {
            aspect,
            pointcut: Pointcut::compile(pointcut),
            order,
            seq,
        });
        self.sort();
    }

    /// Keep bindings globally sorted by `order`, then by registration sequence.
    fn sort(&mut self) {
        self.bindings.sort_by(|a, b| match a.order.cmp(&b.order) {
            Ordering::Equal => a.seq.cmp(&b.seq),
            non_eq => non_eq,
        });
    }

    /// Return all registered bindings, sorted by `order` (pyfly:
    /// `get_all_bindings`).
    #[must_use]
    pub fn get_all_bindings(&self) -> &[AdviceBinding] {
        &self.bindings
    }

    /// Return the bindings whose pointcut matches `qualified_name`, preserving
    /// the global order (pyfly: `get_matching`).
    #[must_use]
    pub fn get_matching(&self, qualified_name: &str) -> Vec<AdviceBinding> {
        self.bindings
            .iter()
            .filter(|b| b.pointcut.is_match(qualified_name))
            .cloned()
            .collect()
    }

    /// `true` when no bindings match `qualified_name` — the executor uses this
    /// to skip weaving entirely, matching pyfly's "non-matching methods
    /// untouched".
    #[must_use]
    pub fn has_match(&self, qualified_name: &str) -> bool {
        self.bindings
            .iter()
            .any(|b| b.pointcut.is_match(qualified_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aspect::NoopAspect;

    // ---- Port of pyfly tests/aop/test_registry.py::TestAspectRegistry ----
    //
    // In pyfly, `LoggingAspect` declares TWO advice methods (before +
    // after_returning) on one pointcut "service.*.*". The Rust idiom binds one
    // Aspect impl per `register` call, so the direct analogue of "LoggingAspect
    // produces 2 bindings" is "register the logging aspect once on
    // service.*.*". We assert the matching/ordering behaviour the pyfly tests
    // ultimately verify.

    fn logging() -> Arc<dyn Aspect> {
        Arc::new(NoopAspect)
    }

    #[test]
    fn test_register_single_aspect_binding_count() {
        let mut registry = AspectRegistry::new();
        registry.register(logging(), "service.*.*", 0);
        assert_eq!(registry.get_all_bindings().len(), 1);
    }

    #[test]
    fn test_bindings_have_correct_pointcuts() {
        let mut registry = AspectRegistry::new();
        registry.register(logging(), "service.*.*", 0);
        let bindings = registry.get_all_bindings();
        assert_eq!(bindings[0].pattern(), "service.*.*");
    }

    #[test]
    fn test_multiple_aspects_ordered_by_order() {
        let mut registry = AspectRegistry::new();
        registry.register(Arc::new(NoopAspect), "service.*.*", 0); // logging, order 0
        registry.register(Arc::new(NoopAspect), "service.*.create", 10); // security, order 10
        registry.register(Arc::new(NoopAspect), "service.*.*", -5); // early, order -5

        let bindings = registry.get_all_bindings();
        let orders: Vec<i32> = bindings.iter().map(|b| b.order).collect();
        let mut sorted = orders.clone();
        sorted.sort_unstable();
        assert_eq!(orders, sorted);
        assert_eq!(bindings[0].order, -5);
        assert_eq!(bindings[bindings.len() - 1].order, 10);
    }

    #[test]
    fn test_get_matching_returns_matching_bindings() {
        let mut registry = AspectRegistry::new();
        registry.register(Arc::new(NoopAspect), "service.*.*", 0);
        registry.register(Arc::new(NoopAspect), "service.*.create", 10);

        let matches = registry.get_matching("service.OrderService.create");
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn test_get_matching_partial() {
        let mut registry = AspectRegistry::new();
        registry.register(Arc::new(NoopAspect), "service.*.*", 0);
        registry.register(Arc::new(NoopAspect), "service.*.create", 10);

        // "delete" does not match the "service.*.create" binding.
        let matches = registry.get_matching("service.OrderService.delete");
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn test_no_match_for_unrelated_qualified_name() {
        let mut registry = AspectRegistry::new();
        registry.register(Arc::new(NoopAspect), "service.*.*", 0);
        registry.register(Arc::new(NoopAspect), "service.*.create", 10);

        let matches = registry.get_matching("repo.UserRepo.find_by_id");
        assert_eq!(matches.len(), 0);
        assert!(!registry.has_match("repo.UserRepo.find_by_id"));
    }

    #[test]
    fn equal_order_preserves_registration_sequence() {
        let mut registry = AspectRegistry::new();
        registry.register(Arc::new(NoopAspect), "a.*.first", 5);
        registry.register(Arc::new(NoopAspect), "a.*.second", 5);
        let bindings = registry.get_all_bindings();
        assert_eq!(bindings[0].pattern(), "a.*.first");
        assert_eq!(bindings[1].pattern(), "a.*.second");
    }
}
