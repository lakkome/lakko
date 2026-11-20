// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Which account key a device of this account binds its conversation identity
//! to (ADR-0050 device binding; ADR-0074 section 3.7 and "Joining after a
//! rotation", 2026-11-15, G-56).
//!
//! After a key rotation the account's conversation leaves stay bound to the
//! oldest own predecessor, the *conversation key* `C`; the directory and the
//! phrase hold the newest key, the *current* key. A device that joins the
//! account must bind under `C`, and must not bind at all while `C` is a key it
//! does not hold: a binding under another key makes it another account to
//! every gate of its siblings, for good once it is in a conversation.
//!
//! [`binding_verdict`] is the one rule the three clients run before a session
//! lands. Keys are 32-byte Ed25519 account signature keys.
//!
//! ```text
//!   target  = C when the link names one, else the oldest predecessor, else current
//!   held(k) = k == current || k in predecessors
//!
//!   1. bound in predecessors                         -> Keep
//!   2. bound == current && holds conversations       -> KeepUnderCurrentKey
//!   3. bound == target                               -> Keep
//!   4. link could not be read                        -> Wait
//!   5. target not held                               -> Wait
//!   6. otherwise                                     -> Mint(target)
//! ```
//!
//! A client runs the rule with [`OwnLink::Absent`] first: when that answer is
//! kept ([`BindingVerdict::is_kept`]) the stored binding holds whatever the
//! link says, and no fetch is needed. Otherwise it reads the link and runs
//! the rule again with the answer.

/// What the account's own conversation link said, as read from the
/// directory's identity bundle and verified against this device's current key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnLink {
    /// No link, an empty one, or one that did not verify against the current
    /// key (minted before a reset): the account's devices follow no rotation.
    Absent,
    /// The link names this conversation key.
    Key([u8; 32]),
    /// The bundle could not be read.
    Unknown,
}

/// What to do with the device's stored conversation binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingVerdict {
    /// The stored binding is kept.
    Keep,
    /// The binding names the current key on a device that already holds
    /// conversations: kept, since a re-mint would split the device between
    /// two account keys. Unreachable for a device that passed this rule
    /// before its first conversation.
    KeepUnderCurrentKey,
    /// Do not bind, publish, join or open anything: the key the device must
    /// bind under is not held yet, or the link could not be read.
    Wait,
    /// Mint (or re-mint) the binding under this key, which this device holds.
    Mint([u8; 32]),
}

impl BindingVerdict {
    /// Whether the stored binding is kept as it is.
    #[must_use]
    pub const fn is_kept(&self) -> bool {
        matches!(self, Self::Keep | Self::KeepUnderCurrentKey)
    }
}

