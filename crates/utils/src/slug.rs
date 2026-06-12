//! URL-safe slug generation — the Rust port of Go's `utils.Slugify`,
//! itself a mirror of the Java `firefly-common-utils` SlugUtil and the
//! .NET `FireflyFramework.Utils` `Slug.Make` helpers.

use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

/// Converts `s` into a URL-safe lower-case slug: the input is
/// canonically decomposed (NFD), every non-spacing combining mark
/// (Unicode category `Mn`) is dropped — folding any canonically
/// decomposable letter to its base letter — runs of any other
/// non-alphanumeric character collapse to a single dash, and
/// leading/trailing dashes are trimmed.
///
/// This mirrors the Go port's transform chain
/// (`norm.NFD` → `runes.Remove(runes.In(unicode.Mn))` → `norm.NFC`)
/// exactly; the final NFC recomposition is omitted because canonical
/// composition can neither create nor consume ASCII alphanumerics, so
/// it cannot change the slug. As in Go, only *canonical* (NFD)
/// decompositions apply — compatibility-only characters (`½`, `Ⅻ`,
/// `ǅ`, …) and letters with no decomposition at all (`æ`, `ø`, `ß`,
/// `đ`, `ł`, …) become separators, and spacing/enclosing marks
/// (categories `Mc`/`Me`) are kept as separators rather than dropped.
///
/// ```
/// assert_eq!(firefly_utils::slugify("Cañón del Río"), "canon-del-rio");
/// assert_eq!(firefly_utils::slugify("Việt Nam"), "viet-nam");
/// ```
pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = true; // suppress leading dashes
    for c in s.nfd() {
        // Drop non-spacing combining marks (Mn), exactly like Go's
        // runes.Remove(runes.In(unicode.Mn)).
        if c.general_category() == GeneralCategory::NonspacingMark {
            continue;
        }
        match c {
            'A'..='Z' => {
                out.push(c.to_ascii_lowercase());
                prev_dash = false;
            }
            'a'..='z' | '0'..='9' => {
                out.push(c);
                prev_dash = false;
            }
            _ => {
                if !prev_dash {
                    out.push('-');
                    prev_dash = true;
                }
            }
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of Go `TestSlugify` — the exact table from utils_test.go.
    #[test]
    fn slugify_ports_go_table() {
        let cases = [
            ("Hello World", "hello-world"),
            ("Cañón del Río", "canon-del-rio"),
            ("   spaces   everywhere   ", "spaces-everywhere"),
            ("!!!@@@###", ""),
            ("Already-Slug", "already-slug"),
            ("números 42", "numeros-42"),
        ];
        for (input, want) in cases {
            assert_eq!(slugify(input), want, "slugify({input:?})");
        }
    }

    /// Rust-specific edge cases, each verified against the Go
    /// implementation's actual output.
    #[test]
    fn slugify_matches_go_edge_cases() {
        let cases = [
            ("", ""),
            ("Æther", "ther"),           // Æ has no canonical decomposition
            ("中文 page 1", "page-1"),   // non-Latin scripts become separators
            ("øre Straße", "re-stra-e"), // ø and ß have no decomposition
            ("--a--b--", "a-b"),
            ("İstanbul ışık", "istanbul-s-k"), // İ folds, dotless ı does not
            ("łódź", "odz"),                   // ł has no decomposition
        ];
        for (input, want) in cases {
            assert_eq!(slugify(input), want, "slugify({input:?})");
        }
    }

    /// Decomposed (NFD) input — base letters followed by combining
    /// marks — slugifies identically to precomposed input.
    #[test]
    fn slugify_handles_decomposed_input() {
        // "Cañón" written as C a n ̃ o ́ n.
        assert_eq!(slugify("Can\u{0303}o\u{0301}n"), "canon");
        assert_eq!(slugify("Can\u{0303}o\u{0301}n"), slugify("Cañón"));
    }

    /// Regression test: decomposable letters outside Latin-1
    /// Supplement / Latin Extended-A, and combining marks outside
    /// U+0300..=U+036F, must fold exactly like Go's NFD + Mn-removal
    /// chain. Expected values are the verified Go `Slugify` outputs.
    #[test]
    fn slugify_folds_all_canonical_decompositions_like_go() {
        let cases = [
            // Vietnamese — U+1EC7 (Latin Extended Additional).
            ("Việt Nam", "viet-nam"),
            // Pinyin — U+01CD / U+01DA (Latin Extended-B).
            ("\u{01CD}n pinyin n\u{01DA}", "an-pinyin-nu"),
            // Mn mark outside the U+0300..=U+036F block (U+1DC4,
            // Combining Diacritical Marks Supplement) is dropped.
            ("a\u{1DC4}b", "ab"),
            // Canonical singleton decomposition to ASCII: U+212A
            // KELVIN SIGN folds to 'K'.
            ("\u{212A}elvin", "kelvin"),
            // Canonical singleton to non-ASCII: U+2126 OHM SIGN
            // becomes Greek omega, i.e. a separator.
            ("\u{2126}hm", "hm"),
        ];
        for (input, want) in cases {
            assert_eq!(slugify(input), want, "slugify({input:?})");
        }
    }

    /// Go-parity guard: only category-Mn marks are removed and only
    /// *canonical* decompositions fold. Spacing (Mc) and enclosing
    /// (Me) marks survive as separators, and compatibility-only
    /// characters never fold. Expected values are the verified Go
    /// `Slugify` outputs.
    #[test]
    fn slugify_keeps_non_mn_marks_and_compat_chars_like_go() {
        let cases = [
            // U+0903 DEVANAGARI SIGN VISARGA — Mc, kept as separator.
            ("a\u{0903}b", "a-b"),
            // U+20DD COMBINING ENCLOSING CIRCLE — Me, kept as separator.
            ("a\u{20DD}b", "a-b"),
            // Compatibility-only decompositions are not applied.
            ("\u{2162} \u{00BD}", ""),  // Ⅲ ½
            ("\u{01C5}ungla", "ungla"), // ǅ digraph
        ];
        for (input, want) in cases {
            assert_eq!(slugify(input), want, "slugify({input:?})");
        }
    }
}
