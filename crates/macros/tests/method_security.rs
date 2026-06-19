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

//! End-to-end tests for the `#[pre_authorize]` / `#[post_authorize]`
//! method-security macros, driven through the ambient security context that a
//! real request would scope (here scoped explicitly with
//! `with_authentication_scope`).

use std::collections::HashMap;

use firefly::security::{with_authentication_scope, Authentication, SecurityError};

/// A service error that adopts the security tier's denial — the `From` impl is
/// what `?` (pre) and `From::from` (post) lower to in the generated code.
#[derive(Debug, PartialEq)]
enum SvcErr {
    Denied(SecurityError),
}

impl From<SecurityError> for SvcErr {
    fn from(e: SecurityError) -> Self {
        SvcErr::Denied(e)
    }
}

fn principal(name: &str, roles: &[&str], authorities: &[&str]) -> Authentication {
    Authentication {
        principal: name.into(),
        username: name.into(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        authorities: authorities.iter().map(|a| a.to_string()).collect(),
        claims: HashMap::new(),
    }
}

// --- pre_authorize: free async fns over each rule form --------------------

#[firefly::pre_authorize(role = "ADMIN")]
async fn admin_only() -> Result<&'static str, SvcErr> {
    Ok("closed")
}

#[firefly::pre_authorize(authenticated)]
async fn any_authenticated() -> Result<&'static str, SvcErr> {
    Ok("hello")
}

#[firefly::pre_authorize(any_role = ["AUDITOR", "ADMIN"])]
async fn audit() -> Result<&'static str, SvcErr> {
    Ok("report")
}

#[tokio::test]
async fn pre_authorize_role_admits_only_the_role() {
    // No ambient context → unauthenticated.
    assert_eq!(
        admin_only().await,
        Err(SvcErr::Denied(SecurityError::Unauthenticated))
    );
    // An ADMIN passes.
    let ok = with_authentication_scope(principal("root", &["ADMIN"], &[]), admin_only()).await;
    assert_eq!(ok, Ok("closed"));
    // A USER is forbidden.
    let denied = with_authentication_scope(principal("u", &["USER"], &[]), admin_only()).await;
    assert_eq!(denied, Err(SvcErr::Denied(SecurityError::Forbidden)));
}

#[tokio::test]
async fn pre_authorize_authenticated_rejects_anonymous() {
    let ok = with_authentication_scope(principal("u", &[], &[]), any_authenticated()).await;
    assert_eq!(ok, Ok("hello"));
    // Anonymous is present but not a real principal.
    let denied = with_authentication_scope(Authentication::anonymous(), any_authenticated()).await;
    assert_eq!(denied, Err(SvcErr::Denied(SecurityError::Forbidden)));
}

#[tokio::test]
async fn pre_authorize_any_role_matches_either() {
    let ok = with_authentication_scope(principal("a", &["AUDITOR"], &[]), audit()).await;
    assert_eq!(ok, Ok("report"));
    let denied = with_authentication_scope(principal("a", &["CLERK"], &[]), audit()).await;
    assert_eq!(denied, Err(SvcErr::Denied(SecurityError::Forbidden)));
}

// --- pre_authorize on an impl method (authority rule) ----------------------

struct Reports;

impl Reports {
    #[firefly::pre_authorize(authority = "reports:read")]
    async fn read(&self) -> Result<&'static str, SvcErr> {
        Ok("rows")
    }
}

#[tokio::test]
async fn pre_authorize_on_method_uses_authority() {
    let svc = Reports;
    let ok = with_authentication_scope(principal("svc", &[], &["reports:read"]), svc.read()).await;
    assert_eq!(ok, Ok("rows"));
    let denied =
        with_authentication_scope(principal("svc", &[], &["reports:write"]), svc.read()).await;
    assert_eq!(denied, Err(SvcErr::Denied(SecurityError::Forbidden)));
}

// --- pre_authorize: SpEL-style expression over arguments + principal -------

/// Spring's `@PreAuthorize("#id == authentication.name")` — the caller may act
/// only on their own id. A non-keyword argument is a boolean Rust expression
/// evaluated with the method's parameters and `auth` (`&Authentication`) bound.
#[firefly::pre_authorize(auth.principal == id)]
async fn read_account(id: &str) -> Result<String, SvcErr> {
    Ok(format!("account:{id}"))
}

/// Combines a role check with an ownership check over an argument.
#[firefly::pre_authorize(auth.has_role("ADMIN") || auth.principal == owner)]
async fn edit_doc(owner: &str) -> Result<&'static str, SvcErr> {
    Ok("edited")
}

#[tokio::test]
async fn pre_authorize_expression_binds_arguments_and_principal() {
    // alice reading her own account → allowed.
    let ok = with_authentication_scope(principal("alice", &[], &[]), read_account("alice")).await;
    assert_eq!(ok.unwrap(), "account:alice");
    // alice reading bob's account → forbidden.
    let denied = with_authentication_scope(principal("alice", &[], &[]), read_account("bob")).await;
    assert_eq!(denied, Err(SvcErr::Denied(SecurityError::Forbidden)));
    // No ambient context → unauthenticated (the principal binding fails closed).
    assert_eq!(
        read_account("alice").await,
        Err(SvcErr::Denied(SecurityError::Unauthenticated))
    );
}

