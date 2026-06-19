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

//! ACL / domain-object security — the Rust analog of Spring Security's
//! `spring-security-acl`.
//!
//! Where the Tier 3 [`PermissionEvaluator`](crate::PermissionEvaluator) answers
//! "may this principal do X to this object?" with arbitrary logic, an **ACL**
//! answers it from per-object access-control lists: each domain object
//! ([`ObjectIdentity`]) has an [`Acl`] of [`AccessControlEntry`]s granting (or
//! denying) [`Permission`]s to [`Sid`]s (a principal or an authority).
//!
//! The pieces mirror Spring:
//!
//! - [`Permission`] — the `BasePermission` bitmask (`READ`, `WRITE`, `CREATE`,
//!   `DELETE`, `ADMINISTRATION`), combinable into a cumulative mask.
//! - [`Sid`] — a security identity: a [`Sid::Principal`] (username) or a
//!   [`Sid::Authority`] (a granted authority / role), Spring's `PrincipalSid` /
//!   `GrantedAuthoritySid`.
//! - [`ObjectIdentity`] — a domain object's `(type, identifier)` key.
//! - [`Acl`] — owner + ordered [`AccessControlEntry`]s + optional parent for
//!   **inheritance**.
//! - [`AclService`] — looks an [`Acl`] up by identity; [`InMemoryAclService`] is
//!   the built-in mutable store (Spring's `MutableAclService`).
//! - [`AclPermissionEvaluator`] — bridges an [`AclService`] to the
//!   [`PermissionEvaluator`](crate::PermissionEvaluator) port, so
//!   `hasPermission(...)` in method security resolves against the ACLs.
//!
//! Evaluation is **default-deny**: a permission is granted only if an applicable
//! granting ACE is found (locally or via the inheritance chain); the first ACE
//! matching a `(sid, permission)` wins, so a deny ACE placed before a grant
//! takes precedence (Spring's `DefaultPermissionGrantingStrategy`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::authentication::{Authentication, ROLE_PREFIX};
use crate::permission::PermissionEvaluator;

/// The maximum number of parent hops followed during inheritance resolution — a
/// guard against a cyclic or pathologically deep parent chain.
const MAX_INHERITANCE_DEPTH: usize = 32;

/// A permission as a bitmask — the Rust analog of Spring's `BasePermission`.
///
/// The five base permissions occupy single bits and can be combined with
/// [`union`](Permission::union) into a cumulative mask. An ACE's permission
/// *contains* a requested permission when every requested bit is set in the
/// ACE's mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Permission(i32);

impl Permission {
    /// Read access (mask `1`).
    pub const READ: Permission = Permission(1);
    /// Write / modify access (mask `2`).
    pub const WRITE: Permission = Permission(2);
    /// Create access (mask `4`).
    pub const CREATE: Permission = Permission(4);
    /// Delete access (mask `8`).
    pub const DELETE: Permission = Permission(8);
    /// Administrative / take-ownership access (mask `16`).
    pub const ADMINISTRATION: Permission = Permission(16);

    /// Builds a permission from a raw bitmask.
    #[must_use]
    pub const fn from_mask(mask: i32) -> Permission {
        Permission(mask)
    }

    /// The raw bitmask.
    #[must_use]
    pub const fn mask(self) -> i32 {
        self.0
    }

    /// The cumulative permission carrying both sets of bits.
    #[must_use]
    pub const fn union(self, other: Permission) -> Permission {
        Permission(self.0 | other.0)
    }

    /// Whether this (cumulative) permission contains every bit of `required`.
    #[must_use]
    pub const fn contains(self, required: Permission) -> bool {
        (self.0 & required.0) == required.0
    }

    /// Parses a base-permission name (case-insensitive), the bridge from a
    /// method-security `hasPermission(obj, "read")` string to a [`Permission`].
    /// Recognises `read`, `write`, `create`, `delete`, and
    /// `administration` (or `admin`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Permission> {
        match name.trim().to_ascii_lowercase().as_str() {
            "read" => Some(Permission::READ),
            "write" => Some(Permission::WRITE),
            "create" => Some(Permission::CREATE),
            "delete" => Some(Permission::DELETE),
            "administration" | "admin" => Some(Permission::ADMINISTRATION),
            _ => None,
        }
    }
}

/// A security identity an [`AccessControlEntry`] is granted to — Spring's `Sid`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Sid {
    /// A specific principal, by username (Spring's `PrincipalSid`).
    Principal(String),
    /// A granted authority / role (Spring's `GrantedAuthoritySid`), e.g.
    /// `ROLE_ADMIN`.
    Authority(String),
}

/// A domain object's identity — its `(type, identifier)` key, Spring's
/// `ObjectIdentity`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectIdentity {
    /// The object's type (e.g. a fully-qualified domain-class name or table).
    pub object_type: String,
    /// The object's identifier within its type (e.g. a primary key).
    pub identifier: String,
}

