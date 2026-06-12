//! Tiny shell-style glob matcher used by the filter exclude-pattern
//! surface — the Rust analog of the `fnmatch` calls in pyfly's
//! `OncePerRequestFilter.should_not_filter`. Supports `*` (any run of
//! characters, including `/`, exactly like `fnmatch`) and `?` (any
//! single character).

/// Returns whether `path` matches the shell-style `pattern`.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    fn inner(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') => {
                // `*` matches zero or more of anything (fnmatch is not
                // path-aware, so `/` is included).
                (0..=s.len()).any(|i| inner(&p[1..], &s[i..]))
            }
            Some(b'?') => !s.is_empty() && inner(&p[1..], &s[1..]),
            Some(&c) => s.first() == Some(&c) && inner(&p[1..], &s[1..]),
        }
    }
    inner(pattern.as_bytes(), path.as_bytes())
}

/// Returns whether `path` matches any of `patterns`.
pub(crate) fn matches_any(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|p| glob_match(p, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_semantics_match_fnmatch() {
        assert!(glob_match("/actuator/*", "/actuator/prometheus"));
        assert!(glob_match("/actuator/*", "/actuator/a/b"));
        assert!(glob_match("/admin/api/sse/*", "/admin/api/sse/metrics"));
        assert!(glob_match("/health", "/health"));
        assert!(!glob_match("/health", "/healthz"));
        assert!(glob_match("/a?c", "/abc"));
        assert!(!glob_match("/a?c", "/ac"));
        assert!(!glob_match("/actuator/*", "/api/users"));
    }
}
