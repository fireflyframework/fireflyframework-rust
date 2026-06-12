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

//! Safe boolean-expression evaluation for conditional workflow steps —
//! [`Node::when`](crate::Node::when).
//!
//! The Rust spelling of pyfly's `condition.py` SpEL substitute
//! (`pyfly.transactional.workflow.condition`). pyfly parses a restricted
//! Python expression against a namespace of workflow facts (`results`,
//! `variables`, `headers`, `input`) using a whitelisted AST so a condition
//! can never run arbitrary code. The Rust port evaluates the same fact-based
//! comparison grammar against a [`StepContext`].
//!
//! Supported grammar (whitespace-insensitive):
//!
//! * **Operands** — `results['step']`, `variables['key']`, `headers['h']`,
//!   `input['field']` (each looking the value up in the context), plus JSON
//!   literals: numbers, quoted strings (`'x'` / `"x"`), `true`/`false`/`null`.
//!   A field access `results['step'].field` reads a sub-field of a JSON
//!   object.
//! * **Comparisons** — `==`, `!=`, `<`, `<=`, `>`, `>=`.
//! * **Membership** — `in`, `not in` (right side a JSON array/object/string).
//! * **Boolean** — `and`, `or`, `not`, with parentheses for grouping.
//! * A bare operand is truthy by JSON truthiness (non-null, non-false,
//!   non-zero, non-empty).
//!
//! Anything the parser does not recognise raises [`ConditionError`]; the
//! workflow executor treats that as "skip" (fail-closed), mirroring pyfly.

use serde_json::Value;

use crate::step_context::StepContext;

/// Error raised when a condition expression is malformed or uses an
/// unsupported construct — pyfly's `ConditionError`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid condition: {0}")]
pub struct ConditionError(pub String);

/// Evaluates `expression` against the facts in `ctx`, returning the boolean
/// result. Mirrors pyfly's `evaluate_condition`.
pub(crate) fn evaluate(expression: &str, ctx: &StepContext) -> Result<bool, ConditionError> {
    let tokens = tokenize(expression)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        ctx,
    };
    let value = parser.parse_or()?;
    if parser.pos != parser.tokens.len() {
        return Err(ConditionError(format!(
            "unexpected trailing tokens in {expression:?}"
        )));
    }
    Ok(truthy(&value))
}

/// JSON truthiness, matching Python's `bool(...)` over the value types a
/// condition can produce.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

// -- Lexer --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Dot,
    Op(String), // == != < <= > >=
    And,
    Or,
    Not,
    In,
}

fn tokenize(expr: &str) -> Result<Vec<Tok>, ConditionError> {
    let chars: Vec<char> = expr.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            ']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '.' => {
                out.push(Tok::Dot);
                i += 1;
            }
            '\'' | '"' => {
                let quote = c;
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != quote {
                    s.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(ConditionError("unterminated string".into()));
                }
                i += 1; // closing quote
                out.push(Tok::Str(s));
            }
            '=' | '!' | '<' | '>' => {
                let mut op = String::new();
                op.push(c);
                i += 1;
                if i < chars.len() && chars[i] == '=' {
                    op.push('=');
                    i += 1;
                }
                if op == "=" || op == "!" {
                    return Err(ConditionError(format!("invalid operator {op:?}")));
                }
                out.push(Tok::Op(op));
            }
            d if d.is_ascii_digit() || (d == '-' && peek_digit(&chars, i + 1)) => {
                let start = i;
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                let n = text
                    .parse::<f64>()
                    .map_err(|_| ConditionError(format!("invalid number {text:?}")))?;
                out.push(Tok::Num(n));
            }
            a if a.is_alphabetic() || a == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                out.push(match word.as_str() {
                    "and" => Tok::And,
                    "or" => Tok::Or,
                    "not" => Tok::Not,
                    "in" => Tok::In,
                    "true" | "True" => Tok::Bool(true),
                    "false" | "False" => Tok::Bool(false),
                    "null" | "None" => Tok::Null,
                    _ => Tok::Ident(word),
                });
            }
            other => return Err(ConditionError(format!("unexpected character {other:?}"))),
        }
    }
    Ok(out)
}

fn peek_digit(chars: &[char], idx: usize) -> bool {
    chars.get(idx).is_some_and(|c| c.is_ascii_digit())
}

