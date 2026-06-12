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

//! The pure, transport-independent message-dispatch decision and the
//! `fnmatch`-style pattern matching, both ported directly from pyfly's
//! `RabbitMqEventBus._start_consumer.on_message`.

use firefly_eda::{Event, Handler};

/// The acknowledgement a consumer must apply to a delivery, decided by
/// [`dispatch`].
///
/// Maps one-to-one onto pyfly's `on_message` outcomes and onto the
/// at-least-once contract in the brief:
///
/// | Outcome                       | pyfly call                  | AMQP action            |
/// |-------------------------------|-----------------------------|------------------------|
/// | handled (or no match)         | `message.ack()`             | `basic_ack`            |
/// | a handler raised              | `message.reject(requeue=True)` | `basic_nack(requeue=true)` |
/// | body could not deserialize    | `message.reject(requeue=False)`| `basic_reject(requeue=false)` |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// Acknowledge the delivery — handled successfully, or no handler
    /// matched (a non-match is not a failure, so the message is
    /// consumed exactly as pyfly does).
    Ack,
    /// Negatively-acknowledge with requeue — a matching handler failed,
    /// so the broker redelivers for another attempt (at-least-once).
    NackRequeue,
    /// Reject without requeue — the body could not be deserialized, so
    /// the poison message is dropped instead of looping forever.
    RejectDrop,
}

/// Returns `true` when `event_type` matches the `fnmatch`-style
/// `pattern` — the Rust port of Python's `fnmatch.fnmatch`, supporting
/// `*` (any run), `?` (one char) and `[...]` character classes.
///
/// `*` here spans any character including `.`, exactly like
/// `fnmatch` (and unlike shell `globset` defaults), so `order.*`
/// matches `order.created.v2`.
///
/// ```
/// use firefly_eda_rabbitmq::pattern_matches;
/// assert!(pattern_matches("order.*", "order.created"));
/// assert!(pattern_matches("order.*", "order.created.v2"));
/// assert!(!pattern_matches("payment.*", "order.created"));
/// assert!(pattern_matches("*", "anything"));
/// ```
pub fn pattern_matches(pattern: &str, event_type: &str) -> bool {
    fnmatch(pattern.as_bytes(), event_type.as_bytes())
}

/// Recursive `fnmatch` over byte slices: `*` → any run, `?` → one byte,
/// `[...]` → a character class (with a leading `!` negating it).
fn fnmatch(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some(b'*') => {
            // Collapse runs of '*' then try to match the remainder at
            // every suffix of `text`.
            let rest = &pattern[1..];
            if rest.is_empty() {
                return true;
            }
            (0..=text.len()).any(|i| fnmatch(rest, &text[i..]))
        }
        Some(b'?') => !text.is_empty() && fnmatch(&pattern[1..], &text[1..]),
        Some(b'[') => match (class_match(&pattern[1..]), text.first()) {
            (Some((matched_set, consumed)), Some(&ch)) => {
                matched_set(ch) && fnmatch(&pattern[1 + consumed..], &text[1..])
            }
            // Unterminated class: treat '[' as a literal, like fnmatch.
            _ => !text.is_empty() && pattern[0] == text[0] && fnmatch(&pattern[1..], &text[1..]),
        },
        Some(&pc) => !text.is_empty() && pc == text[0] && fnmatch(&pattern[1..], &text[1..]),
    }
}

/// Parses a `[...]` character class starting just after the `[`,
/// returning a membership test and the number of bytes consumed
/// (excluding the opening `[` and including the closing `]`), or `None`
/// if the class is unterminated.
#[allow(clippy::type_complexity)]
fn class_match(after_bracket: &[u8]) -> Option<(Box<dyn Fn(u8) -> bool>, usize)> {
    let negate = after_bracket.first() == Some(&b'!');
    let body_start = usize::from(negate);
    let close = after_bracket[body_start..]
        .iter()
        .position(|&c| c == b']')?;
    let body = after_bracket[body_start..body_start + close].to_vec();
    // consumed = optional '!' + body + closing ']'.
    let consumed = body_start + close + 1;
    let test = move |ch: u8| {
        let mut hit = false;
        let mut i = 0;
        while i < body.len() {
            if i + 2 < body.len() && body[i + 1] == b'-' {
                if body[i] <= ch && ch <= body[i + 2] {
                    hit = true;
                }
                i += 3;
            } else {
                if body[i] == ch {
                    hit = true;
                }
                i += 1;
            }
        }
        hit != negate
    };
    Some((Box::new(test), consumed))
}

