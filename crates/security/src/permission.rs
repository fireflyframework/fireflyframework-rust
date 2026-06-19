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

//! Domain-object permissions — the Rust analog of Spring Security's
//! `PermissionEvaluator` and the SpEL `hasPermission(target, permission)`.
//!
//! A [`PermissionEvaluator`] answers "may this principal perform `permission` on
//! this domain object?". Register one process-wide with
//! [`set_permission_evaluator`]; method-security expressions then call
//! [`has_permission`] — usable directly inside `#[pre_authorize]` /
//! `#[post_authorize]` since they bind `auth`:
//!
//! ```rust,ignore
//! #[pre_authorize(firefly_security::has_permission(auth, account, "read"))]
//! async fn read(&self, account: &Account) -> Result<Statement, Error> { /* … */ }
//! ```
//!
//! The default — **no evaluator registered** — denies every permission
//! (fail-closed), so wiring an evaluator is a deliberate opt-in. The target is
//! erased to [`std::any::Any`] so a single registered evaluator can serve every
//! domain type by downcasting, mirroring Spring's reflective contract while
//! staying type-safe at the call site.

use std::any::Any;
use std::sync::{Arc, OnceLock};

use crate::authentication::Authentication;

/// Decides whether a principal holds a `permission` on a domain object — the
/// Rust analog of Spring's `PermissionEvaluator`.
///
/// The `target` is type-erased; an implementation downcasts it (via
/// [`Any::downcast_ref`]) to the domain types it understands and denies
/// (returns `false`) for any type or permission it does not recognise.
pub trait PermissionEvaluator: Send + Sync {
    /// Whether `auth` may perform `permission` on `target`.
    fn has_permission(&self, auth: &Authentication, target: &dyn Any, permission: &str) -> bool;
}

/// The process-wide evaluator, set once at startup (Spring's single
/// `PermissionEvaluator` bean).
static EVALUATOR: OnceLock<Arc<dyn PermissionEvaluator>> = OnceLock::new();

/// Registers the process-wide [`PermissionEvaluator`]. Returns `Err` (handing
/// the rejected evaluator back) if one was already set — it is a set-once
/// startup hook, not a runtime switch.
///
/// # Errors
///
/// Returns the passed-in `evaluator` unchanged if an evaluator is already
/// registered.
pub fn set_permission_evaluator(
    evaluator: Arc<dyn PermissionEvaluator>,
) -> Result<(), Arc<dyn PermissionEvaluator>> {
    EVALUATOR.set(evaluator)
}

/// Whether `auth` may perform `permission` on `target`, per the registered
/// [`PermissionEvaluator`] — Spring's `hasPermission(target, permission)`.
///
/// Returns `false` (deny) when no evaluator is registered, so an unconfigured
/// application fails closed. The `target` is taken by reference and erased to
/// [`Any`] for the evaluator to downcast.
#[must_use]
pub fn has_permission<T: Any>(auth: &Authentication, target: &T, permission: &str) -> bool {
    EVALUATOR
        .get()
        .is_some_and(|e| e.has_permission(auth, target, permission))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Account {
        owner: String,
    }

    /// Grants `read`/`write` on an `Account` to its owner; denies everything
    /// else (unknown target type, unknown permission, non-owner).
    struct OwnerPermissionEvaluator;
    impl PermissionEvaluator for OwnerPermissionEvaluator {
        fn has_permission(
            &self,
            auth: &Authentication,
            target: &dyn Any,
            permission: &str,
        ) -> bool {
            match target.downcast_ref::<Account>() {
                Some(account) => {
                    matches!(permission, "read" | "write") && account.owner == auth.principal
                }
                None => false,
            }
        }
    }

    fn principal(name: &str) -> Authentication {
        Authentication {
            principal: name.into(),
            username: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn evaluator_grants_owner_and_denies_others() {
        let eval = OwnerPermissionEvaluator;
        let alice = principal("alice");
        let acct = Account {
            owner: "alice".into(),
        };

        // Owner + known permission → granted.
        assert!(eval.has_permission(&alice, &acct, "read"));
        assert!(eval.has_permission(&alice, &acct, "write"));
        // Non-owner → denied.
        assert!(!eval.has_permission(&principal("bob"), &acct, "read"));
        // Unknown permission → denied even for the owner.
        assert!(!eval.has_permission(&alice, &acct, "delete"));
        // Unknown target type → denied.
        assert!(!eval.has_permission(&alice, &"some string", "read"));
    }

    // The global registry is set-once per process; this is the only test that
    // touches it, so ordering/parallelism cannot make it flaky.
    #[test]
    fn global_registry_defaults_to_deny_then_delegates() {
        let alice = principal("alice");
        let acct = Account {
            owner: "alice".into(),
        };

        // No evaluator registered yet → fail closed.
        assert!(!has_permission(&alice, &acct, "read"));

        // Register, then the same call delegates and grants.
        assert!(set_permission_evaluator(Arc::new(OwnerPermissionEvaluator)).is_ok());
        assert!(has_permission(&alice, &acct, "read"));
        assert!(!has_permission(&principal("bob"), &acct, "read"));

        // A second set is rejected (set-once), handing the evaluator back.
        assert!(set_permission_evaluator(Arc::new(OwnerPermissionEvaluator)).is_err());
    }
}
