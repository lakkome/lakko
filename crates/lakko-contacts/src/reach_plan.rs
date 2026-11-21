// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Which held reach grant writes a Welcome or signs a committer delegation
//! (G-53, maintainer decision 2026-11-21).
//!
//! A device keeps the reach grants other people handed it, filed per person,
//! class and the scope they arrived in (the group or channel whose profile or
//! member card carried them). Since ADR-0078 section 9 the `group`, `member`
//! and `channel` keys are **account-wide**: one key per class and version,
//! handed out in every group, so the scope a grant arrived in says nothing
//! about the key, only where it came from. The three clients each ordered
//! these grants their own way, never compared versions across scopes, and let
//! a stale grant block the unsigned write to an open mailbox. This module is
//! the one rule they call instead.
//!
//! ## The rule
//!
//! [`plan_reach_writes`] takes the grants held for **one person and one
//! mailbox** (every binding already verified by the caller against the
//! account key the person is addressed by) and a [`ReachWriteContext`]:
//!
//! 1. **Supersession.** For an account-wide class (`contact`, `group`,
//!    `member`, `channel`, `fof`) only the highest version held counts,
//!    whatever scope it arrived in; every lower version is returned in
//!    [`ReachWritePlan::superseded`] for the caller to delete. For a scoped
//!    class (`link`, `reply`) the same holds per scope id. Grants of equal
//!    class and version are the same key and collapse to one candidate.
//! 2. **Shared scope.** A `group` or `member` grant is a candidate only while
//!    the person and this account share at least one current scope
//!    ([`ReachWriteContext::shared_scopes`], computed by the caller from
//!    rosters), or when it is the grant the search of this add found. A
//!    highest-version grant that fails this is kept (not superseded) and not
//!    offered.
//! 3. **Welcome order:** the found grant of this add, then `contact`, then
//!    each `reply`, then `member`, then `group`, then `channel`, then `fof`.
//!    `link` is never a candidate.
//! 4. **Delegation order:** the same restricted to `contact`, `group`,
//!    `channel` and `fof`. `member` and `reply` are excluded on purpose: the
//!    Delivery Service would accept them (a delegation names no class), but a
//!    `member` key is handed to co-members the person may hide from and is
//!    for Welcomes into a group the person shares, and a `reply` key opens
//!    the mailbox for one flow (a request it answers).
//! 5. **Open fallback:** [`open_mailbox_write_allowed`] says when the unsigned
//!    write to an open mailbox may follow: the person's record is open and
//!    every candidate was refused (`401`), which includes holding none.
//!
//! The found reach comes first because the search that produced this add
//! just saw it work. An open mailbox found by the search is not a signed
//! candidate: the Delivery Service answers a proofless write to a closed
//! mailbox as if stored, so the unsigned write must come after every signed
//! one, which can be refused visibly.

use core::result::Result as CoreResult;
use std::collections::BTreeMap;

use thiserror::Error;

use crate::reach::{ReachClass, MAX_REACH_SCOPE_ID_LEN};

/// What the write is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReachPurpose {
    /// A Welcome this device writes to the person's mailbox itself.
    Welcome,
    /// A committer delegation this device signs so another device writes
    /// the Welcome.
    Delegation,
}

impl ReachPurpose {
    /// The classes this purpose may use, in the order they are tried.
    #[must_use]
    pub const fn class_order(self) -> &'static [ReachClass] {
        match self {
            Self::Welcome => &[
                ReachClass::Contact,
                ReachClass::Reply,
                ReachClass::Member,
                ReachClass::Group,
                ReachClass::Channel,
                ReachClass::Fof,
            ],
            Self::Delegation => &[
                ReachClass::Contact,
                ReachClass::Group,
                ReachClass::Channel,
                ReachClass::Fof,
            ],
        }
    }

    /// Whether this purpose may use `class`.
    #[must_use]
    pub fn allows(self, class: ReachClass) -> bool {
        self.class_order().contains(&class)
    }
}

