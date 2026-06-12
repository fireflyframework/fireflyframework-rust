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

//! Port definitions — the Rust counterpart of the Go
//! `ruleengine/interfaces` package.
//!
//! [`Evaluator`] is the rule-engine port: anything that can judge a
//! [`RuleSet`] against a fact. [`Verdict`] is its result. The default
//! implementation lives in [`crate::core`]; the trait exists so the
//! [`crate::web`] layer (and application code) can be wired against an
//! abstraction.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::core::{EvalError, EvaluationMode};
use crate::models::{Action, RuleSet};

/// A fact is the JSON object rules are evaluated against — the Rust
/// counterpart of Go's `map[string]any`. Condition paths
/// (`user.address.country`) are resolved by descending nested objects.
pub type Fact = serde_json::Map<String, serde_json::Value>;

/// `Verdict` is the result of evaluating a [`RuleSet`] against a fact.
///
/// The Go port never serializes this type; the Rust port gives it the
/// lowercase `matched` / `actions` wire names used by the
/// [`crate::web`] REST surface and the [`crate::sdk`] client.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Verdict {
    /// Ids of rules whose [`crate::models::Logic`] evaluated true, in
    /// firing (descending-priority) order.
    #[serde(default)]
    pub matched: Vec<String>,
    /// Actions emitted, in priority order.
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// `Evaluator` is the rule-engine port.
///
/// Object-safe (`Arc<dyn Evaluator>`) so HTTP layers and tests can
/// inject alternative engines; the method is `async` per the framework
/// convention for ports, even though the in-tree
/// [`crate::core::AstEvaluator`] is CPU-bound and never awaits.
#[async_trait]
pub trait Evaluator: Send + Sync {
    /// Evaluates every rule of `set` against `fact` and returns the
    /// merged [`Verdict`] — equivalent to
    /// [`evaluate_with_mode`](Evaluator::evaluate_with_mode) under
    /// [`EvaluationMode::All`].
    async fn evaluate(&self, set: &RuleSet, fact: &Fact) -> Result<Verdict, EvalError>;

    /// Evaluates `set` against `fact` under the given
    /// [`EvaluationMode`].
    ///
    /// The default implementation ignores the mode and delegates to
    /// [`evaluate`](Evaluator::evaluate) (i.e. always [`EvaluationMode::All`]),
    /// so existing port implementations keep working unchanged; the
    /// in-tree [`crate::core::AstEvaluator`] overrides it to honour
    /// [`EvaluationMode::FirstMatch`].
    async fn evaluate_with_mode(
        &self,
        set: &RuleSet,
        fact: &Fact,
        mode: EvaluationMode,
    ) -> Result<Verdict, EvalError> {
        let _ = mode;
        self.evaluate(set, fact).await
    }
}