/// The binding verdict.
///
/// `bound` is the account key the stored binding names (`None` when there is
/// no binding or it did not verify); `predecessors` are the own predecessor
/// keys this device holds, oldest first, the current key excluded.
#[must_use]
pub fn binding_verdict(
    bound: Option<&[u8; 32]>,
    current: &[u8; 32],
    predecessors: &[[u8; 32]],
    link: &OwnLink,
    holds_conversations: bool,
) -> BindingVerdict {
    let target = match link {
        OwnLink::Key(c) => *c,
        OwnLink::Absent | OwnLink::Unknown => predecessors.first().copied().unwrap_or(*current),
    };
    if let Some(b) = bound {
        if predecessors.contains(b) {
            return BindingVerdict::Keep;
        }
        if b == current && holds_conversations {
            return BindingVerdict::KeepUnderCurrentKey;
        }
        if *b == target {
            return BindingVerdict::Keep;
        }
    }
    if matches!(link, OwnLink::Unknown) {
        return BindingVerdict::Wait;
    }
    let held = target == *current || predecessors.contains(&target);
    if held {
        BindingVerdict::Mint(target)
    } else {
        BindingVerdict::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const CUR: [u8; 32] = [0xC0; 32];
    const K0: [u8; 32] = [0x10; 32];
    const K1: [u8; 32] = [0x11; 32];
    const FOREIGN: [u8; 32] = [0xFF; 32];

    #[derive(Clone, Copy)]
    enum B {
        Nil,
        Cur,
        K0,
        K1,
        Foreign,
    }

    fn bound(b: B) -> Option<[u8; 32]> {
        match b {
            B::Nil => None,
            B::Cur => Some(CUR),
            B::K0 => Some(K0),
            B::K1 => Some(K1),
            B::Foreign => Some(FOREIGN),
        }
    }

    use BindingVerdict::{Keep, KeepUnderCurrentKey, Mint, Wait};

    /// One vector: bound, predecessors, link, holds conversations, expected.
    type Vector = (B, &'static [[u8; 32]], OwnLink, bool, BindingVerdict);

    /// The 17 shared vectors of contract-g56 section 2.1, in its order. Every
    /// client suite carries the same table.
    #[test]
    fn the_shared_truth_table() {
        let k0 = OwnLink::Key(K0);
        let table: [Vector; 17] = [
            (B::Nil, &[], OwnLink::Absent, false, Mint(CUR)),
            (B::Nil, &[K0], OwnLink::Absent, false, Mint(K0)),
            (B::Nil, &[K0], k0, false, Mint(K0)),
            (B::Nil, &[], k0, false, Wait),
            (B::Nil, &[K1], k0, false, Wait),
            (B::Nil, &[K0, K1], k0, false, Mint(K0)),
            (B::Nil, &[], OwnLink::Unknown, false, Wait),
            (B::Cur, &[], OwnLink::Absent, false, Keep),
            (B::Cur, &[], k0, false, Wait),
            (B::Cur, &[K0], k0, false, Mint(K0)),
            (B::Cur, &[K0], k0, true, KeepUnderCurrentKey),
            (B::Cur, &[], k0, true, KeepUnderCurrentKey),
            (B::K0, &[K0], k0, true, Keep),
            (B::K1, &[K0, K1], k0, true, Keep),
            (B::Foreign, &[], OwnLink::Absent, false, Mint(CUR)),
            (B::Foreign, &[], k0, false, Wait),
            (B::Foreign, &[K0], OwnLink::Unknown, false, Wait),
        ];
        for (i, (b, preds, link, conv, expected)) in table.iter().enumerate() {
            let got = binding_verdict(bound(*b).as_ref(), &CUR, preds, link, *conv);
            assert_eq!(got, *expected, "vector {}", i + 1);
        }
    }

    #[test]
    fn a_warm_device_whose_binding_holds_needs_no_fetch() {
        // Absent first: kept, so the link is never read.
        let v = binding_verdict(Some(&K0), &CUR, &[K0], &OwnLink::Absent, true);
        assert!(v.is_kept());
        let v = binding_verdict(Some(&CUR), &CUR, &[], &OwnLink::Absent, false);
        assert!(v.is_kept());
        assert!(!Wait.is_kept());
        assert!(!Mint(CUR).is_kept());
    }

    fn arb_key() -> impl Strategy<Value = [u8; 32]> {
        prop_oneof![
            Just(CUR),
            Just(K0),
            Just(K1),
            Just(FOREIGN),
            any::<[u8; 32]>()
        ]
    }

    fn arb_link() -> impl Strategy<Value = OwnLink> {
        prop_oneof![
            Just(OwnLink::Absent),
            Just(OwnLink::Unknown),
            arb_key().prop_map(OwnLink::Key)
        ]
    }

    proptest! {
        /// Whatever the inputs, a mint is only ever under a key this device
        /// holds, and under `C` whenever the link names one.
        #[test]
        fn a_mint_is_always_under_a_held_key(
            b in prop::option::of(arb_key()),
            preds in prop::collection::vec(arb_key(), 0..4),
            link in arb_link(),
            conv in any::<bool>(),
        ) {
            match binding_verdict(b.as_ref(), &CUR, &preds, &link, conv) {
                Mint(k) => {
                    prop_assert!(k == CUR || preds.contains(&k));
                    if let OwnLink::Key(c) = link { prop_assert_eq!(k, c); }
                    prop_assert!(!matches!(link, OwnLink::Unknown));
                }
                Wait => prop_assert!(!(b.is_some() && preds.contains(&b.unwrap_or_default()))),
                Keep | KeepUnderCurrentKey => prop_assert!(b.is_some()),
            }
        }
    }
}
