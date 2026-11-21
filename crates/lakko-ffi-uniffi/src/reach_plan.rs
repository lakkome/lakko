// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! G-53 — which held reach grant writes a Welcome or signs a committer
//! delegation. The rule is `lakko_contacts::reach_plan`; this is its UniFFI
//! surface, so no client keeps an ordering of its own.

use lakko_contacts::reach_plan::{
    open_mailbox_write_allowed as rust_open_mailbox_write_allowed,
    plan_reach_writes as rust_plan_reach_writes, HeldReach, ReachPlanError, ReachPurpose,
    ReachWriteContext,
};

use crate::scoped_discovery::FfiReachClass;
use crate::FfiError;

impl From<ReachPlanError> for FfiError {
    fn from(e: ReachPlanError) -> Self {
        Self::Reach(format!("{}: {e}", e.code()))
    }
}

/// What the write is for. Mirror of
/// [`lakko_contacts::reach_plan::ReachPurpose`].
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiReachPurpose {
    /// A Welcome this device writes to the person's mailbox itself: found,
    /// contact, reply, member, group, channel, fof.
    Welcome,
    /// A committer delegation this device signs: found, contact, group,
    /// channel, fof (never member, reply or link).
    Delegation,
}

impl From<FfiReachPurpose> for ReachPurpose {
    fn from(p: FfiReachPurpose) -> Self {
        match p {
            FfiReachPurpose::Welcome => Self::Welcome,
            FfiReachPurpose::Delegation => Self::Delegation,
        }
    }
}

/// One grant this device holds for the person, as the rule sees it. The seed
/// and binding stay with the caller; `index` is the caller's handle, unique
/// within one call.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiHeldReach {
    /// The grant's class.
    pub class: FfiReachClass,
    /// The scope it is filed under (the group or channel it arrived in, the
    /// link or request id; empty for contact, fof and a found grant). At most
    /// 64 bytes.
    pub scope: Vec<u8>,
    /// The grant's version.
    pub version: u32,
    /// The caller's handle.
    pub index: u32,
}

/// What [`plan_reach_writes`] decided.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiReachWritePlan {
    /// The grants to try, best first, by `index`; one per distinct key.
    pub candidates: Vec<u32>,
    /// The grants a higher version replaced, by `index`, ascending. Delete
    /// them.
    pub superseded: Vec<u32>,
}

/// Plan a Welcome or a delegation to ONE person's ONE mailbox from the
/// grants held for them (every binding already verified against the account
/// key the person is addressed by). `target_scope` is the group the write is
/// for; `shared_scopes` the group and channel ids this account and the person
/// are both currently members of; `found` the `index` of the grant the search
/// of this add found the person through. A `group` or `member` grant is
/// offered only with a shared scope or when found; lower versions of a key
/// family come back in `superseded`.
///
/// # Errors
///
/// [`FfiError::Reach`] `scopeTooLong` / `duplicateIndex` / `foundNotHeld`.
#[uniffi::export]
pub fn plan_reach_writes(
    held: Vec<FfiHeldReach>,
    purpose: FfiReachPurpose,
    target_scope: Option<Vec<u8>>,
    shared_scopes: Vec<Vec<u8>>,
    found: Option<u32>,
) -> Result<FfiReachWritePlan, FfiError> {
    let held: Vec<HeldReach> = held
        .into_iter()
        .map(|h| HeldReach {
            class: h.class.into(),
            scope: h.scope,
            version: h.version,
            index: h.index,
        })
        .collect();
    let ctx = ReachWriteContext {
        target_scope,
        shared_scopes,
        found,
    };
    let plan = rust_plan_reach_writes(&held, purpose.into(), &ctx)?;
    Ok(FfiReachWritePlan {
        candidates: plan.candidates,
        superseded: plan.superseded,
    })
}

/// Whether the unsigned write to the person's open mailbox may be tried: the
/// record is open (a public record, or the search found the person in
/// Everyone) and `refused` (candidates refused with `401`) reaches
/// `candidates` (the plan's candidate count), so also when there are none.
#[uniffi::export]
#[must_use]
pub fn reach_open_mailbox_write_allowed(record_open: bool, candidates: u32, refused: u32) -> bool {
    rust_open_mailbox_write_allowed(record_open, candidates, refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(class: FfiReachClass, scope: &[u8], version: u32, index: u32) -> FfiHeldReach {
        FfiHeldReach {
            class,
            scope: scope.to_vec(),
            version,
            index,
        }
    }

    #[test]
    fn welcome_plan_orders_supersedes_and_leads_with_found() {
        let x = vec![0x11; 32];
        let held = vec![
            h(FfiReachClass::Group, &x, 1, 0),
            h(FfiReachClass::Group, &[0x22; 32], 3, 1),
            h(FfiReachClass::Member, &x, 3, 2),
            h(FfiReachClass::Contact, &[], 0, 3),
            h(FfiReachClass::Fof, &[], 0, 4),
            h(FfiReachClass::Reply, &[7; 16], 0, 5),
        ];
        let plan = plan_reach_writes(
            held.clone(),
            FfiReachPurpose::Welcome,
            None,
            vec![x.clone()],
            Some(4),
        )
        .unwrap();
        assert_eq!(plan.candidates, vec![4, 3, 5, 2, 1]);
        assert_eq!(plan.superseded, vec![0]);
        let plan =
            plan_reach_writes(held, FfiReachPurpose::Delegation, None, vec![x], None).unwrap();
        assert_eq!(plan.candidates, vec![3, 1, 4]);
    }

    #[test]
    fn no_shared_scope_offers_no_group_or_member_grant() {
        let held = vec![
            h(FfiReachClass::Group, &[0x11; 32], 2, 0),
            h(FfiReachClass::Member, &[0x11; 32], 2, 1),
        ];
        let plan = plan_reach_writes(held, FfiReachPurpose::Welcome, None, vec![], None).unwrap();
        assert!(plan.candidates.is_empty());
        assert!(plan.superseded.is_empty());
    }

    #[test]
    fn bad_input_maps_to_coded_reach_errors() {
        let held = vec![
            h(FfiReachClass::Contact, &[], 0, 1),
            h(FfiReachClass::Fof, &[], 0, 1),
        ];
        match plan_reach_writes(held, FfiReachPurpose::Welcome, None, vec![], None) {
            Err(FfiError::Reach(m)) => assert!(m.starts_with("duplicateIndex: ")),
            other => panic!("unexpected {other:?}"),
        }
        let held = vec![h(FfiReachClass::Contact, &[], 0, 1)];
        match plan_reach_writes(
            held.clone(),
            FfiReachPurpose::Welcome,
            None,
            vec![],
            Some(2),
        ) {
            Err(FfiError::Reach(m)) => assert!(m.starts_with("foundNotHeld: ")),
            other => panic!("unexpected {other:?}"),
        }
        match plan_reach_writes(
            held,
            FfiReachPurpose::Welcome,
            Some(vec![0; 65]),
            vec![],
            None,
        ) {
            Err(FfiError::Reach(m)) => assert!(m.starts_with("scopeTooLong: ")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn open_fallback_matches_the_core() {
        assert!(reach_open_mailbox_write_allowed(true, 0, 0));
        assert!(reach_open_mailbox_write_allowed(true, 3, 3));
        assert!(!reach_open_mailbox_write_allowed(true, 3, 2));
        assert!(!reach_open_mailbox_write_allowed(false, 0, 0));
    }
}
