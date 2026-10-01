// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group-admin role table (ADR-0022) carried in the `0xFF02` GroupContext
//! extension of a conversation [`Group`](crate::group::Group).
//!
//! A role table names exactly one **owner** (the group creator) and a set of
//! **admins**, all keyed on the member's Ed25519 **signature public key** — the
//! stable per-account identity, never the leaf index (leaf indices renumber on
//! every add/remove cycle; ADR-0022). The table is stored as an opaque,
//! canonically-encoded `Extension::Unknown(0xFF02)` body inside the
//! authenticated GroupContext, so every member that has applied the
//! role-setting commit agrees on it and the zero-trust server never parses it
//! (invariant #2).
//!
//! The wire body is fixed-layout and bounded:
//!
//! ```text
//! version:     u8                       // ROLE_TABLE_VERSION; unknown => refuse
//! owner:       [u8; 32]                 // Ed25519 signature key
//! admin_count: u16 (big-endian)         // <= MAX_ADMINS
//! admins:      [[u8; 32]; admin_count]  // sorted + deduplicated (canonical)
//! ```
//!
//! Decoding is fail-closed per ADR-0022 Finding 4 (the I-7 history-bundle
//! discriminator precedent): an unrecognised `version`, a truncated body, or an
//! `admin_count` over the cap is a hard error, never a best-effort parse.

use crate::error::{Error, Result};

/// The role-table GroupContext extension type (ADR-0022). `0xFF02` is in the
/// MLS private-use range, so OpenMLS surfaces it as `Extension::Unknown`.
/// Distinct from ADR-0015's DeviceGroup removal-policy `0xFF01`.
pub(crate) const ROLE_EXTENSION_TYPE: u16 = 0xFF02;

/// Current role-table wire version. An unknown version is **refused**
/// (fail-closed, ADR-0022 Finding 4).
const ROLE_TABLE_VERSION: u8 = 1;

/// Upper bound on the number of admins in a single table — keeps the parser
/// allocation bounded against a hostile GroupContext (ADR-0022).
pub(crate) const MAX_ADMINS: usize = 256;

/// Ed25519 signature public key length.
const PUBKEY_LEN: usize = 32;

/// Offset one past the owner key: `version(1) + owner(32)`.
const OWNER_END: usize = 1 + PUBKEY_LEN;

/// Fixed prefix length: `version(1) + owner(32) + admin_count(2)`.
const HEADER_LEN: usize = OWNER_END + 2;

/// A member's role in a group that has opted into the ADR-0022 role policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The group creator, or whoever they handed the slot on to
    /// ([`RoleTable::with_owner_transferred`]). Exactly one; cannot be removed
    /// or demoted by anyone, including itself; the only role that may change
    /// the role table.
    Owner,
    /// Appointed by the owner. May add / remove non-owner members.
    Admin,
    /// May not add or remove anyone (may still leave — a self-Remove).
    Member,
}

/// What becomes of the **outgoing** owner when ownership is transferred
/// ([`RoleTable::with_owner_transferred`], [`crate::Group::transfer_ownership`]).
///
/// Stated rather than assumed, because either silent default is wrong in one
/// direction: silently keeping admin leaves an unwanted administrator behind in
/// a group the outgoing owner meant to hand over and walk away from, and
/// silently dropping to plain member strips them, in one irreversible commit, of
/// every power they had in a group they created.
///
/// [`OutgoingOwner::Admin`] is the default, and the variant a client should
/// pre-select. Not because it preserves a way back — it does not, and that has
/// to be said plainly: a role change is owner-only, so **a transfer cannot be
/// undone by the person who made it**, whatever they keep. It is the default
/// because it is the recoverable side of an unrecoverable action. Keeping admin
/// leaves the group with one more member who can add, remove and sweep at
/// exactly the moment its leadership is changing hands — which is when
/// ADR-0045's "no owner and no admins online" stall is most likely and most
/// expensive — and the outgoing owner can shed it at any time by asking the new
/// owner to demote them, which is one ordinary role change. The reverse mistake
/// cannot be corrected without the new owner's cooperation either, and costs the
/// group its administrability in the meantime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutgoingOwner {
    /// Keep the outgoing owner in the admin set. The recommended default.
    #[default]
    Admin,
    /// Demote the outgoing owner to a plain member.
    Member,
}

