//! URL-safe slug generation — the Rust port of Go's `utils.Slugify`,
//! itself a mirror of the Java `firefly-common-utils` SlugUtil and the
//! .NET `FireflyFramework.Utils` `Slug.Make` helpers.

/// Converts `s` into a URL-safe lower-case slug: accented Latin
/// letters are folded to their ASCII base letter, combining diacritical
/// marks are dropped, runs of any other character collapse to a single
/// dash, and leading/trailing dashes are trimmed.
///
/// The Go port reaches the same result via NFD normalisation plus
/// removal of Unicode combining marks (`Mn`); this port folds the
/// canonically-decomposable Latin-1 Supplement and Latin Extended-A
/// letters with an explicit table and strips the Combining Diacritical
/// Marks block (U+0300..=U+036F), which yields identical output for
/// both precomposed and decomposed input. Letters with no canonical
/// decomposition (`æ`, `ø`, `ß`, `đ`, `ł`, …) become separators in
/// both ports.
///
/// ```
/// assert_eq!(firefly_utils::slugify("Cañón del Río"), "canon-del-rio");
/// ```
pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = true; // suppress leading dashes
    for c in s.chars() {
        // Drop combining diacritical marks entirely (decomposed input).
        if ('\u{0300}'..='\u{036F}').contains(&c) {
            continue;
        }
        let c = fold_latin(c);
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

/// Folds a precomposed Latin letter with diacritics to its ASCII base
/// letter, covering every Latin-1 Supplement and Latin Extended-A
/// character with a *canonical* (NFD) decomposition. Characters
/// without one — `Æ`, `Ð`, `Ø`, `Þ`, `ß`, `đ`, `ħ`, `ı`, `ł`, `ŋ`,
/// `œ`, `ŧ`, … — are returned unchanged so they become separators,
/// exactly as in the Go port.
fn fold_latin(c: char) -> char {
    match c {
        'À'..='Å' => 'A',
        'Ç' => 'C',
        'È'..='Ë' => 'E',
        'Ì'..='Ï' => 'I',
        'Ñ' => 'N',
        'Ò'..='Ö' => 'O',
        'Ù'..='Ü' => 'U',
        'Ý' => 'Y',
        'à'..='å' => 'a',
        'ç' => 'c',
        'è'..='ë' => 'e',
        'ì'..='ï' => 'i',
        'ñ' => 'n',
        'ò'..='ö' => 'o',
        'ù'..='ü' => 'u',
        'ý' | 'ÿ' => 'y',
        'Ā' | 'Ă' | 'Ą' => 'A',
        'ā' | 'ă' | 'ą' => 'a',
        'Ć' | 'Ĉ' | 'Ċ' | 'Č' => 'C',
        'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'Ď' => 'D',
        'ď' => 'd',
        'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => 'E',
        'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => 'G',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'Ĥ' => 'H',
        'ĥ' => 'h',
        'Ĩ' | 'Ī' | 'Ĭ' | 'Į' | 'İ' => 'I',
        'ĩ' | 'ī' | 'ĭ' | 'į' => 'i',
        'Ĵ' => 'J',
        'ĵ' => 'j',
        'Ķ' => 'K',
        'ķ' => 'k',
        'Ĺ' | 'Ļ' | 'Ľ' => 'L',
        'ĺ' | 'ļ' | 'ľ' => 'l',
        'Ń' | 'Ņ' | 'Ň' => 'N',
        'ń' | 'ņ' | 'ň' => 'n',
        'Ō' | 'Ŏ' | 'Ő' => 'O',
        'ō' | 'ŏ' | 'ő' => 'o',
        'Ŕ' | 'Ŗ' | 'Ř' => 'R',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'Ś' | 'Ŝ' | 'Ş' | 'Š' => 'S',
        'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'Ţ' | 'Ť' => 'T',
        'ţ' | 'ť' => 't',
        'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' => 'U',
        'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
        'Ŵ' => 'W',
        'ŵ' => 'w',
        'Ŷ' => 'Y',
        'ŷ' => 'y',
        'Ÿ' => 'Y',
        'Ź' | 'Ż' | 'Ž' => 'Z',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
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
}
