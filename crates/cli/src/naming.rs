//! Name-case derivation for code generators.
//!
//! This is a hand-rolled port of pyfly's `naming.py`: it splits an identifier
//! of any style (kebab, snake, camel, Pascal, or space-separated) into its
//! constituent lowercase words, then re-assembles them into every case variant
//! the artifact templates need. No external crate (e.g. `heck`) is used so the
//! behaviour matches the Python reference exactly, including the naive English
//! pluralization rules.

use serde::Serialize;

/// Split an identifier of any style into lowercase words.
///
/// Non-alphanumeric runs are word boundaries, and a lowercase/digit → uppercase
/// transition (camelCase / PascalCase) is also a boundary.
fn words(raw: &str) -> Vec<String> {
    // First, replace every run of non-alphanumeric characters with a space.
    let mut spaced = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            spaced.push(ch);
        } else {
            spaced.push(' ');
        }
    }

    // Then insert a space at each camelCase boundary: a lowercase letter or
    // digit immediately followed by an uppercase letter.
    let chars: Vec<char> = spaced.chars().collect();
    let mut split = String::with_capacity(chars.len() + 8);
    for (i, &ch) in chars.iter().enumerate() {
        if i > 0 {
            let prev = chars[i - 1];
            if (prev.is_ascii_lowercase() || prev.is_ascii_digit()) && ch.is_ascii_uppercase() {
                split.push(' ');
            }
        }
        split.push(ch);
    }

    split
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Naive English pluralization good enough for identifiers.
fn pluralize(word: &str) -> String {
    // `...y` preceded by a consonant → `...ies`.
    if let Some(stem) = word.strip_suffix('y') {
        let before_y = word.chars().rev().nth(1);
        let is_vowel = matches!(before_y, Some('a' | 'e' | 'i' | 'o' | 'u'));
        if !is_vowel {
            return format!("{stem}ies");
        }
    }
    if word.ends_with('s')
        || word.ends_with('x')
        || word.ends_with('z')
        || word.ends_with("ch")
        || word.ends_with("sh")
    {
        return format!("{word}es");
    }
    format!("{word}s")
}

/// Capitalize the first character of an ASCII word, lowercasing the rest.
fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// All case variants of an identifier the generators need.
///
/// Mirrors pyfly's `Names` dataclass field-for-field so the same template
/// context keys (`names.pascal`, `names.snake_plural`, …) resolve identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Names {
    /// The original, unmodified user-supplied name.
    pub raw: String,
    /// `PascalCase` form (e.g. `UserAccount`).
    pub pascal: String,
    /// `snake_case` form (e.g. `user_account`).
    pub snake: String,
    /// `kebab-case` form (e.g. `user-account`).
    pub kebab: String,
    /// `camelCase` form (e.g. `userAccount`).
    pub camel: String,
    /// Pluralized `snake_case` (e.g. `user_accounts`).
    pub snake_plural: String,
    /// Pluralized `kebab-case` (e.g. `user-accounts`).
    pub kebab_plural: String,
    /// Pluralized `PascalCase` (e.g. `UserAccounts`).
    pub pascal_plural: String,
    /// Space-separated lowercase words (e.g. `user account`).
    pub human: String,
    /// Pluralized space-separated words (e.g. `user accounts`).
    pub human_plural: String,
    /// `SCREAMING_SNAKE_CASE` form (e.g. `USER_ACCOUNT`). Rust-port addition for
    /// constant generation; not present in pyfly but harmless to templates that
    /// ignore it.
    pub screaming: String,
}

/// Derive every case variant from a single user-supplied name.
///
/// Returns `None` when `raw` contains no alphanumeric characters (pyfly raises
/// `ValueError` in that case; the Rust port surfaces the failure as an
/// [`Option`] so callers can render a friendly diagnostic).
///
/// # Examples
/// ```
/// let n = firefly_cli::naming::names("user-account").unwrap();
/// assert_eq!(n.pascal, "UserAccount");
/// assert_eq!(n.snake, "user_account");
/// assert_eq!(n.camel, "userAccount");
/// ```
pub fn names(raw: &str) -> Option<Names> {
    let words = words(raw);
    if words.is_empty() {
        return None;
    }
    let snake = words.join("_");

    let mut plural_words = words.clone();
    let last = plural_words.len() - 1;
    plural_words[last] = pluralize(&plural_words[last]);
    let snake_plural = plural_words.join("_");

    let pascal: String = words.iter().map(|w| capitalize(w)).collect();
    let camel = {
        let mut it = words.iter();
        let head = it.next().cloned().unwrap_or_default();
        let tail: String = it.map(|w| capitalize(w)).collect();
        head + &tail
    };

    Some(Names {
        raw: raw.to_string(),
        pascal,
        snake: snake.clone(),
        kebab: words.join("-"),
        camel,
        snake_plural: snake_plural.clone(),
        kebab_plural: plural_words.join("-"),
        pascal_plural: plural_words.iter().map(|w| capitalize(w)).collect(),
        human: words.join(" "),
        human_plural: plural_words.join(" "),
        screaming: snake.to_uppercase(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Ported directly from pyfly tests/cli/test_naming.py ──

    #[test]
    fn pascal_from_kebab() {
        let n = names("user-account").unwrap();
        assert_eq!(n.pascal, "UserAccount");
        assert_eq!(n.snake, "user_account");
        assert_eq!(n.kebab, "user-account");
        assert_eq!(n.camel, "userAccount");
    }

    #[test]
    fn from_pascal_input() {
        let n = names("UserAccount").unwrap();
        assert_eq!(n.snake, "user_account");
        assert_eq!(n.kebab, "user-account");
    }

    #[test]
    fn from_snake_input() {
        let n = names("order_item").unwrap();
        assert_eq!(n.pascal, "OrderItem");
    }

    #[test]
    fn plurals() {
        assert_eq!(names("category").unwrap().snake_plural, "categories");
        assert_eq!(names("box").unwrap().snake_plural, "boxes");
        assert_eq!(names("user").unwrap().snake_plural, "users");
        assert_eq!(names("Category").unwrap().kebab_plural, "categories");
        assert_eq!(names("User").unwrap().pascal_plural, "Users");
    }

    #[test]
    fn human() {
        assert_eq!(names("user-account").unwrap().human, "user account");
    }

    // ── Additional edge cases for the Rust port ──

    #[test]
    fn empty_or_punctuation_only_returns_none() {
        assert!(names("").is_none());
        assert!(names("---").is_none());
        assert!(names("  __  ").is_none());
    }

    #[test]
    fn screaming_snake() {
        assert_eq!(names("user-account").unwrap().screaming, "USER_ACCOUNT");
    }

    #[test]
    fn camel_boundary_with_digits() {
        let n = names("oauth2Client").unwrap();
        assert_eq!(n.snake, "oauth2_client");
        assert_eq!(n.pascal, "Oauth2Client");
    }

    #[test]
    fn vowel_before_y_keeps_s() {
        assert_eq!(names("day").unwrap().snake_plural, "days");
        assert_eq!(names("key").unwrap().snake_plural, "keys");
    }
}