/// One grant this device holds for the person, as the rule sees it. The
/// seed and binding stay with the caller; `index` is the caller's handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldReach {
    /// The grant's class.
    pub class: ReachClass,
    /// The scope it is filed under: for `group`, `member`, `channel` the
    /// group or channel it arrived in; for `link` and `reply` the link or
    /// request id; empty for `contact`, `fof` and a found grant. At most
    /// [`MAX_REACH_SCOPE_ID_LEN`] bytes.
    pub scope: Vec<u8>,
    /// The grant's version.
    pub version: u32,
    /// The caller's handle for this grant; unique within one call.
    pub index: u32,
}

/// The situation of one write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReachWriteContext {
    /// The group the Welcome or the delegation is for, if any. Only used to
    /// choose which of several equal grants represents a candidate.
    pub target_scope: Option<Vec<u8>>,
    /// The group and channel ids both this account and the person are
    /// currently members of. Order and repeats do not matter.
    pub shared_scopes: Vec<Vec<u8>>,
    /// The `index` of the grant the search that produced this add found
    /// the person through, if any.
    pub found: Option<u32>,
}

/// What [`plan_reach_writes`] decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReachWritePlan {
    /// The grants to try, best first, by `index`; one per distinct key.
    pub candidates: Vec<u32>,
    /// The grants a higher version of the same key family replaced, by
    /// `index`, ascending. The caller deletes them.
    pub superseded: Vec<u32>,
}

/// Why the input was refused. A reason code; never a seed byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReachPlanError {
    /// A scope id is longer than [`MAX_REACH_SCOPE_ID_LEN`].
    #[error("a scope id is {len} bytes, at most {MAX_REACH_SCOPE_ID_LEN} allowed")]
    ScopeTooLong {
        /// The offending length.
        len: usize,
    },
    /// Two held grants carry the same index.
    #[error("held grant index {0} appears twice")]
    DuplicateIndex(u32),
    /// The found index names no held grant.
    #[error("found index {0} names no held grant")]
    FoundNotHeld(u32),
}

impl ReachPlanError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ScopeTooLong { .. } => "scopeTooLong",
            Self::DuplicateIndex(_) => "duplicateIndex",
            Self::FoundNotHeld(_) => "foundNotHeld",
        }
    }
}

/// The key family a grant belongs to: one per account-wide class, one per
/// scope for `link` and `reply`.
fn family(held: &HeldReach) -> (ReachClass, &[u8]) {
    if held.class.is_account_wide() {
        (held.class, &[])
    } else {
        (held.class, held.scope.as_slice())
    }
}

/// The ordering key choosing which of several equal grants represents their
/// candidate: the found grant, then one filed in the target scope, then one
/// in a shared scope, then the lowest scope bytes, then the lowest index.
fn representative_key<'a>(
    held: &'a HeldReach,
    ctx: &ReachWriteContext,
) -> (bool, bool, bool, &'a [u8], u32) {
    let is_found = ctx.found == Some(held.index);
    let in_target = ctx.target_scope.as_deref() == Some(held.scope.as_slice());
    let in_shared = ctx.shared_scopes.iter().any(|s| s == &held.scope);
    (!is_found, !in_target, !in_shared, &held.scope, held.index)
}

