//! Pointcut expression matching for AOP advice targeting.
//!
//! This is a 1:1 port of pyfly's `pyfly.aop.pointcut` module
//! (`_segment_to_regex` / `_pattern_to_regex` / `matches_pointcut`). The
//! pattern language is dot-segmented against *qualified names* of the form
//! `stereotype.ClassName.method` (e.g. `service.OrderService.create`):
//!
//! * `*` matches exactly one dot-separated segment (never crosses a dot).
//! * `**` matches one or more segments (crosses dots — any depth).
//! * Partial globs inside a segment use fnmatch rules, where `*` expands to
//!   `[^.]*` (within one segment) and `?` to `[^.]`; everything else is taken
//!   literally — e.g. `get_*` matches `get_order`, `*Service` matches
//!   `OrderService`.

use std::sync::Arc;

use regex::Regex;

/// Check whether `qualified_name` matches a pointcut `pattern`.
///
/// This compiles the pattern on every call — for repeated matching against the
/// same pattern, compile once with [`Pointcut::compile`] and reuse it.
///
/// # Pattern syntax
///
/// * `*` — matches exactly one dot-separated segment.
/// * `**` — matches one or more segments (crosses dots).
/// * Partial globs in any segment use fnmatch rules, e.g. `get_*` matches
///   `get_order`.
///
/// # Examples
///
/// ```
/// use firefly_aop::matches_pointcut;
///
/// assert!(matches_pointcut("service.*.*", "service.OrderService.create"));
/// assert!(matches_pointcut("**.*Service.*", "a.b.c.OrderService.create"));
/// assert!(matches_pointcut("mymod.MyClass.get_*", "mymod.MyClass.get_order"));
/// assert!(!matches_pointcut("*.my_method", "a.b.MyClass.my_method"));
/// ```
#[must_use]
pub fn matches_pointcut(pattern: &str, qualified_name: &str) -> bool {
    Pointcut::compile(pattern).is_match(qualified_name)
}

/// Convert a single pattern segment to a regex fragment.
///
/// Handles literal text, `*` (single segment), `**` (any depth), and partial
/// globs like `get_*` or `*Service`. This mirrors pyfly's `_segment_to_regex`
/// character-for-character.
fn segment_to_regex(seg: &str) -> String {
    if seg == "**" {
        // Matches one or more dot-separated segments.
        return r"(?:[^.]+\.)*[^.]+".to_string();
    }
    if seg == "*" {
        // Matches exactly one segment (no dots).
        return r"[^.]+".to_string();
    }

    // Partial glob — translate character-by-character, keeping `*` as `[^.]*`
    // (within one segment) and escaping everything else.
    let mut parts = String::new();
    for ch in seg.chars() {
        match ch {
            '*' => parts.push_str("[^.]*"),
            '?' => parts.push_str("[^.]"),
            other => parts.push_str(&regex::escape(&other.to_string())),
        }
    }
    parts
}

/// Convert a pointcut pattern string into the equivalent anchored regex source.
///
/// Segments are joined on a literal `.` and the whole expression is anchored
/// with `^…$` so it behaves like Python's `re.fullmatch` (which only matches if
/// the pattern consumes the entire string).
fn pattern_to_regex_source(pattern: &str) -> String {
    let parts: Vec<String> = pattern.split('.').map(segment_to_regex).collect();
    let full = parts.join(r"\.");
    // `fullmatch` semantics: anchor both ends.
    format!("^(?:{full})$")
}

/// A compiled pointcut expression.
///
/// Compile once and reuse for repeated matching (the registry holds one of
/// these per binding so patterns are never recompiled per dispatch).
#[derive(Debug, Clone)]
pub struct Pointcut {
    pattern: Arc<str>,
    regex: Regex,
}

impl Pointcut {
    /// Compile a pointcut `pattern` into a reusable matcher.
    ///
    /// # Panics
    ///
    /// Panics only if the generated regex is somehow invalid; the generator
    /// escapes all literal characters, so this cannot happen for any input.
    #[must_use]
    pub fn compile(pattern: &str) -> Self {
        let source = pattern_to_regex_source(pattern);
        let regex = Regex::new(&source).expect("pointcut regex is always valid by construction");
        Self {
            pattern: Arc::from(pattern),
            regex,
        }
    }

