//! A minimal port of Go's `mime.ParseMediaType`, used by
//! [`TwilioValidator`](crate::TwilioValidator) to reproduce
//! `net/http.Request.ParseForm`'s Content-Type dispatch exactly.
//!
//! Only what `ParseForm` observes is exposed: the lowercased media
//! type, and whether parsing errored. Parameters are still *validated*
//! (so a malformed parameter or a duplicate name with a conflicting
//! value errors, exactly as in Go), but their values are not returned.

use std::collections::HashMap;

/// Go's `mime.ParseMediaType`, reduced to the `(mediatype, err)` pair
/// `Request.ParseForm` consumes: returns the trimmed, lowercased
/// `type/subtype` on success, and `None` on any input `ParseMediaType`
/// rejects — a malformed media type (`checkMediaTypeDisposition`), a
/// malformed parameter (`ErrInvalidMediaParameter`), or a duplicate
/// parameter name with a different value.
pub(crate) fn parse_media_type(v: &str) -> Option<String> {
    let base = v.split(';').next().unwrap_or(v);
    let mediatype = base.trim().to_ascii_lowercase();
    check_media_type_disposition(&mediatype)?;

    // Duplicate detection mirrors Go's bucket layout: parameters whose
    // name contains `*` (RFC 2231 continuations) are grouped by the
    // part before the first `*`; plain parameters share one map.
    let mut params: HashMap<String, String> = HashMap::new();
    let mut continuation: HashMap<String, HashMap<String, String>> = HashMap::new();

    let mut rest = &v[base.len()..];
    while !rest.is_empty() {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let Some((key, value, next)) = consume_media_param(rest) else {
            if rest.trim() == ";" {
                // Ignore trailing semicolons, as Go does.
                break;
            }
            return None; // Go: ErrInvalidMediaParameter.
        };
        let pmap = match key.split_once('*') {
            Some((base_name, _)) => continuation.entry(base_name.to_owned()).or_default(),
            None => &mut params,
        };
        if let Some(existing) = pmap.get(&key) {
            if existing != &value {
                return None; // Go: "mime: duplicate parameter name".
            }
        }
        pmap.insert(key, value);
        rest = next;
    }
    Some(mediatype)
}

/// Go's `checkMediaTypeDisposition`: a token, optionally followed by
/// `/` and a subtype token, with nothing trailing. A bare token with
/// no subtype is valid.
fn check_media_type_disposition(s: &str) -> Option<()> {
    let (typ, rest) = consume_token(s);
    if typ.is_empty() {
        return None; // "mime: no media type"
    }
    if rest.is_empty() {
        return Some(());
    }
    let rest = rest.strip_prefix('/')?; // "mime: expected slash after first token"
    let (subtype, rest) = consume_token(rest);
    if subtype.is_empty() {
        return None; // "mime: expected token after slash"
    }
    if !rest.is_empty() {
        return None; // "mime: unexpected content after media subtype"
    }
    Some(())
}

/// Go's `consumeMediaParam`: `;` `key` `=` `value`, with optional
/// whitespace around every piece. Returns the lowercased key, the
/// (possibly unquoted) value, and the remainder; `None` on a parse
/// failure (Go signals that by returning an empty key).
fn consume_media_param(v: &str) -> Option<(String, String, &str)> {
    let rest = v.trim_start();
    let rest = rest.strip_prefix(';')?;
    let rest = rest.trim_start();
    let (param, rest) = consume_token(rest);
    if param.is_empty() {
        return None;
    }
    // Tokens are ASCII-only, so ASCII lowercase == Go's ToLower.
    let param = param.to_ascii_lowercase();
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    let rest = rest.trim_start();
    let (value, rest2) = consume_value(rest);
    if value.is_empty() && rest2 == rest {
        return None;
    }
    Some((param, value, rest2))
}

/// Go's `consumeToken`: the longest leading run of RFC 1521 token
/// characters, plus the remainder.
fn consume_token(v: &str) -> (&str, &str) {
    match v.find(|c: char| !is_token_char(c)) {
        None => (v, ""),
        Some(pos) => (&v[..pos], &v[pos..]),
    }
}

/// Go's `consumeValue`: a bare token, or a quoted-string where `\`
/// escapes only tspecials (the MSIE leniency) and a bare CR/LF or a
/// missing closing quote is a failure (empty value, `rest == v`).
fn consume_value(v: &str) -> (String, &str) {
    if v.is_empty() {
        return (String::new(), v);
    }
    if !v.starts_with('"') {
        let (token, rest) = consume_token(v);
        return (token.to_owned(), rest);
    }
    let bytes = v.as_bytes();
    let mut buf: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                // Only whole UTF-8 sequences were copied, so this
                // cannot fail; the fallback is purely defensive.
                let value = String::from_utf8(buf).unwrap_or_default();
                return (value, &v[i + 1..]);
            }
            b'\\' if i + 1 < bytes.len() && is_tspecial(char::from(bytes[i + 1])) => {
                buf.push(bytes[i + 1]);
                i += 2;
            }
            b'\r' | b'\n' => return (String::new(), v),
            b => {
                buf.push(b);
                i += 1;
            }
        }
    }
    // Did not find an end quote.
    (String::new(), v)
}

/// Go's `isTokenChar`: any US-ASCII CHAR except SPACE, CTLs, or
/// tspecials.
fn is_token_char(c: char) -> bool {
    c > '\x20' && c < '\x7f' && !is_tspecial(c)
}

/// Go's `isTSpecial`: the RFC 1521 tspecials set.
fn is_tspecial(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '<' | '>' | '@' | ',' | ';' | ':' | '\\' | '"' | '/' | '[' | ']' | '?' | '='
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_lowercases_the_media_type() {
        let cases = [
            ("application/json", "application/json"),
            ("Application/JSON", "application/json"),
            (" text/plain ", "text/plain"),
            // A bare token with no subtype is valid in Go.
            ("text", "text"),
            // Parameters are validated then discarded.
            (
                "application/x-www-form-urlencoded; charset=UTF-8",
                "application/x-www-form-urlencoded",
            ),
            // Quoted values, including escaped tspecials.
            (r#"form-data; name="fo\"o""#, "form-data"),
            // Equal duplicates are tolerated, as in Go.
            ("text/plain; a=1; a=1", "text/plain"),
            ("text/plain; t*0=a; t*0=a", "text/plain"),
            // Trailing semicolons are ignored.
            ("text/plain;", "text/plain"),
            ("text/plain ; ", "text/plain"),
        ];
        for (input, want) in cases {
            assert_eq!(parse_media_type(input).as_deref(), Some(want), "{input:?}");
        }
    }

    #[test]
    fn rejects_what_go_mime_parse_media_type_rejects() {
        let cases = [
            "",                       // mime: no media type
            "/json",                  // mime: no media type
            "application/",           // mime: expected token after slash
            "a/b/c",                  // mime: unexpected content after media subtype
            "text plain",             // mime: expected slash after first token
            "text/plain; charset",    // ErrInvalidMediaParameter (no '=')
            "text/plain; charset=",   // ErrInvalidMediaParameter (empty value)
            "text/plain; =utf-8",     // ErrInvalidMediaParameter (empty key)
            "text/plain; a=\"open",   // unterminated quoted-string
            "text/plain; a=\"x\ny\"", // bare LF in quoted-string
            "text/plain; a=1; a=2",   // mime: duplicate parameter name
            "text/plain; t*0=a; t*0=b",
        ];
        for input in cases {
            assert_eq!(parse_media_type(input), None, "{input:?}");
        }
    }
}