/// Plan a write to one person's mailbox: the candidates in the order to try
/// them, and the grants to delete. Pure and total over valid input; the
/// result does not depend on the order of `held` or of `shared_scopes`.
///
/// # Errors
///
/// * [`ReachPlanError::ScopeTooLong`] — a held, target or shared scope is
///   longer than [`MAX_REACH_SCOPE_ID_LEN`].
/// * [`ReachPlanError::DuplicateIndex`] — two held grants carry one index.
/// * [`ReachPlanError::FoundNotHeld`] — `found` names no held grant.
pub fn plan_reach_writes(
    held: &[HeldReach],
    purpose: ReachPurpose,
    ctx: &ReachWriteContext,
) -> CoreResult<ReachWritePlan, ReachPlanError> {
    let scope_ok = |s: &[u8]| {
        if s.len() > MAX_REACH_SCOPE_ID_LEN {
            Err(ReachPlanError::ScopeTooLong { len: s.len() })
        } else {
            Ok(())
        }
    };
    let mut indices = std::collections::BTreeSet::new();
    for h in held {
        scope_ok(&h.scope)?;
        if !indices.insert(h.index) {
            return Err(ReachPlanError::DuplicateIndex(h.index));
        }
    }
    if let Some(t) = &ctx.target_scope {
        scope_ok(t)?;
    }
    for s in &ctx.shared_scopes {
        scope_ok(s)?;
    }
    if let Some(found) = ctx.found {
        if !indices.contains(&found) {
            return Err(ReachPlanError::FoundNotHeld(found));
        }
    }

    // Rule 1: the highest version per key family.
    let mut highest: BTreeMap<(ReachClass, &[u8]), u32> = BTreeMap::new();
    for h in held {
        let entry = highest.entry(family(h)).or_insert(h.version);
        if h.version > *entry {
            *entry = h.version;
        }
    }
    let mut superseded: Vec<u32> = held
        .iter()
        .filter(|h| highest.get(&family(h)).is_some_and(|top| h.version < *top))
        .map(|h| h.index)
        .collect();
    superseded.sort_unstable();

    // One candidate per family: its highest-version grants, collapsed to the
    // representative, if the purpose and rule 2 allow it.
    let found_family = ctx
        .found
        .and_then(|i| held.iter().find(|h| h.index == i))
        .map(family);
    let shares_scope = !ctx.shared_scopes.is_empty();
    let mut candidate_of: BTreeMap<(ReachClass, &[u8]), u32> = BTreeMap::new();
    for (fam, top) in &highest {
        let (class, _) = *fam;
        if !purpose.allows(class) {
            continue;
        }
        let tops: Vec<&HeldReach> = held
            .iter()
            .filter(|h| family(h) == *fam && h.version == *top)
            .collect();
        let found_here =
            found_family == Some(*fam) && tops.iter().any(|h| ctx.found == Some(h.index));
        let needs_shared = matches!(class, ReachClass::Group | ReachClass::Member);
        if needs_shared && !shares_scope && !found_here {
            continue;
        }
        if let Some(rep) = tops
            .iter()
            .min_by(|a, b| representative_key(a, ctx).cmp(&representative_key(b, ctx)))
        {
            candidate_of.insert(*fam, rep.index);
        }
    }

    // Rule 3/4: order. The found candidate first, then by class order, and
    // within a class (only `reply` has several) by scope bytes.
    let mut candidates: Vec<u32> = Vec::with_capacity(candidate_of.len());
    if let Some(fam) = found_family {
        if let Some(&idx) = candidate_of.get(&fam) {
            if ctx.found == Some(idx) {
                candidates.push(idx);
            }
        }
    }
    for class in purpose.class_order() {
        // BTreeMap order within a class is by scope bytes ascending.
        for (fam, idx) in &candidate_of {
            if fam.0 == *class && !candidates.contains(idx) {
                candidates.push(*idx);
            }
        }
    }
    Ok(ReachWritePlan {
        candidates,
        superseded,
    })
}

