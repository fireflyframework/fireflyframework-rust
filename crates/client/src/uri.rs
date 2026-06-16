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

//! URI helpers shared by the declarative HTTP-interface client codegen.
//!
//! [`encode_path_segment`] percent-encodes a value before it is spliced into a
//! single URL path segment, so a path variable like `id = "a/b"` cannot inject
//! an extra segment (or a query / fragment). This is the Rust analog of Spring's
//! `UriComponentsBuilder` path-variable encoding, used by the `#[http_client]`
//! macro when it expands a `:name` template hole.

/// Percent-encodes `s` for safe inclusion in a **single** URL path segment.
///
/// Every byte outside the RFC 3986 "unreserved" set plus the `sub-delims` and a
/// small set of path-safe punctuation (`@`, `:`) that are legal *within* a
/// segment is escaped as `%XX`. Crucially the segment separators and component
/// delimiters — `/`, `?`, `#`, `%`, and whitespace — are always escaped, so a
/// caller-supplied value can never break out of its segment.
///
/// This mirrors Spring's `UriComponentsBuilder` path-variable encoding, which
/// the [`http_client`](firefly_macros) macro relies on when substituting a
/// `:name` template hole.
///
/// # Examples
///
/// ```
/// use firefly_client::encode_path_segment;
///
/// assert_eq!(encode_path_segment("42"), "42");
/// assert_eq!(encode_path_segment("a/b"), "a%2Fb");
/// assert_eq!(encode_path_segment("a b?c#d%e"), "a%20b%3Fc%23d%25e");
/// ```
pub fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if is_path_segment_safe(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_digit(b >> 4));
            out.push(hex_digit(b & 0x0f));
        }
    }
    out
}

/// Whether `b` may appear literally in a single URL path segment.
///
/// The allowed set is RFC 3986 `pchar` minus `%` (which always introduces an
/// escape) — i.e. `unreserved` + `sub-delims` + `:` + `@`. Everything else,
/// including the segment / component delimiters `/ ? #` and any control or
/// non-ASCII byte, is escaped.
fn is_path_segment_safe(b: u8) -> bool {
    matches!(b,
        // unreserved: ALPHA / DIGIT / "-" / "." / "_" / "~"
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
        | b'-' | b'.' | b'_' | b'~'
        // sub-delims
        | b'!' | b'$' | b'&' | b'\'' | b'(' | b')'
        | b'*' | b'+' | b',' | b';' | b'='
        // pchar extras legal within a segment
        | b':' | b'@'
    )
}

/// The uppercase hex digit for a nibble in `0..=15` (matching the uppercase
/// `%XX` escapes Spring's `UriComponentsBuilder` produces).
fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_unreserved_untouched() {
        assert_eq!(encode_path_segment("abcXYZ0189-._~"), "abcXYZ0189-._~");
    }

    #[test]
    fn escapes_segment_and_component_delimiters() {
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
        assert_eq!(encode_path_segment("a?b"), "a%3Fb");
        assert_eq!(encode_path_segment("a#b"), "a%23b");
        assert_eq!(encode_path_segment("100%"), "100%25");
    }

    #[test]
    fn escapes_whitespace_and_non_ascii() {
        assert_eq!(encode_path_segment("a b"), "a%20b");
        // U+00E9 (é) is two UTF-8 bytes, both escaped.
        assert_eq!(encode_path_segment("\u{00e9}"), "%C3%A9");
    }

    #[test]
    fn keeps_segment_safe_punctuation() {
        // `:` and `@` are legal within a path segment and pass through.
        assert_eq!(encode_path_segment("a:b@c"), "a:b@c");
        assert_eq!(encode_path_segment("a,b;c=d"), "a,b;c=d");
    }
}