/// A group's admin role table: one owner + a sorted, deduplicated admin set,
/// all keyed on Ed25519 signature public key (ADR-0022).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleTable {
    owner: [u8; PUBKEY_LEN],
    /// Sorted ascending + deduplicated, and never containing `owner` (the
    /// owner is not also listed as an admin — `role_of` returns `Owner` first).
    admins: Vec<[u8; PUBKEY_LEN]>,
}

impl RoleTable {
    /// Build a role table from an owner key + a candidate admin set. The admin
    /// set is canonicalised: sorted ascending, deduplicated, and the owner key
    /// removed if present (it is already the owner). Canonicalisation makes the
    /// encoded bytes deterministic, so every member's GroupContext carries the
    /// identical body.
    #[must_use]
    pub fn new(owner: [u8; PUBKEY_LEN], admins: Vec<[u8; PUBKEY_LEN]>) -> Self {
        let mut admins: Vec<[u8; PUBKEY_LEN]> =
            admins.into_iter().filter(|a| *a != owner).collect();
        admins.sort_unstable();
        admins.dedup();
        admins.truncate(MAX_ADMINS);
        Self { owner, admins }
    }

    /// A table with just the owner and no admins (the state at `Group::create`).
    #[must_use]
    pub fn owner_only(owner: [u8; PUBKEY_LEN]) -> Self {
        Self {
            owner,
            admins: Vec::new(),
        }
    }

    /// The same table with the owner slot moved to `new_owner`, and the
    /// outgoing owner placed per `outgoing` (ADR-0022's deferred
    /// ownership-transfer follow-up).
    ///
    /// Pure, total and **membership-blind**: a role table names keys, not group
    /// members, so it cannot tell whether `new_owner` is in the group. That
    /// question is the whole safety of a transfer — an owner outside the group
    /// is a group nobody can ever administer again, because a role change is
    /// owner-only — and it is answered where the roster is visible:
    /// [`crate::Group::transfer_ownership`] refuses it when authoring, and every
    /// peer's commit gate refuses it on receipt (fail-closed, invariant #4).
    ///
    /// Transfer to the current owner is the identity function for both variants
    /// of `outgoing`, because [`RoleTable::new`] strips the owner key from the
    /// admin set — so the operation is total rather than producing a table that
    /// names one key twice.
    #[must_use]
    pub fn with_owner_transferred(
        &self,
        new_owner: [u8; PUBKEY_LEN],
        outgoing: OutgoingOwner,
    ) -> Self {
        let mut admins = self.admins.clone();
        if outgoing == OutgoingOwner::Admin {
            admins.push(self.owner);
        }
        // `new` re-canonicalises: it strips `new_owner` from the admin set (a
        // promoted admin must not remain listed as one) and re-sorts, so the
        // encoded bytes stay deterministic for every member.
        Self::new(new_owner, admins)
    }

    /// The owner's signature public key.
    #[must_use]
    pub fn owner(&self) -> &[u8; PUBKEY_LEN] {
        &self.owner
    }

    /// The sorted, deduplicated admin signature public keys (never the owner).
    #[must_use]
    pub fn admins(&self) -> &[[u8; PUBKEY_LEN]] {
        &self.admins
    }

    /// The role of a member identified by `pubkey`. A non-32-byte key (or any
    /// key not in the table) is a [`Role::Member`].
    #[must_use]
    pub fn role_of(&self, pubkey: &[u8]) -> Role {
        if pubkey == self.owner {
            return Role::Owner;
        }
        if pubkey.len() == PUBKEY_LEN && self.admins.iter().any(|a| a.as_slice() == pubkey) {
            return Role::Admin;
        }
        Role::Member
    }

    /// `true` when `pubkey` is the owner.
    #[must_use]
    pub fn is_owner(&self, pubkey: &[u8]) -> bool {
        pubkey == self.owner
    }

    /// `true` when `pubkey` may add / remove members — i.e. owner or admin.
    #[must_use]
    pub fn can_administer(&self, pubkey: &[u8]) -> bool {
        matches!(self.role_of(pubkey), Role::Owner | Role::Admin)
    }

