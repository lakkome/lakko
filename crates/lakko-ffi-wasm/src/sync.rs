// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Mirror of `lakko-sync`: hybrid logical clock + edit chain +
//! drafts. Plain value types; no FFI subtleties.

use std::sync::Mutex;

use lakko_sync::{
    Draft as RustDraft, EditChain as RustEditChain, EditOp as RustEditOp, Hlc as RustHlc,
    HybridLogicalClock as RustHlcState, Resolved as RustResolved,
};
use wasm_bindgen::prelude::*;

// ============================================================================
// Hlc
// ============================================================================

/// JS-visible Hybrid Logical Clock value: physical milliseconds plus
/// a logical counter. Total-ordered lexicographically. Mirror of
/// [`lakko_sync::Hlc`].
#[wasm_bindgen]
#[derive(Copy, Clone, Debug)]
pub struct Hlc {
    inner: RustHlc,
}

impl From<RustHlc> for Hlc {
    fn from(inner: RustHlc) -> Self {
        Self { inner }
    }
}

impl From<Hlc> for RustHlc {
    fn from(h: Hlc) -> Self {
        h.inner
    }
}

#[wasm_bindgen]
impl Hlc {
    /// Construct an `Hlc` from raw fields. Most callers should drive
    /// a [`HybridLogicalClock`] instead.
    #[wasm_bindgen(constructor)]
    pub fn new(physical_ms: u64, logical_counter: u64) -> Self {
        Self {
            inner: RustHlc::new(physical_ms, logical_counter),
        }
    }

    /// The minimum-value `Hlc` (`pt = 0`, `lc = 0`).
    #[wasm_bindgen]
    pub fn zero() -> Self {
        RustHlc::ZERO.into()
    }

    /// Physical-time component, milliseconds since UNIX epoch.
    #[wasm_bindgen(getter, js_name = physicalMs)]
    pub fn physical_ms(&self) -> u64 {
        self.inner.pt
    }

    /// Logical counter — breaks ties when `physicalMs` collides.
    #[wasm_bindgen(getter, js_name = logicalCounter)]
    pub fn logical_counter(&self) -> u64 {
        self.inner.lc
    }

    /// Lexicographic ordering: returns `-1` if `self < other`, `0` if
    /// equal, `1` if `self > other`. JS lacks a built-in `Ord`; this
    /// is the cheapest-friction equivalent.
    #[wasm_bindgen(js_name = compareTo)]
    pub fn compare_to(&self, other: &Hlc) -> i32 {
        match self.inner.cmp(&other.inner) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }
}

// ============================================================================
// HybridLogicalClock
// ============================================================================

/// Per-device HLC state. Wrap once at startup, persist `last` across
/// restarts via [`HybridLogicalClock::last`] / the `with_state`
/// constructor. Mirror of [`lakko_sync::HybridLogicalClock`].
///
/// `Mutex` so one shared instance can be cloned into multiple async
/// callers; on wasm32 there's only one thread anyway, so the mutex
/// never actually contests.
#[wasm_bindgen]
pub struct HybridLogicalClock {
    inner: Mutex<RustHlcState>,
}

#[wasm_bindgen]
impl HybridLogicalClock {
    /// Fresh clock seeded at `Hlc::ZERO`.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RustHlcState::new()),
        }
    }

    /// Construct from a previous `last` stamp — used to keep events
    /// monotonic across app restarts (caller persists `last` to
    /// local storage).
    #[wasm_bindgen(js_name = withState)]
    pub fn with_state(last: &Hlc) -> Self {
        Self {
            inner: Mutex::new(RustHlcState::with_state(last.inner)),
        }
    }

    /// Most recent `Hlc` issued by this clock.
    #[wasm_bindgen(getter)]
    pub fn last(&self) -> Result<Hlc, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("hlc state poisoned"))?;
        Ok(g.last().into())
    }

    /// Issue a fresh stamp for a local event happening at wall-clock
    /// time `wall_now_ms`. Strictly greater than every previous
    /// return.
    #[wasm_bindgen]
    pub fn tick(&self, wall_now_ms: u64) -> Result<Hlc, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("hlc state poisoned"))?;
        Ok(g.tick(wall_now_ms).into())
    }

    /// Merge an `Hlc` from another device. Returns a fresh local
    /// stamp ordered strictly after both sides.
    #[wasm_bindgen(js_name = mergeRemote)]
    pub fn merge_remote(&self, remote: &Hlc, wall_now_ms: u64) -> Result<Hlc, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("hlc state poisoned"))?;
        Ok(g.merge_remote(remote.inner, wall_now_ms).into())
    }
}

