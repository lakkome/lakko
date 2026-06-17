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
    /// The group creator. Exactly one; cannot be removed or demoted by others;
    /// the only role that may change the role table.
    Owner,
    /// Appointed by the owner. May add / remove non-owner members.
    Admin,
    /// May not add or remove anyone (may still leave — a self-Remove).
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