    /// Encode to the canonical fixed-layout body (see the module docs).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        // `admins` is capped at MAX_ADMINS (<= u16::MAX) by `new`, so the
        // conversion is always lossless; the saturating fallback is defensive.
        let count = u16::try_from(self.admins.len()).unwrap_or(u16::MAX);
        let mut out = Vec::with_capacity(HEADER_LEN + self.admins.len() * PUBKEY_LEN);
        out.push(ROLE_TABLE_VERSION);
        out.extend_from_slice(&self.owner);
        out.extend_from_slice(&count.to_be_bytes());
        for a in &self.admins {
            out.extend_from_slice(a);
        }
        out
    }

    /// Decode a role-table body. Fail-closed: an unknown `version`, a truncated
    /// body, an `admin_count` over [`MAX_ADMINS`], or a trailing-byte mismatch
    /// is a hard [`Error::Group`], never a partial parse (ADR-0022 Finding 4).
    ///
    /// # Errors
    ///
    /// [`Error::Group`] on any structural or version violation.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Group(format!(
                "role table too short: {} < {HEADER_LEN}",
                bytes.len()
            )));
        }
        let version = bytes[0];
        if version != ROLE_TABLE_VERSION {
            return Err(Error::Group(format!(
                "role table version {version} unsupported (expected {ROLE_TABLE_VERSION})"
            )));
        }
        let mut owner = [0u8; PUBKEY_LEN];
        owner.copy_from_slice(&bytes[1..OWNER_END]);

        let mut count_bytes = [0u8; 2];
        count_bytes.copy_from_slice(&bytes[OWNER_END..HEADER_LEN]);
        let admin_count = u16::from_be_bytes(count_bytes) as usize;
        if admin_count > MAX_ADMINS {
            return Err(Error::Group(format!(
                "role table admin_count {admin_count} exceeds cap {MAX_ADMINS}"
            )));
        }
        let expected_len = HEADER_LEN + admin_count * PUBKEY_LEN;
        if bytes.len() != expected_len {
            return Err(Error::Group(format!(
                "role table length {} != expected {expected_len} for {admin_count} admins",
                bytes.len()
            )));
        }
        // The exact-length check above guarantees `chunks_exact` yields
        // precisely `admin_count` full 32-byte chunks with no remainder.
        let mut admins = Vec::with_capacity(admin_count);
        for chunk in bytes[HEADER_LEN..].chunks_exact(PUBKEY_LEN) {
            let mut a = [0u8; PUBKEY_LEN];
            a.copy_from_slice(chunk);
            admins.push(a);
        }
        // Canonicalise on the way in (sort + dedup + owner-strip) so a
        // non-canonical body still yields correct role lookups.
        Ok(Self::new(owner, admins))
    }
}

/// The ADR-0045 **committer rank** of `pubkey`: `0` for the owner, `k+1` for
/// the `k`-th admin in the table's own canonical order, `None` for anyone else.
///
/// This is the whole of ADR-0045 decision 1. There is no election protocol, no
/// election message and no committer state to hand over: the role table lives in
/// the authenticated `0xFF02` GroupContext extension, so every member holds the
/// identical bytes, and [`RoleTable::new`] sorts and deduplicates the admin set,
/// so every member derives the identical order from them. A pure, total function
/// of shared authenticated state cannot split-brain on *who* rank `k` is.
///
/// It deliberately ranks **signature public keys**, never leaf indices: OpenMLS
/// reuses a vacated leaf for the next Add, so a leaf-keyed committer identity
/// would silently change hands on every membership change — the exact reason
/// ADR-0022 keyed the role table on the signature key in the first place. It is
/// therefore *not* `lakko_calls::link_invite::elect_committer`, which ranks by
/// leaf index and needs an SFU presence oracle a conversation group does not
/// have; the two coexist by design and neither subsumes the other.
///
/// Rank orders the authorised committers; it does not widen them. The set that
/// may commit the **whole** pending set is exactly ADR-0022's owner-plus-admins,
/// unchanged. ADR-0050 section 2.5a ranks the remaining member accounts after
/// them for device admissions only — see [`account_committer_rank`].
#[must_use]
pub fn committer_rank(table: &RoleTable, pubkey: &[u8]) -> Option<u32> {
    if table.is_owner(pubkey) {
        return Some(0);
    }
    if pubkey.len() != PUBKEY_LEN {
        return None;
    }
    let position = table.admins().iter().position(|a| a.as_slice() == pubkey)?;
    // `admins` is capped at MAX_ADMINS (256), so neither conversion nor the
    // increment can overflow; both are written fallibly rather than asserted.
    u32::try_from(position).ok()?.checked_add(1)
}