    /// Return the original pattern string this pointcut was compiled from.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Test whether `qualified_name` matches this pointcut.
    #[must_use]
    pub fn is_match(&self, qualified_name: &str) -> bool {
        self.regex.is_match(qualified_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Port of pyfly tests/aop/test_pointcut.py::TestMatchesPointcut ----

    #[test]
    fn test_exact_match() {
        assert!(matches_pointcut(
            "service.OrderService.create",
            "service.OrderService.create"
        ));
    }

    #[test]
    fn test_star_matches_single_segment_method() {
        assert!(matches_pointcut(
            "service.OrderService.*",
            "service.OrderService.create"
        ));
    }

    #[test]
    fn test_star_matches_single_segment_class() {
        assert!(matches_pointcut(
            "service.*.create",
            "service.OrderService.create"
        ));
    }

    #[test]
    fn test_star_matches_single_segment_module() {
        assert!(matches_pointcut(
            "*.OrderService.create",
            "service.OrderService.create"
        ));
    }

    #[test]
    fn test_star_all_segments() {
        assert!(matches_pointcut("*.*.*", "service.OrderService.create"));
    }

    #[test]
    fn test_doublestar_any_depth() {
        assert!(matches_pointcut(
            "**.*Service.*",
            "a.b.c.OrderService.create"
        ));
    }

    #[test]
    fn test_doublestar_single_depth() {
        assert!(matches_pointcut("**.*", "module.method"));
    }

    #[test]
    fn test_doublestar_deep() {
        assert!(matches_pointcut("**.do_work", "a.b.c.d.e.do_work"));
    }

    #[test]
    fn test_no_match_completely_different() {
        assert!(!matches_pointcut(
            "service.OrderService.create",
            "other.Foo.bar"
        ));
    }

    #[test]
    fn test_star_does_not_cross_dots() {
        assert!(!matches_pointcut("*.my_method", "a.b.MyClass.my_method"));
    }

    #[test]
    fn test_prefix_wildcard_matches() {
        assert!(matches_pointcut(
            "mymod.MyClass.get_*",
            "mymod.MyClass.get_order"
        ));
    }

    #[test]
    fn test_prefix_wildcard_no_match() {
        assert!(!matches_pointcut(
            "mymod.MyClass.get_*",
            "mymod.MyClass.set_order"
        ));
    }

    #[test]
    fn test_service_star_star() {
        assert!(matches_pointcut(
            "service.*.*",
            "service.OrderService.create"
        ));
    }

    #[test]
    fn test_service_star_star_no_extra_depth() {
        assert!(!matches_pointcut(
            "service.*.*",
            "service.sub.OrderService.create"
        ));
    }

    // ---- Compiled Pointcut reuse ----

    #[test]
    fn compiled_pointcut_reuse_matches_free_function() {
        let pc = Pointcut::compile("**.*Service.*");
        assert_eq!(pc.pattern(), "**.*Service.*");
        assert!(pc.is_match("a.b.c.OrderService.create"));
        assert!(!pc.is_match("a.b.c.OrderRepo.create"));
    }

    #[test]
    fn question_mark_glob_matches_single_char() {
        // `?` -> `[^.]`, one non-dot char.
        assert!(matches_pointcut("m.C.get?", "m.C.geta"));
        assert!(!matches_pointcut("m.C.get?", "m.C.get"));
        // does not cross a dot
        assert!(!matches_pointcut("m.C.get?", "m.C.ge.a"));
    }

    #[test]
    fn special_regex_chars_in_literal_segment_are_escaped() {
        // A literal segment containing regex metacharacters must match literally.
        assert!(matches_pointcut("m.C.do+work", "m.C.do+work"));
        assert!(!matches_pointcut("m.C.do+work", "m.C.doowork"));
    }
}