impl Default for HybridLogicalClock {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// EditChain
// ============================================================================

/// Single operation in a message's edit chain. Use [`EditOp::update`]
/// or [`EditOp::delete`] to construct.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct EditOp {
    inner: RustEditOp,
}

#[wasm_bindgen]
impl EditOp {
    /// Construct an `Update` op.
    #[wasm_bindgen]
    pub fn update(hlc: &Hlc, body: String) -> Self {
        Self {
            inner: RustEditOp::Update {
                hlc: hlc.inner,
                body,
            },
        }
    }

    /// Construct a `Delete` (tombstone) op.
    #[wasm_bindgen]
    pub fn delete(hlc: &Hlc) -> Self {
        Self {
            inner: RustEditOp::Delete { hlc: hlc.inner },
        }
    }

    /// Hlc of this op.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> Hlc {
        self.inner.hlc().into()
    }

    /// Discriminator: `"Update"` or `"Delete"`.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        match self.inner {
            RustEditOp::Update { .. } => "Update".into(),
            RustEditOp::Delete { .. } => "Delete".into(),
        }
    }

    /// Body for `Update`, `None` for `Delete`.
    #[wasm_bindgen(getter)]
    pub fn body(&self) -> Option<String> {
        match &self.inner {
            RustEditOp::Update { body, .. } => Some(body.clone()),
            RustEditOp::Delete { .. } => None,
        }
    }
}

/// Discriminator strings for [`EditChain::resolve`]. Returned alongside
/// the resolved body via [`EditResolved`].
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EditResolvedKind {
    /// Latest op is an `Update` — `body` is `Some`.
    Visible,
    /// Latest op is `Delete`.
    Deleted,
    /// Chain is empty.
    Empty,
}

/// Resolved view of an edit chain.
#[wasm_bindgen]
pub struct EditResolved {
    kind: EditResolvedKind,
    body: Option<String>,
}

#[wasm_bindgen]
impl EditResolved {
    /// `"Visible" | "Deleted" | "Empty"`.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        match self.kind {
            EditResolvedKind::Visible => "Visible".into(),
            EditResolvedKind::Deleted => "Deleted".into(),
            EditResolvedKind::Empty => "Empty".into(),
        }
    }

    /// Resolved text — present iff `kind == "Visible"`.
    #[wasm_bindgen(getter)]
    pub fn body(&self) -> Option<String> {
        self.body.clone()
    }
}

/// Append-only edit log per message, last-write-wins by [`Hlc`].
/// Mirror of [`lakko_sync::EditChain`].
#[wasm_bindgen]
pub struct EditChain {
    inner: Mutex<RustEditChain>,
}

#[wasm_bindgen]
impl EditChain {
    /// Empty chain.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RustEditChain::new()),
        }
    }

    /// Append an op. Cheap (Vec push). Idempotent in resolution —
    /// duplicates are stored verbatim, but `resolve` still returns
    /// the highest-HLC op.
    #[wasm_bindgen]
    pub fn append(&self, op: &EditOp) -> Result<(), JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("edit chain poisoned"))?;
        g.append(op.inner.clone());
        Ok(())
    }

    /// Number of stored ops.
    #[wasm_bindgen(getter)]
    pub fn len(&self) -> Result<usize, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("edit chain poisoned"))?;
        Ok(g.len())
    }

    /// `true` if no ops have been appended.
    #[wasm_bindgen(js_name = isEmpty, getter)]
    pub fn is_empty(&self) -> Result<bool, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("edit chain poisoned"))?;
        Ok(g.is_empty())
    }

    /// Resolve the chain to user-visible state.
    #[wasm_bindgen]
    pub fn resolve(&self) -> Result<EditResolved, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("edit chain poisoned"))?;
        Ok(match g.current() {
            RustResolved::Visible(body) => EditResolved {
                kind: EditResolvedKind::Visible,
                body: Some(body),
            },
            RustResolved::Deleted => EditResolved {
                kind: EditResolvedKind::Deleted,
                body: None,
            },
            RustResolved::Empty => EditResolved {
                kind: EditResolvedKind::Empty,
                body: None,
            },
        })
    }
}

impl Default for EditChain {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Draft
// ============================================================================

/// Per-group draft. Mirror of [`lakko_sync::Draft`]. Replaceable by a
/// strictly-greater [`Hlc`] for the same `groupId`.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct Draft {
    inner: RustDraft,
}

#[wasm_bindgen]
impl Draft {
    /// Construct a fresh draft. `groupId` must be exactly 32 bytes.
    #[wasm_bindgen(constructor)]
    pub fn new(group_id: &[u8], body: String, hlc: &Hlc) -> Result<Draft, JsError> {
        if group_id.len() != 32 {
            return Err(JsError::new("groupId must be 32 bytes"));
        }
        let mut id = [0u8; 32];
        id.copy_from_slice(group_id);
        Ok(Self {
            inner: RustDraft::new(id, body, hlc.inner),
        })
    }