/// The ADR-0050 section 2.5a **account rank** of `account`: the owner and the
/// admins exactly as [`committer_rank`] ranks them, and every other member
/// account `n + 1 + i`, where `n` is the number of admins in the table and `i`
/// is the account's position among the **non-admin** member accounts of
/// `roster_accounts`, sorted bytewise and deduplicated.
///
/// `roster_accounts` is the account key of every leaf in the epoch's roster
/// (one entry per leaf, so an account with several devices appears several
/// times; duplicates collapse). Entries that are not 32 bytes long — a leaf
/// whose credential resolved to nothing — are ignored, and so is any account
/// the table already ranks as owner or admin.
///
/// Like [`committer_rank`] this is a pure, total function of state every
/// member holds identically (the authenticated role table and the roster at
/// one epoch), so every peer derives the same rank for the same account; and
/// like it, it ranks **account keys, never leaf indices** (C-68), so reusing a
/// vacated leaf renumbers nobody. Ranks are a permutation: owner `0`, admins
/// `1..=n`, member accounts `n + 1 ..= n + m`, no two accounts sharing one.
///
/// **Having a rank is not permission to commit.** A member account's rank only
/// orders the members that may sweep **device admissions** (section 2.5a);
/// which proposals a sweeper may carry is decided by
/// `Group::stage_pending_proposals`, not here. `None` for an account that is
/// neither in the table nor in the roster, or a wrong-length key.
#[must_use]
pub fn account_committer_rank<'a, I>(
    table: &RoleTable,
    roster_accounts: I,
    account: &[u8],
) -> Option<u32>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    if let Some(rank) = committer_rank(table, account) {
        return Some(rank);
    }
    if account.len() != PUBKEY_LEN {
        return None;
    }
    let mut plain: Vec<&[u8]> = roster_accounts
        .into_iter()
        .filter(|a| a.len() == PUBKEY_LEN && committer_rank(table, a).is_none())
        .collect();
    plain.sort_unstable();
    plain.dedup();
    let position = plain.iter().position(|a| *a == account)?;
    // Admins are capped at MAX_ADMINS and member accounts by the roster size,
    // so these cannot overflow; written fallibly rather than asserted.
    u32::try_from(table.admins().len())
        .ok()?
        .checked_add(1)?
        .checked_add(u32::try_from(position).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(b: u8) -> [u8; PUBKEY_LEN] {
        [b; PUBKEY_LEN]
    }

    #[test]
    fn round_trip_owner_only() {
        let t = RoleTable::owner_only(pk(1));
        let bytes = t.encode();
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(RoleTable::decode(&bytes).unwrap(), t);
    }

    #[test]
    fn round_trip_with_admins() {
        let t = RoleTable::new(pk(9), vec![pk(3), pk(1), pk(2)]);
        let bytes = t.encode();
        let back = RoleTable::decode(&bytes).unwrap();
        assert_eq!(back, t);
        // canonical: sorted ascending
        assert_eq!(back.admins(), &[pk(1), pk(2), pk(3)]);
    }

    #[test]
    fn new_canonicalises_sort_dedup_and_strips_owner() {
        let t = RoleTable::new(pk(5), vec![pk(2), pk(2), pk(5), pk(1)]);
        // pk(5) (owner) stripped, pk(2) deduped, sorted
        assert_eq!(t.admins(), &[pk(1), pk(2)]);
    }

    #[test]
    fn transfer_moves_the_owner_slot_and_keeps_the_outgoing_owner_as_admin() {
        let t = RoleTable::new(pk(9), vec![pk(1), pk(2)]);
        let after = t.with_owner_transferred(pk(1), OutgoingOwner::Admin);
        assert_eq!(after.owner(), &pk(1), "the named member is now the owner");
        assert_eq!(
            after.admins(),
            &[pk(2), pk(9)],
            "the promoted admin leaves the admin set; the outgoing owner joins it",
        );
        assert_eq!(after.role_of(&pk(1)), Role::Owner);
        assert_eq!(after.role_of(&pk(9)), Role::Admin);
        assert_eq!(after.role_of(&pk(2)), Role::Admin);
    }

    #[test]
    fn transfer_can_drop_the_outgoing_owner_to_a_plain_member() {
        let t = RoleTable::new(pk(9), vec![pk(1), pk(2)]);
        let after = t.with_owner_transferred(pk(1), OutgoingOwner::Member);
        assert_eq!(after.owner(), &pk(1));
        assert_eq!(after.admins(), &[pk(2)]);
        assert_eq!(
            after.role_of(&pk(9)),
            Role::Member,
            "the outgoing owner keeps nothing",
        );
    }

    #[test]
    fn transfer_to_a_plain_member_promotes_only_them() {
        let t = RoleTable::new(pk(9), vec![pk(2)]);
        let after = t.with_owner_transferred(pk(7), OutgoingOwner::Admin);
        assert_eq!(after.owner(), &pk(7));
        assert_eq!(after.admins(), &[pk(2), pk(9)]);
    }

    #[test]
    fn transfer_to_the_current_owner_is_the_identity_for_both_variants() {
        // Total rather than producing a table that names one key twice.
        let t = RoleTable::new(pk(9), vec![pk(1), pk(2)]);
        assert_eq!(t.with_owner_transferred(pk(9), OutgoingOwner::Admin), t);
        assert_eq!(t.with_owner_transferred(pk(9), OutgoingOwner::Member), t);
    }

    #[test]
    fn a_transferred_table_round_trips_through_the_codec() {
        // The transfer is only real once it is in the authenticated
        // GroupContext, so the canonical encoding has to survive it.
        let t = RoleTable::new(pk(9), vec![pk(1), pk(2)]);
        let after = t.with_owner_transferred(pk(1), OutgoingOwner::Admin);
        let back = RoleTable::decode(&after.encode()).expect("decode");
        assert_eq!(back, after);
        assert_eq!(
            committer_rank(&back, &pk(1)),
            Some(0),
            "the new owner leads"
        );
        assert_eq!(committer_rank(&back, &pk(9)), Some(2), "pk(2) sorts first");
    }

    #[test]
    fn outgoing_owner_defaults_to_admin() {
        assert_eq!(OutgoingOwner::default(), OutgoingOwner::Admin);
    }

    #[test]
    fn role_lookup() {
        let t = RoleTable::new(pk(9), vec![pk(2), pk(3)]);
        assert_eq!(t.role_of(&pk(9)), Role::Owner);
        assert_eq!(t.role_of(&pk(2)), Role::Admin);
        assert_eq!(t.role_of(&pk(7)), Role::Member);
        assert!(t.is_owner(&pk(9)));
        assert!(!t.is_owner(&pk(2)));
        assert!(t.can_administer(&pk(9)));
        assert!(t.can_administer(&pk(2)));
        assert!(!t.can_administer(&pk(7)));
    }

    #[test]
    fn role_of_non_32_byte_key_is_member() {
        let t = RoleTable::owner_only(pk(1));
        assert_eq!(t.role_of(&[1u8; 16]), Role::Member);
        assert_eq!(t.role_of(&[]), Role::Member);
    }

    #[test]
    fn decode_rejects_unknown_version() {
        let mut bytes = RoleTable::owner_only(pk(1)).encode();
        bytes[0] = 0xFF;
        assert!(RoleTable::decode(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_truncated_header() {
        assert!(RoleTable::decode(&[]).is_err());
        assert!(RoleTable::decode(&[ROLE_TABLE_VERSION; HEADER_LEN - 1]).is_err());
    }

    #[test]
    fn decode_rejects_length_mismatch() {
        let mut bytes = RoleTable::new(pk(9), vec![pk(1)]).encode();
        bytes.push(0); // one trailing byte
        assert!(RoleTable::decode(&bytes).is_err());
        bytes.truncate(HEADER_LEN + 1); // claims 1 admin, only 1 byte of it
        assert!(RoleTable::decode(&bytes).is_err());
    }

    #[test]
    fn decode_rejects_admin_count_over_cap() {
        // Header claiming u16::MAX admins but no admin bytes — must refuse on
        // the cap (or the length check) before allocating.
        let mut bytes = Vec::new();
        bytes.push(ROLE_TABLE_VERSION);
        bytes.extend_from_slice(&pk(1));
        bytes.extend_from_slice(&u16::MAX.to_be_bytes());
        assert!(RoleTable::decode(&bytes).is_err());
    }

    #[test]
    fn committer_rank_is_owner_first_then_the_tables_own_admin_order() {
        // `new` sorts + dedups, so the admin order is canonical and every
        // member derives it identically from the same GroupContext bytes.
        let t = RoleTable::new(pk(9), vec![pk(3), pk(1), pk(2)]);
        assert_eq!(committer_rank(&t, &pk(9)), Some(0), "owner is rank 0");
        assert_eq!(committer_rank(&t, &pk(1)), Some(1));
        assert_eq!(committer_rank(&t, &pk(2)), Some(2));
        assert_eq!(committer_rank(&t, &pk(3)), Some(3));
        assert_eq!(
            committer_rank(&t, &pk(7)),
            None,
            "a plain member has no rank"
        );
        assert_eq!(committer_rank(&t, &[]), None);
        assert_eq!(committer_rank(&t, &[9u8; 16]), None, "wrong-length key");
    }

    #[test]
    fn committer_rank_is_insertion_order_independent() {
        // The same owner + admin SET in any input order yields the same ranks —
        // the property that makes the duty split-brain-free without negotiation.
        let a = RoleTable::new(pk(9), vec![pk(3), pk(1), pk(2)]);
        let b = RoleTable::new(pk(9), vec![pk(2), pk(3), pk(1), pk(1)]);
        for key in [pk(9), pk(1), pk(2), pk(3), pk(7)] {
            assert_eq!(committer_rank(&a, &key), committer_rank(&b, &key));
        }
    }

    #[test]
    fn account_rank_puts_member_accounts_after_the_admins_in_key_order() {
        // ADR-0050 section 2.5a: owner 0, admins 1..=n, then every other
        // member account in bytewise key order, deduplicated, ignoring the
        // owner/admins and any malformed (non-32-byte) entry.
        let t = RoleTable::new(pk(9), vec![pk(4), pk(2)]);
        let roster: Vec<Vec<u8>> = vec![
            pk(7).to_vec(),
            pk(9).to_vec(), // owner
            pk(5).to_vec(),
            pk(2).to_vec(), // admin
            pk(7).to_vec(), // a second leaf of the same account
            vec![1, 2, 3],  // a leaf that resolved to nothing
            pk(1).to_vec(),
        ];
        let rank = |k: &[u8]| account_committer_rank(&t, roster.iter().map(Vec::as_slice), k);
        assert_eq!(rank(&pk(9)), Some(0), "owner");
        assert_eq!(rank(&pk(2)), Some(1), "first admin");
        assert_eq!(
            rank(&pk(4)),
            Some(2),
            "second admin, even if not in the roster"
        );
        assert_eq!(rank(&pk(1)), Some(3), "first member account");
        assert_eq!(rank(&pk(5)), Some(4));
        assert_eq!(rank(&pk(7)), Some(5), "two leaves, one rank");
        assert_eq!(rank(&pk(8)), None, "not in the roster");
        assert_eq!(rank(&[1, 2, 3]), None, "wrong length");

        // Permutation: the ranks of the distinct accounts are 0..=5, no gaps.
        let mut ranks: Vec<u32> = [pk(9), pk(2), pk(4), pk(1), pk(5), pk(7)]
            .iter()
            .filter_map(|k| rank(k))
            .collect();
        ranks.sort_unstable();
        assert_eq!(ranks, (0..=5).collect::<Vec<_>>());

        // Independent of the order the roster is listed in.
        let reversed: Vec<&[u8]> = roster.iter().rev().map(Vec::as_slice).collect();
        for k in [pk(1), pk(5), pk(7)] {
            assert_eq!(
                account_committer_rank(&t, reversed.iter().copied(), &k),
                rank(&k)
            );
        }
    }

    #[test]
    fn decode_accepts_max_admins() {
        // 256 unique admin keys (each differs in byte 0), owner distinct.
        let admins: Vec<[u8; PUBKEY_LEN]> = (0..MAX_ADMINS)
            .map(|i| {
                let mut a = [0u8; PUBKEY_LEN];
                a[0] = u8::try_from(i).unwrap_or(0);
                a[1] = 0xAA;
                a
            })
            .collect();
        let t = RoleTable::new(pk(0xFF), admins);
        assert_eq!(t.admins().len(), MAX_ADMINS);
        let bytes = t.encode();
        assert_eq!(RoleTable::decode(&bytes).unwrap(), t);
    }
}