// -- Parser / evaluator -------------------------------------------------------

struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
    ctx: &'a StepContext,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<&Tok> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// `or`-expression: lowest precedence.
    fn parse_or(&mut self) -> Result<Value, ConditionError> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.bump();
            let right = self.parse_and()?;
            left = Value::Bool(truthy(&left) || truthy(&right));
        }
        Ok(left)
    }

    /// `and`-expression.
    fn parse_and(&mut self) -> Result<Value, ConditionError> {
        let mut left = self.parse_not()?;
        while matches!(self.peek(), Some(Tok::And)) {
            self.bump();
            let right = self.parse_not()?;
            left = Value::Bool(truthy(&left) && truthy(&right));
        }
        Ok(left)
    }

    /// `not`-expression.
    fn parse_not(&mut self) -> Result<Value, ConditionError> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.bump();
            let operand = self.parse_not()?;
            return Ok(Value::Bool(!truthy(&operand)));
        }
        self.parse_comparison()
    }

    /// Comparison / membership.
    fn parse_comparison(&mut self) -> Result<Value, ConditionError> {
        let left = self.parse_operand()?;
        match self.peek() {
            Some(Tok::Op(op)) => {
                let op = op.clone();
                self.bump();
                let right = self.parse_operand()?;
                Ok(Value::Bool(compare(&left, &op, &right)?))
            }
            Some(Tok::In) => {
                self.bump();
                let right = self.parse_operand()?;
                Ok(Value::Bool(contains(&right, &left)))
            }
            Some(Tok::Not) => {
                // `not in`
                self.bump();
                if !matches!(self.peek(), Some(Tok::In)) {
                    return Err(ConditionError("expected 'in' after 'not'".into()));
                }
                self.bump();
                let right = self.parse_operand()?;
                Ok(Value::Bool(!contains(&right, &left)))
            }
            _ => Ok(left),
        }
    }

    /// A single operand: a grouped expression, a literal, or a context
    /// lookup possibly followed by `.field` accessors.
    fn parse_operand(&mut self) -> Result<Value, ConditionError> {
        let base = match self.bump().cloned() {
            Some(Tok::LParen) => {
                let inner = self.parse_or()?;
                if !matches!(self.bump(), Some(Tok::RParen)) {
                    return Err(ConditionError("expected ')'".into()));
                }
                inner
            }
            Some(Tok::Str(s)) => Value::String(s),
            Some(Tok::Num(n)) => serde_json::json!(n),
            Some(Tok::Bool(b)) => Value::Bool(b),
            Some(Tok::Null) => Value::Null,
            Some(Tok::Ident(name)) => self.parse_lookup(&name)?,
            other => return Err(ConditionError(format!("unexpected token {other:?}"))),
        };
        self.parse_field_accessors(base)
    }

    /// `results['x']` / `variables['x']` / `headers['x']` / `input['x']`.
    fn parse_lookup(&mut self, name: &str) -> Result<Value, ConditionError> {
        // A bracket subscript indexes the named container.
        if matches!(self.peek(), Some(Tok::LBracket)) {
            self.bump();
            let key = match self.bump().cloned() {
                Some(Tok::Str(s)) => s,
                other => {
                    return Err(ConditionError(format!(
                        "expected string key, got {other:?}"
                    )))
                }
            };
            if !matches!(self.bump(), Some(Tok::RBracket)) {
                return Err(ConditionError("expected ']'".into()));
            }
            return Ok(match name {
                "results" => self.ctx.result(&key).unwrap_or(Value::Null),
                "variables" => self.ctx.variable(&key).unwrap_or(Value::Null),
                "headers" => self
                    .ctx
                    .header(&key)
                    .map(Value::String)
                    .unwrap_or(Value::Null),
                "input" => self.ctx.input_field(&key).unwrap_or(Value::Null),
                other => return Err(ConditionError(format!("unknown namespace {other:?}"))),
            });
        }
        // A bare `input` resolves to the whole input value.
        match name {
            "input" => Ok(self.ctx.input()),
            other => Err(ConditionError(format!("unknown name {other:?}"))),
        }
    }

    /// Zero or more `.field` accessors on a JSON object value.
    fn parse_field_accessors(&mut self, mut value: Value) -> Result<Value, ConditionError> {
        while matches!(self.peek(), Some(Tok::Dot)) {
            self.bump();
            let field = match self.bump().cloned() {
                Some(Tok::Ident(f)) => f,
                other => {
                    return Err(ConditionError(format!(
                        "expected field name, got {other:?}"
                    )))
                }
            };
            value = value
                .as_object()
                .and_then(|m| m.get(&field).cloned())
                .unwrap_or(Value::Null);
        }
        Ok(value)
    }
}