    /// 32-byte group pseudonym this draft belongs to.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.inner.group_id.to_vec()
    }

    /// Current draft text.
    #[wasm_bindgen(getter)]
    pub fn body(&self) -> String {
        self.inner.body.clone()
    }

    /// HLC at which the user last edited this draft.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> Hlc {
        self.inner.hlc.into()
    }

    /// Should `incoming` replace `self`? `true` iff `groupId` matches
    /// AND `incoming.hlc` is strictly greater.
    #[wasm_bindgen(js_name = shouldReplaceWith)]
    pub fn should_replace_with(&self, incoming: &Draft) -> bool {
        self.inner.should_replace_with(&incoming.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hlc_construct_and_compare() {
        let a = Hlc::new(100, 0);
        let b = Hlc::new(200, 0);
        let c = Hlc::new(100, 0);
        assert_eq!(a.compare_to(&b), -1);
        assert_eq!(b.compare_to(&a), 1);
        assert_eq!(a.compare_to(&c), 0);
    }

    #[test]
    fn hlc_zero_is_minimum() {
        assert_eq!(Hlc::zero().compare_to(&Hlc::new(1, 0)), -1);
        assert_eq!(Hlc::zero().compare_to(&Hlc::new(0, 1)), -1);
    }

    #[test]
    fn hybrid_logical_clock_ticks_strictly() {
        let c = HybridLogicalClock::new();
        let a = c.tick(1_000).unwrap();
        let b = c.tick(1_000).unwrap();
        assert_eq!(a.compare_to(&b), -1);
    }

    #[test]
    fn hybrid_logical_clock_with_state_preserves_monotonicity() {
        let saved = Hlc::new(10_000, 5);
        let c = HybridLogicalClock::with_state(&saved);
        let next = c.tick(1).unwrap();
        // Wall clock backwards → lc bumps but pt stays.
        assert_eq!(next.physical_ms(), 10_000);
        assert_eq!(next.logical_counter(), 6);
    }

    #[test]
    fn edit_chain_resolves_lww() {
        let chain = EditChain::new();
        chain
            .append(&EditOp::update(&Hlc::new(100, 0), "first".into()))
            .unwrap();
        chain
            .append(&EditOp::update(&Hlc::new(200, 0), "second".into()))
            .unwrap();
        let r = chain.resolve().unwrap();
        assert_eq!(r.kind(), "Visible");
        assert_eq!(r.body().as_deref(), Some("second"));
    }

    #[test]
    fn edit_chain_delete_tombstones_when_latest() {
        let chain = EditChain::new();
        chain
            .append(&EditOp::update(&Hlc::new(100, 0), "x".into()))
            .unwrap();
        chain.append(&EditOp::delete(&Hlc::new(200, 0))).unwrap();
        let r = chain.resolve().unwrap();
        assert_eq!(r.kind(), "Deleted");
        assert!(r.body().is_none());
    }

    #[test]
    fn edit_chain_empty_resolves_empty() {
        let chain = EditChain::new();
        assert_eq!(chain.resolve().unwrap().kind(), "Empty");
        assert!(chain.is_empty().unwrap());
        assert_eq!(chain.len().unwrap(), 0);
    }

    #[test]
    fn edit_op_kind_and_body_accessors() {
        let u = EditOp::update(&Hlc::new(1, 0), "hello".into());
        assert_eq!(u.kind(), "Update");
        assert_eq!(u.body().as_deref(), Some("hello"));
        assert_eq!(u.hlc().physical_ms(), 1);

        let d = EditOp::delete(&Hlc::new(5, 0));
        assert_eq!(d.kind(), "Delete");
        assert!(d.body().is_none());
    }

    // Length-validation error paths invoke `JsError::new`; the
    // wasm-bindgen import stub panics on host targets. Coverage for
    // length validation lives in the `lakko-sync` crate.

    #[test]
    fn draft_replacement_rules() {
        let group = [0xABu8; 32];
        let cur = Draft::new(&group, "old".into(), &Hlc::new(100, 0)).unwrap();
        let newer = Draft::new(&group, "new".into(), &Hlc::new(200, 0)).unwrap();
        let stale = Draft::new(&group, "stale".into(), &Hlc::new(50, 0)).unwrap();
        let other_group = Draft::new(&[0xCDu8; 32], "x".into(), &Hlc::new(999, 0)).unwrap();

        assert!(cur.should_replace_with(&newer));
        assert!(!cur.should_replace_with(&stale));
        assert!(!cur.should_replace_with(&other_group));
    }
}