impl ObjectIdentity {
    /// Builds an object identity from a type and identifier.
    #[must_use]
    pub fn new(object_type: impl Into<String>, identifier: impl Into<String>) -> Self {
        Self {
            object_type: object_type.into(),
            identifier: identifier.into(),
        }
    }

    fn key(&self) -> (String, String) {
        (self.object_type.clone(), self.identifier.clone())
    }
}

/// One entry of an [`Acl`] — grants (or denies) a [`Permission`] to a [`Sid`].
/// Spring's `AccessControlEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessControlEntry {
    /// The identity this entry applies to.
    pub sid: Sid,
    /// The permission (possibly cumulative) this entry concerns.
    pub permission: Permission,
    /// `true` grants the permission; `false` explicitly denies it.
    pub granting: bool,
}

/// An access-control list for one domain object — Spring's `Acl`.
///
/// Entries are evaluated in order; the first entry matching a `(sid, permission)`
/// decides. When no local entry applies and [`entries_inheriting`](Acl::entries_inheriting)
/// is set, evaluation continues at the [`parent`](Acl::parent).
#[derive(Debug, Clone)]
pub struct Acl {
    /// The object this ACL governs.
    pub object_identity: ObjectIdentity,
    /// The object's owner (always granted, implicitly, administrative control in
    /// Spring; here exposed via [`owner`](Acl::owner) for callers that honour it).
    pub owner: Sid,
    /// The ordered access-control entries.
    pub entries: Vec<AccessControlEntry>,
    /// The parent object whose ACL is consulted when inheriting.
    pub parent: Option<ObjectIdentity>,
    /// Whether unmatched permissions fall through to the [`parent`](Acl::parent).
    pub entries_inheriting: bool,
}

impl Acl {
    /// Builds an empty, non-inheriting ACL for `object_identity` owned by `owner`.
    #[must_use]
    pub fn new(object_identity: ObjectIdentity, owner: Sid) -> Self {
        Self {
            object_identity,
            owner,
            entries: Vec::new(),
            parent: None,
            entries_inheriting: false,
        }
    }

    /// Appends a granting entry for `sid` / `permission`.
    #[must_use]
    pub fn grant(mut self, sid: Sid, permission: Permission) -> Self {
        self.entries.push(AccessControlEntry {
            sid,
            permission,
            granting: true,
        });
        self
    }

    /// Appends a denying entry for `sid` / `permission` (takes precedence over a
    /// later grant for the same pair).
    #[must_use]
    pub fn deny(mut self, sid: Sid, permission: Permission) -> Self {
        self.entries.push(AccessControlEntry {
            sid,
            permission,
            granting: false,
        });
        self
    }

    /// Sets a parent object for inheritance.
    #[must_use]
    pub fn with_parent(mut self, parent: ObjectIdentity, inheriting: bool) -> Self {
        self.parent = Some(parent);
        self.entries_inheriting = inheriting;
        self
    }

    /// The object's owner.
    #[must_use]
    pub fn owner(&self) -> &Sid {
        &self.owner
    }

    /// The local decision for `permission`/`sids`, ignoring inheritance:
    /// `Some(true)` granted, `Some(false)` denied, `None` no applicable entry.
    fn local_decision(&self, permission: Permission, sids: &[Sid]) -> Option<bool> {
        for ace in &self.entries {
            if sids.contains(&ace.sid) && ace.permission.contains(permission) {
                return Some(ace.granting);
            }
        }
        None
    }
}

/// Looks up [`Acl`]s by [`ObjectIdentity`] — Spring's `AclService`.
pub trait AclService: Send + Sync {
    /// Reads the ACL for `object_identity`, if one exists.
    fn read_acl(&self, object_identity: &ObjectIdentity) -> Option<Acl>;
}

/// Resolves whether `sids` are granted `permission` on `object_identity`,
/// following the inheritance chain. **Default-deny**: returns `false` when no
/// applicable entry exists anywhere in the chain, when there is no ACL, or when
/// the chain is cyclic / deeper than [`MAX_INHERITANCE_DEPTH`].
#[must_use]
pub fn is_granted(
    service: &dyn AclService,
    object_identity: &ObjectIdentity,
    permission: Permission,
    sids: &[Sid],
) -> bool {
    let mut current = service.read_acl(object_identity);
    for _ in 0..MAX_INHERITANCE_DEPTH {
        let Some(acl) = current else {
            return false;
        };
        if let Some(decision) = acl.local_decision(permission, sids) {
            return decision;
        }
        match (acl.entries_inheriting, &acl.parent) {
            (true, Some(parent)) => current = service.read_acl(parent),
            _ => return false,
        }
    }
    false
}