/// A registered subscription: an `fnmatch` pattern on `event_type` and
/// the [`Handler`] to invoke on a match — pyfly's `(pattern, handler)`
/// tuple.
#[derive(Clone)]
pub struct Subscription {
    /// The `fnmatch` pattern tested against each event's `event_type`.
    pub pattern: String,
    /// The delivery callback invoked when the pattern matches.
    pub handler: Handler,
}

/// Runs every matching handler over `event` and folds the results into
/// the [`Ack`] decision — the body of pyfly's `on_message` after a
/// successful deserialize.
///
/// Handlers run sequentially; a non-match contributes nothing and a
/// raised handler flips the outcome to [`Ack::NackRequeue`] while still
/// letting the remaining handlers run (parity with pyfly's loop, which
/// does not short-circuit on the first failure).
pub async fn dispatch(subscriptions: &[Subscription], event: &Event) -> Ack {
    let mut failed = false;
    for sub in subscriptions {
        if pattern_matches(&sub.pattern, &event.event_type)
            && (sub.handler)(event.clone()).await.is_err()
        {
            failed = true;
        }
    }
    if failed {
        Ack::NackRequeue
    } else {
        Ack::Ack
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_eda::handler;
    use firefly_kernel::FireflyError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn fnmatch_basic_patterns() {
        assert!(pattern_matches("order.*", "order.created"));
        assert!(pattern_matches("order.*", "order.created.v2"));
        assert!(!pattern_matches("payment.*", "order.created"));
        assert!(pattern_matches("*", "anything.at.all"));
        assert!(pattern_matches("order.created", "order.created"));
        assert!(!pattern_matches("order.created", "order.updated"));
    }

    #[test]
    fn fnmatch_question_and_class() {
        assert!(pattern_matches("order.?", "order.x"));
        assert!(!pattern_matches("order.?", "order.xy"));
        assert!(pattern_matches("v[0-9]", "v3"));
        assert!(!pattern_matches("v[0-9]", "vx"));
        assert!(pattern_matches("v[!0-9]", "vx"));
        assert!(!pattern_matches("v[!0-9]", "v3"));
    }

    fn ev(event_type: &str) -> Event {
        Event::new("orders", event_type, "test", None)
    }

    #[tokio::test]
    async fn dispatches_to_matching_subscriber() {
        let seen = Arc::new(AtomicUsize::new(0));
        let seen2 = seen.clone();
        let subs = vec![Subscription {
            pattern: "order.*".into(),
            handler: handler(move |_ev| {
                let seen2 = seen2.clone();
                async move {
                    seen2.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        }];
        assert_eq!(dispatch(&subs, &ev("order.created")).await, Ack::Ack);
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn non_matching_is_acked_not_dispatched() {
        let seen = Arc::new(AtomicUsize::new(0));
        let seen2 = seen.clone();
        let subs = vec![Subscription {
            pattern: "payment.*".into(),
            handler: handler(move |_ev| {
                let seen2 = seen2.clone();
                async move {
                    seen2.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        }];
        // No match is not a failure -> ack (pyfly parity).
        assert_eq!(dispatch(&subs, &ev("order.created")).await, Ack::Ack);
        assert_eq!(seen.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn handler_failure_yields_nack_requeue() {
        let subs = vec![Subscription {
            pattern: "order.*".into(),
            handler: handler(|_ev| async { Err(FireflyError::internal("boom")) }),
        }];
        assert_eq!(
            dispatch(&subs, &ev("order.created")).await,
            Ack::NackRequeue
        );
    }

    #[tokio::test]
    async fn all_matching_handlers_run_even_after_a_failure() {
        let seen = Arc::new(AtomicUsize::new(0));
        let seen2 = seen.clone();
        let subs = vec![
            Subscription {
                pattern: "order.*".into(),
                handler: handler(|_ev| async { Err(FireflyError::internal("boom")) }),
            },
            Subscription {
                pattern: "order.*".into(),
                handler: handler(move |_ev| {
                    let seen2 = seen2.clone();
                    async move {
                        seen2.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }),
            },
        ];
        assert_eq!(
            dispatch(&subs, &ev("order.created")).await,
            Ack::NackRequeue
        );
        // Second handler still ran despite the first failing.
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }
}
