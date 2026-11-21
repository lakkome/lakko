// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! G-53 — which held reach grant writes a Welcome or signs a committer
//! delegation. The rule is `lakko_contacts::reach_plan`; this is its
//! wasm-bindgen surface, the mirror of `lakko-ffi-uniffi::reach_plan`.

use lakko_contacts::reach_plan::{
    open_mailbox_write_allowed as rust_open_mailbox_write_allowed,
    plan_reach_writes as rust_plan_reach_writes, HeldReach as RustHeldReach, ReachPlanError,
    ReachPurpose as RustReachPurpose, ReachWriteContext, ReachWritePlan as RustReachWritePlan,
};
use wasm_bindgen::prelude::*;

use crate::contacts::coded_error;
use crate::scoped_discovery::ReachClass;

/// What the write is for.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReachPurpose {
    /// A Welcome this device writes itself: found, contact, reply, member,
    /// group, channel, fof.
    Welcome = 1,
    /// A committer delegation this device signs: found, contact, group,
    /// channel, fof (never member, reply or link).
    Delegation = 2,
}

impl From<ReachPurpose> for RustReachPurpose {
    fn from(p: ReachPurpose) -> Self {
        match p {
            ReachPurpose::Welcome => Self::Welcome,
            ReachPurpose::Delegation => Self::Delegation,
        }
    }
}

/// One grant this device holds for the person, as the rule sees it. The seed
/// and binding stay with the caller; `index` is the caller's handle, unique
/// within one call. Consumed by [`plan_reach_writes`].
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct HeldReach {
    inner: RustHeldReach,
}

#[wasm_bindgen]
impl HeldReach {
    /// `scope`: the group or channel it arrived in, the link or request id,
    /// or empty (contact, fof, a found grant); at most 64 bytes, checked by
    /// [`plan_reach_writes`].
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(class: ReachClass, scope: &[u8], version: u32, index: u32) -> Self {
        Self {
            inner: RustHeldReach {
                class: class.into(),
                scope: scope.to_vec(),
                version,
                index,
            },
        }
    }

    /// The caller's handle.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn index(&self) -> u32 {
        self.inner.index
    }
}

/// What [`plan_reach_writes`] decided.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct ReachWritePlan {
    inner: RustReachWritePlan,
}

#[wasm_bindgen]
impl ReachWritePlan {
    /// The grants to try, best first, by `index`; one per distinct key.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn candidates(&self) -> Vec<u32> {
        self.inner.candidates.clone()
    }

    /// The grants a higher version replaced, by `index`, ascending. Delete
    /// them.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn superseded(&self) -> Vec<u32> {
        self.inner.superseded.clone()
    }
}

/// Core of [`plan_reach_writes`], testable off-wasm.
fn plan_inner(
    held: Vec<HeldReach>,
    purpose: ReachPurpose,
    target_scope: Option<Vec<u8>>,
    shared_scopes: Vec<Vec<u8>>,
    found: Option<u32>,
) -> Result<RustReachWritePlan, ReachPlanError> {
    let held: Vec<RustHeldReach> = held.into_iter().map(|h| h.inner).collect();
    let ctx = ReachWriteContext {
        target_scope,
        shared_scopes,
        found,
    };
    rust_plan_reach_writes(&held, purpose.into(), &ctx)
}

/// Plan a Welcome or a delegation to ONE person's ONE mailbox from the grants
/// held for them (every binding already verified against the account key the
/// person is addressed by). `targetScope` is the group the write is for;
/// `sharedScopes` the group and channel ids this account and the person are
/// both currently members of; `found` the `index` of the grant the search of
/// this add found the person through. A `group` or `member` grant is offered
/// only with a shared scope or when found; lower versions of a key family come
/// back in `superseded`.
///
/// Every [`HeldReach`] in `held` is consumed (freed) by this call.
///
/// # Errors
///
/// Throws with `reachError` `scopeTooLong` / `duplicateIndex` /
/// `foundNotHeld`.
#[wasm_bindgen(js_name = planReachWrites)]
pub fn plan_reach_writes(
    held: Vec<HeldReach>,
    purpose: ReachPurpose,
    target_scope: Option<Vec<u8>>,
    shared_scopes: Vec<js_sys::Uint8Array>,
    found: Option<u32>,
) -> Result<ReachWritePlan, JsValue> {
    let shared: Vec<Vec<u8>> = shared_scopes
        .iter()
        .map(js_sys::Uint8Array::to_vec)
        .collect();
    plan_inner(held, purpose, target_scope, shared, found)
        .map(|inner| ReachWritePlan { inner })
        .map_err(|e| coded_error(&e.to_string(), "reachError", e.code()))
}

/// Whether the unsigned write to the person's open mailbox may be tried: the
/// record is open (a public record, or the search found the person in
/// Everyone) and `refused` (candidates refused with `401`) reaches
/// `candidates` (the plan's candidate count), so also when there are none.
#[wasm_bindgen(js_name = reachOpenMailboxWriteAllowed)]
#[must_use]
pub fn reach_open_mailbox_write_allowed(record_open: bool, candidates: u32, refused: u32) -> bool {
    rust_open_mailbox_write_allowed(record_open, candidates, refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_and_delegation_plans_match_the_core() {
        let x = vec![0x11; 32];
        let held = || {
            vec![
                HeldReach::new(ReachClass::Group, &x, 1, 0),
                HeldReach::new(ReachClass::Group, &[0x22; 32], 3, 1),
                HeldReach::new(ReachClass::Member, &x, 3, 2),
                HeldReach::new(ReachClass::Contact, &[], 0, 3),
                HeldReach::new(ReachClass::Fof, &[], 0, 4),
                HeldReach::new(ReachClass::Reply, &[7; 16], 0, 5),
            ]
        };
        let plan = plan_inner(
            held(),
            ReachPurpose::Welcome,
            None,
            vec![x.clone()],
            Some(4),
        )
        .unwrap();
        assert_eq!(plan.candidates, vec![4, 3, 5, 2, 1]);
        assert_eq!(plan.superseded, vec![0]);
        let plan = plan_inner(held(), ReachPurpose::Delegation, None, vec![x], None).unwrap();
        assert_eq!(plan.candidates, vec![3, 1, 4]);
        let wrapped = ReachWritePlan { inner: plan };
        assert_eq!(wrapped.candidates(), vec![3, 1, 4]);
        assert_eq!(wrapped.superseded(), vec![0]);
    }

    #[test]
    fn bad_input_is_refused_with_its_code() {
        let held = vec![
            HeldReach::new(ReachClass::Contact, &[], 0, 1),
            HeldReach::new(ReachClass::Fof, &[], 0, 1),
        ];
        assert_eq!(
            plan_inner(held, ReachPurpose::Welcome, None, vec![], None)
                .unwrap_err()
                .code(),
            "duplicateIndex"
        );
        let held = vec![HeldReach::new(ReachClass::Contact, &[], 0, 1)];
        assert_eq!(
            plan_inner(held, ReachPurpose::Welcome, None, vec![], Some(9))
                .unwrap_err()
                .code(),
            "foundNotHeld"
        );
    }

    #[test]
    fn open_fallback_matches_the_core() {
        assert!(reach_open_mailbox_write_allowed(true, 0, 0));
        assert!(reach_open_mailbox_write_allowed(true, 2, 2));
        assert!(!reach_open_mailbox_write_allowed(true, 2, 1));
        assert!(!reach_open_mailbox_write_allowed(false, 0, 0));
    }
}