#[tokio::test]
async fn pre_authorize_expression_combines_role_and_ownership() {
    // The owner may edit their own doc.
    let own = with_authentication_scope(principal("alice", &[], &[]), edit_doc("alice")).await;
    assert_eq!(own, Ok("edited"));
    // An ADMIN may edit anyone's doc.
    let admin =
        with_authentication_scope(principal("root", &["ADMIN"], &[]), edit_doc("bob")).await;
    assert_eq!(admin, Ok("edited"));
    // A non-owner non-admin is forbidden.
    let denied = with_authentication_scope(principal("eve", &[], &[]), edit_doc("bob")).await;
    assert_eq!(denied, Err(SvcErr::Denied(SecurityError::Forbidden)));
}

// --- pre_authorize: hasPermission via a PermissionEvaluator ----------------

struct Account {
    owner: String,
}

/// Grants `read` on an `Account` to its owner — Spring's `PermissionEvaluator`.
struct AccountPermissions;
impl firefly::security::PermissionEvaluator for AccountPermissions {
    fn has_permission(
        &self,
        auth: &Authentication,
        target: &dyn std::any::Any,
        permission: &str,
    ) -> bool {
        target
            .downcast_ref::<Account>()
            .is_some_and(|a| permission == "read" && a.owner == auth.principal)
    }
}

/// Spring's `@PreAuthorize("hasPermission(#account, 'read')")` — the expression
/// form calls the registered evaluator with the bound `auth` and an argument.
#[firefly::pre_authorize(firefly::security::has_permission(auth, account, "read"))]
async fn read_statement(account: &Account) -> Result<&'static str, SvcErr> {
    Ok("statement")
}

#[tokio::test]
async fn pre_authorize_has_permission_consults_the_evaluator() {
    // This is the only test in this binary that registers the evaluator.
    let _ = firefly::security::set_permission_evaluator(std::sync::Arc::new(AccountPermissions));

    let acct = Account {
        owner: "alice".into(),
    };
    // The owner may read.
    let ok = with_authentication_scope(principal("alice", &[], &[]), read_statement(&acct)).await;
    assert_eq!(ok, Ok("statement"));
    // A non-owner is forbidden.
    let denied = with_authentication_scope(principal("bob", &[], &[]), read_statement(&acct)).await;
    assert_eq!(denied, Err(SvcErr::Denied(SecurityError::Forbidden)));
}

// --- post_authorize: returnObject ownership check --------------------------

#[derive(Debug, Clone, PartialEq)]
struct Doc {
    owner: String,
    body: String,
}

/// Only return the document if the caller owns it — the Spring
/// `@PostAuthorize("returnObject.owner == authentication.name")` idiom.
#[firefly::post_authorize(result.owner == auth.principal)]
async fn load_doc(owner: &str) -> Result<Doc, SvcErr> {
    Ok(Doc {
        owner: owner.into(),
        body: "secret".into(),
    })
}

#[tokio::test]
async fn post_authorize_filters_on_return_value() {
    // Caller loads their own document → allowed.
    let ok = with_authentication_scope(principal("alice", &[], &[]), load_doc("alice")).await;
    assert_eq!(ok.unwrap().body, "secret");
    // Caller loads someone else's document → forbidden, value discarded.
    let denied = with_authentication_scope(principal("alice", &[], &[]), load_doc("bob")).await;
    assert_eq!(denied, Err(SvcErr::Denied(SecurityError::Forbidden)));
    // No ambient context → unauthenticated.
    assert_eq!(
        load_doc("alice").await,
        Err(SvcErr::Denied(SecurityError::Unauthenticated))
    );
}

// --- post_filter / pre_filter: collection filtering ------------------------

#[derive(Debug, Clone, PartialEq)]
struct Owned {
    owner: String,
}

fn owned(owner: &str) -> Owned {
    Owned {
        owner: owner.into(),
    }
}

/// Spring's `@PostFilter("filterObject.owner == authentication.name")` — keep
/// only the elements the caller owns.
#[firefly::post_filter(element.owner == auth.principal)]
async fn list_docs() -> Result<Vec<Owned>, SvcErr> {
    Ok(vec![owned("alice"), owned("bob"), owned("alice")])
}

#[tokio::test]
async fn post_filter_retains_only_owned_elements() {
    // alice sees only her own rows.
    let mine = with_authentication_scope(principal("alice", &[], &[]), list_docs())
        .await
        .unwrap();
    assert_eq!(mine, vec![owned("alice"), owned("alice")]);
    // bob sees only his (one).
    let bobs = with_authentication_scope(principal("bob", &[], &[]), list_docs())
        .await
        .unwrap();
    assert_eq!(bobs, vec![owned("bob")]);
    // No ambient context → unauthenticated (the whole call fails closed).
    assert_eq!(
        list_docs().await,
        Err(SvcErr::Denied(SecurityError::Unauthenticated))
    );
}

/// Spring's `@PreFilter` — drop the elements the caller does not own from the
/// `mut` argument before the body runs.
#[firefly::pre_filter(items, element.owner == auth.principal)]
async fn ingest(mut items: Vec<Owned>) -> Result<Vec<Owned>, SvcErr> {
    Ok(items)
}

#[tokio::test]
async fn pre_filter_drops_unowned_arguments_before_body() {
    let input = vec![owned("alice"), owned("bob"), owned("carol")];
    // The body only ever sees alice's elements.
    let kept = with_authentication_scope(principal("alice", &[], &[]), ingest(input.clone()))
        .await
        .unwrap();
    assert_eq!(kept, vec![owned("alice")]);
    // No ambient context → unauthenticated, body never runs.
    assert_eq!(
        ingest(input).await,
        Err(SvcErr::Denied(SecurityError::Unauthenticated))
    );
}