/// Applies a comparison operator to two JSON values.
fn compare(left: &Value, op: &str, right: &Value) -> Result<bool, ConditionError> {
    if op == "==" {
        return Ok(json_eq(left, right));
    }
    if op == "!=" {
        return Ok(!json_eq(left, right));
    }
    // Ordering comparisons: numeric when both are numbers, else lexical for
    // strings.
    let ordering = match (left, right) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .zip(b.as_f64())
            .and_then(|(a, b)| a.partial_cmp(&b)),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        _ => None,
    };
    let Some(ord) = ordering else {
        // Incomparable operands compare false (Python would raise; we
        // fail-closed to a non-match).
        return Ok(false);
    };
    Ok(match op {
        "<" => ord.is_lt(),
        "<=" => ord.is_le(),
        ">" => ord.is_gt(),
        ">=" => ord.is_ge(),
        other => return Err(ConditionError(format!("unsupported operator {other:?}"))),
    })
}

/// JSON equality treating integer/float forms of the same value as equal.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

/// `needle in haystack` over arrays (element membership), objects (key
/// membership) and strings (substring).
fn contains(haystack: &Value, needle: &Value) -> bool {
    match haystack {
        Value::Array(items) => items.iter().any(|item| json_eq(item, needle)),
        Value::Object(map) => needle.as_str().is_some_and(|k| map.contains_key(k)),
        Value::String(s) => needle.as_str().is_some_and(|n| s.contains(n)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> StepContext {
        let c = StepContext::with_input(json!({"amount": 100, "tier": "gold"}));
        c.set_result("always", json!("ran"));
        c.set_result("score", json!({"value": 42}));
        c.set_variable("approved", json!(true));
        c.set_header("x-tenant", "acme");
        c
    }

    // Port of pyfly test_wave_workflow_fixes.py condition: a false condition
    // means the step is skipped.
    #[test]
    fn equality_condition_false() {
        assert!(!evaluate("results['always'] == 'nope'", &ctx()).unwrap());
        assert!(evaluate("results['always'] == 'ran'", &ctx()).unwrap());
    }

    #[test]
    fn numeric_comparisons() {
        assert!(evaluate("input['amount'] > 50", &ctx()).unwrap());
        assert!(!evaluate("input['amount'] < 50", &ctx()).unwrap());
        assert!(evaluate("input['amount'] >= 100", &ctx()).unwrap());
        assert!(evaluate("input['amount'] != 99", &ctx()).unwrap());
    }

    #[test]
    fn boolean_and_or_not() {
        assert!(evaluate("variables['approved'] and input['amount'] > 50", &ctx()).unwrap());
        assert!(evaluate("input['amount'] > 200 or input['tier'] == 'gold'", &ctx()).unwrap());
        assert!(evaluate("not (input['amount'] < 50)", &ctx()).unwrap());
    }

    #[test]
    fn membership() {
        let c = ctx();
        c.set_result("tags", json!(["a", "b", "c"]));
        assert!(evaluate("'b' in results['tags']", &c).unwrap());
        assert!(evaluate("'z' not in results['tags']", &c).unwrap());
    }

    #[test]
    fn field_accessor() {
        assert!(evaluate("results['score'].value == 42", &ctx()).unwrap());
    }

    #[test]
    fn bare_truthiness() {
        assert!(evaluate("variables['approved']", &ctx()).unwrap());
        // A missing variable is null -> falsey.
        assert!(!evaluate("variables['missing']", &ctx()).unwrap());
    }

    #[test]
    fn header_lookup() {
        assert!(evaluate("headers['x-tenant'] == 'acme'", &ctx()).unwrap());
    }

    // Malformed expressions raise ConditionError (executor fails closed).
    #[test]
    fn malformed_is_error() {
        assert!(evaluate("results[", &ctx()).is_err());
        assert!(evaluate("results['x'] === 1", &ctx()).is_err());
        assert!(evaluate("$$$", &ctx()).is_err());
    }
}