/// An in-memory, mutable [`AclService`] — Spring's `MutableAclService`.
#[derive(Default)]
pub struct InMemoryAclService {
    acls: Mutex<HashMap<(String, String), Acl>>,
}

impl InMemoryAclService {
    /// Builds an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces the ACL for its object identity.
    pub fn save(&self, acl: Acl) {
        let key = acl.object_identity.key();
        self.acls
            .lock()
            .expect("acl store poisoned")
            .insert(key, acl);
    }

    /// Removes the ACL for `object_identity`, returning whether one existed.
    pub fn delete(&self, object_identity: &ObjectIdentity) -> bool {
        self.acls
            .lock()
            .expect("acl store poisoned")
            .remove(&object_identity.key())
            .is_some()
    }
}

impl AclService for InMemoryAclService {
    fn read_acl(&self, object_identity: &ObjectIdentity) -> Option<Acl> {
        self.acls
            .lock()
            .expect("acl store poisoned")
            .get(&object_identity.key())
            .cloned()
    }
}

/// A [`PermissionEvaluator`] backed by an [`AclService`] — Spring's
/// `AclPermissionEvaluator`. It turns the principal's identity and authorities
/// into [`Sid`]s and resolves the requested permission against the object's ACL.
///
/// Both `PermissionEvaluator` forms are supported:
/// - **id-based** ([`has_permission_for_id`](PermissionEvaluator::has_permission_for_id)) —
///   pass the object's `(type, identifier)`; this is the natural ACL entry point.
/// - **object-based** ([`has_permission`](PermissionEvaluator::has_permission)) —
///   pass a reference to an [`ObjectIdentity`] as the target (other target types
///   deny).
pub struct AclPermissionEvaluator {
    service: Arc<dyn AclService>,
}

impl AclPermissionEvaluator {
    /// Builds an evaluator over `service`.
    #[must_use]
    pub fn new(service: Arc<dyn AclService>) -> Self {
        Self { service }
    }

    /// The SIDs an authentication presents: its principal, plus a
    /// [`Sid::Authority`] for every role and authority. Each bare role also
    /// yields its `ROLE_`-prefixed form (and vice versa) so an ACE configured
    /// either way matches.
    fn sids_for(auth: &Authentication) -> Vec<Sid> {
        let mut sids = vec![Sid::Principal(auth.principal.clone())];
        let mut push_authority = |value: &str| {
            let sid = Sid::Authority(value.to_string());
            if !sids.contains(&sid) {
                sids.push(sid);
            }
        };
        for role in &auth.roles {
            push_authority(role);
            if let Some(bare) = role.strip_prefix(ROLE_PREFIX) {
                push_authority(bare);
            } else {
                push_authority(&format!("{ROLE_PREFIX}{role}"));
            }
        }
        for authority in &auth.authorities {
            push_authority(authority);
        }
        sids
    }

    fn decide(&self, auth: &Authentication, oid: &ObjectIdentity, permission: &str) -> bool {
        let Some(permission) = Permission::from_name(permission) else {
            return false;
        };
        is_granted(&*self.service, oid, permission, &Self::sids_for(auth))
    }
}

impl PermissionEvaluator for AclPermissionEvaluator {
    fn has_permission(
        &self,
        auth: &Authentication,
        target: &dyn std::any::Any,
        permission: &str,
    ) -> bool {
        match target.downcast_ref::<ObjectIdentity>() {
            Some(oid) => self.decide(auth, oid, permission),
            None => false,
        }
    }