/// Whether the unsigned write to the person's open mailbox may be tried:
/// the person's record is open (a public record, or the search of this add
/// found the person in Everyone) and every one of the `candidates` signed
/// candidates of [`plan_reach_writes`] was refused (`401`), which holds when
/// there are none. `refused` counts distinct candidates refused with `401`;
/// any other failure (a network error) is not a refusal and keeps the write
/// owed.
#[must_use]
pub const fn open_mailbox_write_allowed(record_open: bool, candidates: u32, refused: u32) -> bool {
    record_open && refused >= candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn g(class: ReachClass, scope: &[u8], version: u32, index: u32) -> HeldReach {
        HeldReach {
            class,
            scope: scope.to_vec(),
            version,
            index,
        }
    }

    fn ctx(shared: &[&[u8]], found: Option<u32>) -> ReachWriteContext {
        ReachWriteContext {
            target_scope: None,
            shared_scopes: shared.iter().map(|s| s.to_vec()).collect(),
            found,
        }
    }

    const X: &[u8] = &[0x11; 32];
    const Y: &[u8] = &[0x22; 32];
    const T: &[u8] = &[0x33; 32];

    #[test]
    fn welcome_order_is_found_contact_reply_member_group_channel_fof() {
        let held = vec![
            g(ReachClass::Fof, &[], 0, 1),
            g(ReachClass::Channel, &[9; 16], 0, 2),
            g(ReachClass::Group, X, 0, 3),
            g(ReachClass::Member, X, 0, 4),
            g(ReachClass::Reply, &[7; 16], 0, 5),
            g(ReachClass::Contact, &[], 0, 6),
            g(ReachClass::Link, &[8; 16], 0, 7),
        ];
        let plan = plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[X], None)).unwrap();
        assert_eq!(plan.candidates, vec![6, 5, 4, 3, 2, 1]);
        assert!(plan.superseded.is_empty());
        let plan = plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[X], Some(1))).unwrap();
        assert_eq!(plan.candidates, vec![1, 6, 5, 4, 3, 2]);
    }

    #[test]
    fn delegation_excludes_member_reply_and_link() {
        let held = vec![
            g(ReachClass::Member, X, 0, 1),
            g(ReachClass::Reply, &[7; 16], 0, 2),
            g(ReachClass::Link, &[8; 16], 0, 3),
            g(ReachClass::Group, X, 0, 4),
        ];
        let plan = plan_reach_writes(&held, ReachPurpose::Delegation, &ctx(&[X], Some(1))).unwrap();
        assert_eq!(plan.candidates, vec![4]);
    }

    #[test]
    fn lower_versions_are_superseded_across_scopes() {
        let held = vec![
            g(ReachClass::Group, X, 3, 1),
            g(ReachClass::Group, Y, 5, 2),
            g(ReachClass::Member, X, 5, 3),
            g(ReachClass::Member, Y, 2, 4),
            g(ReachClass::Contact, &[], 1, 5),
            g(ReachClass::Contact, &[], 4, 6),
        ];
        let plan = plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[X], None)).unwrap();
        assert_eq!(plan.superseded, vec![1, 4, 5]);
        assert_eq!(plan.candidates, vec![6, 3, 2]);
    }

    #[test]
    fn equal_versions_collapse_preferring_target_then_shared() {
        let held = vec![
            g(ReachClass::Member, X, 2, 1),
            g(ReachClass::Member, Y, 2, 2),
            g(ReachClass::Member, T, 2, 3),
        ];
        let mut c = ctx(&[Y, T], None);
        assert_eq!(
            plan_reach_writes(&held, ReachPurpose::Welcome, &c)
                .unwrap()
                .candidates,
            vec![2]
        );
        c.target_scope = Some(T.to_vec());
        assert_eq!(
            plan_reach_writes(&held, ReachPurpose::Welcome, &c)
                .unwrap()
                .candidates,
            vec![3]
        );
    }

    #[test]
    fn group_and_member_need_a_shared_scope_unless_found() {
        let held = vec![
            g(ReachClass::Group, X, 4, 1),
            g(ReachClass::Member, X, 4, 2),
            g(ReachClass::Group, Y, 2, 3),
        ];
        // Highest version from a scope no longer shared: kept, not offered,
        // not superseded; the lower one is superseded.
        let plan = plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[], None)).unwrap();
        assert!(plan.candidates.is_empty());
        assert_eq!(plan.superseded, vec![3]);
        // Any current shared scope makes the highest version usable.
        let plan = plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[T], None)).unwrap();
        assert_eq!(plan.candidates, vec![2, 1]);
        // A found grant needs no shared scope.
        let held = vec![g(ReachClass::Group, &[], 4, 9)];
        let plan = plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[], Some(9))).unwrap();
        assert_eq!(plan.candidates, vec![9]);
    }

    #[test]
    fn found_collapses_with_an_equal_held_grant_and_leads() {
        let held = vec![
            g(ReachClass::Contact, &[], 0, 1),
            g(ReachClass::Group, X, 6, 2),
            g(ReachClass::Group, &[], 6, 3),
        ];
        let plan = plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[X], Some(3))).unwrap();
        assert_eq!(plan.candidates, vec![3, 1]);
    }

    #[test]
    fn a_superseded_found_grant_does_not_lead() {
        let held = vec![
            g(ReachClass::Contact, &[], 0, 1),
            g(ReachClass::Group, X, 6, 2),
            g(ReachClass::Group, &[], 3, 3),
        ];
        let plan = plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[X], Some(3))).unwrap();
        assert_eq!(plan.candidates, vec![1, 2]);
        assert_eq!(plan.superseded, vec![3]);
    }

    #[test]
    fn reply_grants_are_per_scope() {
        let held = vec![
            g(ReachClass::Reply, &[2; 16], 0, 1),
            g(ReachClass::Reply, &[1; 16], 0, 2),
            g(ReachClass::Reply, &[1; 16], 1, 3),
        ];
        let plan = plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[], None)).unwrap();
        assert_eq!(plan.candidates, vec![3, 1]);
        assert_eq!(plan.superseded, vec![2]);
    }

    #[test]
    fn bad_input_is_refused() {
        let held = vec![
            g(ReachClass::Contact, &[], 0, 1),
            g(ReachClass::Fof, &[], 0, 1),
        ];
        assert_eq!(
            plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[], None)),
            Err(ReachPlanError::DuplicateIndex(1))
        );
        let held = vec![g(ReachClass::Contact, &[], 0, 1)];
        assert_eq!(
            plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[], Some(2))),
            Err(ReachPlanError::FoundNotHeld(2))
        );
        let long = vec![0u8; MAX_REACH_SCOPE_ID_LEN + 1];
        assert_eq!(
            plan_reach_writes(&held, ReachPurpose::Welcome, &ctx(&[&long], None)),
            Err(ReachPlanError::ScopeTooLong { len: 65 })
        );
        assert_eq!(ReachPlanError::DuplicateIndex(0).code(), "duplicateIndex");
        assert_eq!(ReachPlanError::FoundNotHeld(0).code(), "foundNotHeld");
        assert_eq!(
            ReachPlanError::ScopeTooLong { len: 0 }.code(),
            "scopeTooLong"
        );
    }

    #[test]
    fn open_fallback_needs_an_open_record_and_every_candidate_refused() {
        assert!(open_mailbox_write_allowed(true, 0, 0));
        assert!(open_mailbox_write_allowed(true, 2, 2));
        assert!(!open_mailbox_write_allowed(true, 2, 1));
        assert!(!open_mailbox_write_allowed(false, 0, 0));
        assert!(!open_mailbox_write_allowed(false, 1, 1));
    }

    fn class_strategy() -> impl Strategy<Value = ReachClass> {
        prop::sample::select(ReachClass::ALL.to_vec())
    }

    fn scope_strategy() -> impl Strategy<Value = Vec<u8>> {
        prop::sample::select(vec![
            vec![],
            X.to_vec(),
            Y.to_vec(),
            T.to_vec(),
            vec![5; 16],
        ])
    }

    fn held_strategy() -> impl Strategy<Value = Vec<HeldReach>> {
        prop::collection::vec((class_strategy(), scope_strategy(), 0u32..4), 0..12).prop_map(|v| {
            v.into_iter()
                .enumerate()
                .map(|(i, (class, scope, version))| HeldReach {
                    class,
                    scope,
                    version,
                    index: u32::try_from(i).unwrap_or(u32::MAX),
                })
                .collect()
        })
    }

    proptest! {
        #[test]
        fn the_plan_does_not_depend_on_input_order(
            held in held_strategy(),
            shared in prop::collection::vec(scope_strategy(), 0..3),
            seed in any::<u64>(),
            found_pick in any::<prop::sample::Index>(),
            use_found in any::<bool>(),
            delegation in any::<bool>(),
        ) {
            let purpose = if delegation { ReachPurpose::Delegation } else { ReachPurpose::Welcome };
            let found = (use_found && !held.is_empty()).then(|| held[found_pick.index(held.len())].index);
            let c = ReachWriteContext { target_scope: Some(T.to_vec()), shared_scopes: shared.clone(), found };
            let a = plan_reach_writes(&held, purpose, &c).unwrap();
            // A deterministic shuffle of held and a reversal of shared.
            let mut shuffled = held.clone();
            let n = shuffled.len();
            let mut s = seed;
            for i in (1..n).rev() {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let bound = u64::try_from(i).unwrap_or(0) + 1;
                let j = usize::try_from(s % bound).unwrap_or(0);
                shuffled.swap(i, j);
            }
            let mut rev = shared;
            rev.reverse();
            let c2 = ReachWriteContext { shared_scopes: rev, ..c };
            let b = plan_reach_writes(&shuffled, purpose, &c2).unwrap();
            prop_assert_eq!(a, b);
        }

        #[test]
        fn supersession_and_collapse_hold(
            held in held_strategy(),
            shared in prop::collection::vec(scope_strategy(), 0..3),
            delegation in any::<bool>(),
        ) {
            let purpose = if delegation { ReachPurpose::Delegation } else { ReachPurpose::Welcome };
            let plan = plan_reach_writes(&held, purpose, &ctx(&[], None)).unwrap();
            let plan_s = plan_reach_writes(
                &held,
                purpose,
                &ReachWriteContext { shared_scopes: shared, ..ReachWriteContext::default() },
            ).unwrap();
            for p in [&plan, &plan_s] {
                let by_index = |i: u32| held.iter().find(|h| h.index == i).unwrap();
                // Superseded: exactly the grants below their family's top.
                for h in &held {
                    let top = held.iter().filter(|o| family(o) == family(h)).map(|o| o.version).max().unwrap();
                    prop_assert_eq!(p.superseded.contains(&h.index), h.version < top);
                }
                // Candidates: never superseded, allowed by the purpose, one per family.
                let mut fams = Vec::new();
                for &i in &p.candidates {
                    let h = by_index(i);
                    prop_assert!(!p.superseded.contains(&i));
                    prop_assert!(purpose.allows(h.class));
                    prop_assert!(h.class != ReachClass::Link);
                    let f = family(h);
                    prop_assert!(!fams.contains(&f));
                    fams.push(f);
                }
                // Candidates follow the class order.
                let rank = |c: ReachClass| purpose.class_order().iter().position(|x| *x == c).unwrap();
                for w in p.candidates.windows(2) {
                    prop_assert!(rank(by_index(w[0]).class) <= rank(by_index(w[1]).class));
                }
            }
            // With no shared scope, no group or member grant is offered.
            for &i in &plan.candidates {
                let c = held.iter().find(|h| h.index == i).unwrap().class;
                prop_assert!(!matches!(c, ReachClass::Group | ReachClass::Member));
            }
        }

        #[test]
        fn a_live_found_grant_leads(
            held in held_strategy(),
            pick in any::<prop::sample::Index>(),
            delegation in any::<bool>(),
        ) {
            prop_assume!(!held.is_empty());
            let purpose = if delegation { ReachPurpose::Delegation } else { ReachPurpose::Welcome };
            let found = &held[pick.index(held.len())];
            let plan = plan_reach_writes(&held, purpose, &ctx(&[], Some(found.index))).unwrap();
            let top = held.iter().filter(|o| family(o) == family(found)).map(|o| o.version).max().unwrap();
            if purpose.allows(found.class) && found.version == top {
                prop_assert_eq!(plan.candidates.first().copied(), Some(found.index));
            } else {
                prop_assert!(!plan.candidates.contains(&found.index));
            }
        }
    }
}