    fn has_permission_for_id(
        &self,
        auth: &Authentication,
        object_type: &str,
        identifier: &str,
        permission: &str,
    ) -> bool {
        let oid = ObjectIdentity::new(object_type, identifier);
        self.decide(auth, &oid, permission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: &str = "com.example.BankAccount";

    fn principal(name: &str, roles: &[&str]) -> Authentication {
        Authentication {
            principal: name.into(),
            username: name.into(),
            roles: roles.iter().map(|r| (*r).to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn permission_masks_and_names() {
        assert_eq!(Permission::READ.mask(), 1);
        assert_eq!(Permission::ADMINISTRATION.mask(), 16);
        // Cumulative mask contains its parts but not unrelated bits.
        let rw = Permission::READ.union(Permission::WRITE);
        assert!(rw.contains(Permission::READ));
        assert!(rw.contains(Permission::WRITE));
        assert!(!rw.contains(Permission::DELETE));
        // Name parsing is case-insensitive.
        assert_eq!(Permission::from_name("READ"), Some(Permission::READ));
        assert_eq!(
            Permission::from_name("admin"),
            Some(Permission::ADMINISTRATION)
        );
        assert_eq!(Permission::from_name("nope"), None);
    }

    #[test]
    fn first_matching_ace_decides_and_deny_precedes_grant() {
        let oid = ObjectIdentity::new(ACCOUNT, "1");
        let alice = Sid::Principal("alice".into());
        // A deny placed before a grant for the same (sid, permission) wins.
        let acl = Acl::new(oid.clone(), alice.clone())
            .deny(alice.clone(), Permission::WRITE)
            .grant(alice.clone(), Permission::WRITE)
            .grant(alice.clone(), Permission::READ);
        assert_eq!(
            acl.local_decision(Permission::WRITE, std::slice::from_ref(&alice)),
            Some(false)
        );
        assert_eq!(acl.local_decision(Permission::READ, &[alice]), Some(true));
        // A sid with no entry → no local decision.
        assert_eq!(
            acl.local_decision(Permission::READ, &[Sid::Principal("bob".into())]),
            None
        );
    }

    #[test]
    fn service_is_granted_resolves_direct_entries() {
        let svc = InMemoryAclService::new();
        let oid = ObjectIdentity::new(ACCOUNT, "1");
        let alice = Sid::Principal("alice".into());
        svc.save(Acl::new(oid.clone(), alice.clone()).grant(alice.clone(), Permission::READ));

        assert!(is_granted(
            &svc,
            &oid,
            Permission::READ,
            std::slice::from_ref(&alice)
        ));
        // Permission not granted → deny.
        assert!(!is_granted(&svc, &oid, Permission::WRITE, &[alice]));
        // No ACL for the object → default-deny.
        let other = ObjectIdentity::new(ACCOUNT, "999");
        assert!(!is_granted(
            &svc,
            &other,
            Permission::READ,
            &[Sid::Principal("alice".into())]
        ));
    }

    #[test]
    fn inheritance_consults_the_parent_only_when_inheriting() {
        let svc = InMemoryAclService::new();
        let parent = ObjectIdentity::new(ACCOUNT, "parent");
        let child = ObjectIdentity::new(ACCOUNT, "child");
        let admins = Sid::Authority("ROLE_ADMIN".into());

        // Parent grants ROLE_ADMIN read; child has no local entry.
        svc.save(Acl::new(parent.clone(), admins.clone()).grant(admins.clone(), Permission::READ));
        svc.save(Acl::new(child.clone(), admins.clone()).with_parent(parent.clone(), true));
        assert!(is_granted(
            &svc,
            &child,
            Permission::READ,
            std::slice::from_ref(&admins)
        ));

        // Non-inheriting child does NOT see the parent's grant.
        svc.save(Acl::new(child.clone(), admins.clone()).with_parent(parent.clone(), false));
        assert!(!is_granted(&svc, &child, Permission::READ, &[admins]));
    }

    #[test]
    fn inheritance_terminates_on_a_cycle() {
        let svc = InMemoryAclService::new();
        let a = ObjectIdentity::new(ACCOUNT, "a");
        let b = ObjectIdentity::new(ACCOUNT, "b");
        let alice = Sid::Principal("alice".into());
        // a -> b -> a, no granting entries: resolution must terminate and deny.
        svc.save(Acl::new(a.clone(), alice.clone()).with_parent(b.clone(), true));
        svc.save(Acl::new(b.clone(), alice.clone()).with_parent(a.clone(), true));
        assert!(!is_granted(&svc, &a, Permission::READ, &[alice]));
    }

    #[test]
    fn evaluator_grants_by_principal_and_by_authority() {
        let svc = Arc::new(InMemoryAclService::new());
        let oid = ObjectIdentity::new(ACCOUNT, "1");
        svc.save(
            Acl::new(oid.clone(), Sid::Principal("alice".into()))
                .grant(Sid::Principal("alice".into()), Permission::READ)
                .grant(Sid::Authority("ROLE_ADMIN".into()), Permission::WRITE),
        );
        let eval = AclPermissionEvaluator::new(svc);

        let alice = principal("alice", &[]);
        // id-based form: alice (principal) may read.
        assert!(eval.has_permission_for_id(&alice, ACCOUNT, "1", "read"));
        // alice has no write grant.
        assert!(!eval.has_permission_for_id(&alice, ACCOUNT, "1", "write"));

        // An admin (bare role "ADMIN" → matched as ROLE_ADMIN authority) may write.
        let admin = principal("carol", &["ADMIN"]);
        assert!(eval.has_permission_for_id(&admin, ACCOUNT, "1", "write"));
        // …but the admin is not granted read (only ROLE_ADMIN write + alice read).
        assert!(!eval.has_permission_for_id(&admin, ACCOUNT, "1", "read"));

        // Object-based form with an ObjectIdentity target works too.
        assert!(eval.has_permission(&alice, &oid, "read"));
        // Unknown target type denies.
        assert!(!eval.has_permission(&alice, &"a string", "read"));
        // Unknown permission name denies.
        assert!(!eval.has_permission_for_id(&alice, ACCOUNT, "1", "teleport"));
    }
}
