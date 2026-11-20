// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The blob pass (ADR-0065 section 5): one anonymous token buys an hour's
//! pass with a budget, so a person does not spend a token per file.
//!
//! # Wire
//!
//! The pass travels in the `x-lakko-blob-pass` request header:
//!
//! ```text
//! p1.<base64url-no-pad(kind(1) || pass_id(16) || expires_at(8 BE) || tag(32))>
//! ```
//!
//! 57 bytes, 76 base64url characters, 79 characters with the prefix.
//! `kind` is `1` (upload) or `2` (read); `expires_at` is unix seconds;
//! `tag = HMAC-SHA-256(secret, "lakko/storage/pass/v1" || 0x00 || kind ||
//! pass_id || expires_at)`. The pass names no account. The service verifies
//! it without a lookup.
//!
//! # Budgets
//!
//! What a pass has spent is counted against its id in memory for the pass
//! lifetime ([`BudgetLedger`]). Two consequences, stated: a restart forgets
//! the counts (a pass then spends its budget again), and several instances
//! each count separately (each grants the whole budget). Both are within the
//! "room for heavy use" the budgets are chosen with, and neither lets a pass
//! outlive its expiry.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use subtle::ConstantTimeEq as _;

/// The request header a pass travels in.
pub(crate) const PASS_HEADER: &str = "x-lakko-blob-pass";

/// The secret's environment variable: 64 hex characters.
pub(crate) const PASS_SECRET_VAR: &str = "LAKKO_STORAGE_PASS_SECRET";

const PREFIX: &str = "p1.";
const DOMAIN: &[u8] = b"lakko/storage/pass/v1";
const ID_LEN: usize = 16;
const TAG_LEN: usize = 32;
const RAW_LEN: usize = 1 + ID_LEN + 8 + TAG_LEN;

/// What a pass pays for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PassKind {
    /// `PUT /blob/{hex}`.
    Upload,
    /// `GET` / `HEAD /blob/{hex}`, `POST /blob/keep`.
    Read,
}

impl PassKind {
    const fn byte(self) -> u8 {
        match self {
            Self::Upload => 1,
            Self::Read => 2,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Read => "read",
        }
    }
}

/// A verified pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pass {
    pub(crate) kind: PassKind,
    pub(crate) id: [u8; ID_LEN],
    pub(crate) expires_at: u64,
}

/// Why a pass was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassError {
    /// Not a pass of this format, a wrong tag, or a pass of the other kind.
    Invalid,
    /// A genuine pass past its expiry.
    Expired,
}

/// The service secret passes are made and checked with.
pub(crate) struct PassSecret([u8; 32]);

impl PassSecret {
    /// From 64 hex characters.
    pub(crate) fn from_hex(s: &str) -> Option<Self> {
        let v = hex::decode(s.trim()).ok()?;
        <[u8; 32]>::try_from(v.as_slice()).ok().map(Self)
    }

    #[cfg(test)]
    pub(crate) const fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    fn tag(&self, kind: u8, id: &[u8; ID_LEN], expires_at: u64) -> [u8; TAG_LEN] {
        let mut msg = Vec::with_capacity(DOMAIN.len() + 1 + 1 + ID_LEN + 8);
        msg.extend_from_slice(DOMAIN);
        msg.push(0);
        msg.push(kind);
        msg.extend_from_slice(id);
        msg.extend_from_slice(&expires_at.to_be_bytes());
        lakko_crypto::hash::hmac_sha256(&self.0, &msg)
    }

    /// Make a fresh pass of `kind` expiring at `expires_at`.
    pub(crate) fn issue(&self, kind: PassKind, expires_at: u64) -> (Pass, String) {
        let mut id = [0u8; ID_LEN];
        lakko_crypto::random::fill(&mut id);
        let pass = Pass {
            kind,
            id,
            expires_at,
        };
        (pass, self.encode(&pass))
    }

    fn encode(&self, pass: &Pass) -> String {
        let mut raw = Vec::with_capacity(RAW_LEN);
        raw.push(pass.kind.byte());
        raw.extend_from_slice(&pass.id);
        raw.extend_from_slice(&pass.expires_at.to_be_bytes());
        raw.extend_from_slice(&self.tag(pass.kind.byte(), &pass.id, pass.expires_at));
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(raw))
    }

    /// Verify `value` as a pass of `kind` at `now_secs`: shape, tag (in
    /// constant time), kind, then expiry.
    pub(crate) fn verify(
        &self,
        value: &str,
        kind: PassKind,
        now_secs: u64,
    ) -> Result<Pass, PassError> {
        let body = value.strip_prefix(PREFIX).ok_or(PassError::Invalid)?;
        if body.len() > 80 {
            return Err(PassError::Invalid);
        }
        let raw = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| PassError::Invalid)?;
        if raw.len() != RAW_LEN {
            return Err(PassError::Invalid);
        }
        let kind_byte = raw[0];
        let id: [u8; ID_LEN] = raw[1..=ID_LEN].try_into().map_err(|_| PassError::Invalid)?;
        let exp: [u8; 8] = raw[1 + ID_LEN..1 + ID_LEN + 8]
            .try_into()
            .map_err(|_| PassError::Invalid)?;
        let expires_at = u64::from_be_bytes(exp);
        let tag = &raw[1 + ID_LEN + 8..];
        let expected = self.tag(kind_byte, &id, expires_at);
        if !bool::from(expected.ct_eq(tag)) {
            return Err(PassError::Invalid);
        }
        if kind_byte != kind.byte() {
            return Err(PassError::Invalid);
        }
        if now_secs >= expires_at {
            return Err(PassError::Expired);
        }
        Ok(Pass {
            kind,
            id,
            expires_at,
        })
    }
}

/// The lifetime and the two budgets of a pass. Every default is chosen, not
/// measured (ADR-0065 section 5), with room for heavy media use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PassLimits {
    /// Pass lifetime in seconds. Default 3600 (the ADR's hour).
    pub(crate) ttl_secs: u64,
    /// Uploads per upload pass. Default 300.
    pub(crate) upload_files: u64,
    /// Bytes per upload pass. Default 2 GiB (forty 50 MiB videos).
    pub(crate) upload_bytes: u64,
    /// Downloads (`GET` and `HEAD`) per read pass. Default 3000.
    pub(crate) read_downloads: u64,
    /// Blob ids kept per read pass. Default 5000.
    pub(crate) read_keeps: u64,
    /// Free space an upload must leave on the blob filesystem
    /// (`crate::disk`); 0 = no guard. Default 10 GiB.
    pub(crate) min_free_bytes: u64,
}

impl Default for PassLimits {
    fn default() -> Self {
        Self {
            ttl_secs: 3_600,
            upload_files: 300,
            upload_bytes: 2 * 1024 * 1024 * 1024,
            read_downloads: 3_000,
            read_keeps: 5_000,
            min_free_bytes: crate::disk::DEFAULT_MIN_FREE_BYTES,
        }
    }
}

impl PassLimits {
    /// The two budget ceilings of `kind`.
    pub(crate) const fn ceilings(&self, kind: PassKind) -> (u64, u64) {
        match kind {
            PassKind::Upload => (self.upload_files, self.upload_bytes),
            PassKind::Read => (self.read_downloads, self.read_keeps),
        }
    }
}

/// Spent counters per live pass id.
#[derive(Default)]
pub(crate) struct BudgetLedger {
    spent: Mutex<HashMap<[u8; ID_LEN], (u64, u64, u64)>>,
}

/// Every this many new pass ids the ledger drops expired entries.
const PRUNE_EVERY: usize = 256;

impl BudgetLedger {
    /// Charge `(a, b)` against `pass` (files and bytes for an upload pass,
    /// downloads and keeps for a read pass) under `limits`. All or nothing:
    /// a charge that would cross either ceiling charges nothing.
    pub(crate) fn charge(
        &self,
        pass: &Pass,
        a: u64,
        b: u64,
        limits: &PassLimits,
        now_secs: u64,
    ) -> Result<(), ()> {
        let (max_a, max_b) = limits.ceilings(pass.kind);
        let mut g = self
            .spent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !g.contains_key(&pass.id) && g.len() % PRUNE_EVERY == 0 {
            g.retain(|_, (_, _, exp)| *exp > now_secs);
        }
        let entry = g.entry(pass.id).or_insert((0, 0, pass.expires_at));
        let next_a = entry.0.saturating_add(a);
        let next_b = entry.1.saturating_add(b);
        if next_a > max_a || next_b > max_b {
            return Err(());
        }
        entry.0 = next_a;
        entry.1 = next_b;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> PassSecret {
        PassSecret::from_bytes([5; 32])
    }

    #[test]
    fn a_pass_verifies_for_its_kind_until_it_expires() {
        let s = secret();
        let (pass, value) = s.issue(PassKind::Read, 1_000);
        assert_eq!(value.len(), 79);
        assert_eq!(s.verify(&value, PassKind::Read, 999), Ok(pass));
        assert_eq!(
            s.verify(&value, PassKind::Read, 1_000),
            Err(PassError::Expired)
        );
        assert_eq!(
            s.verify(&value, PassKind::Upload, 10),
            Err(PassError::Invalid)
        );
    }

    #[test]
    fn a_pass_under_another_secret_or_altered_is_invalid() {
        let (_, value) = secret().issue(PassKind::Upload, 1_000);
        let other = PassSecret::from_bytes([6; 32]);
        assert_eq!(
            other.verify(&value, PassKind::Upload, 1),
            Err(PassError::Invalid)
        );
        // Moving the expiry forward breaks the tag.
        let raw = URL_SAFE_NO_PAD.decode(&value[3..]).unwrap();
        let mut raw = raw;
        raw[1 + ID_LEN + 7] ^= 1;
        let forged = format!("p1.{}", URL_SAFE_NO_PAD.encode(&raw));
        assert_eq!(
            secret().verify(&forged, PassKind::Upload, 1),
            Err(PassError::Invalid)
        );
        for junk in ["", "p1.", "p2.AAAA", "p1.!!!!", &"p1.".repeat(40)] {
            assert_eq!(
                secret().verify(junk, PassKind::Upload, 1),
                Err(PassError::Invalid)
            );
        }
    }

    #[test]
    fn two_passes_have_different_ids() {
        let (a, _) = secret().issue(PassKind::Read, 1);
        let (b, _) = secret().issue(PassKind::Read, 1);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn the_ledger_charges_all_or_nothing_per_pass() {
        let limits = PassLimits {
            upload_files: 2,
            upload_bytes: 100,
            ..PassLimits::default()
        };
        let ledger = BudgetLedger::default();
        let (pass, _) = secret().issue(PassKind::Upload, 10);
        assert!(ledger.charge(&pass, 1, 60, &limits, 0).is_ok());
        // Bytes would cross: nothing charged, the file count stays at 1.
        assert!(ledger.charge(&pass, 1, 50, &limits, 0).is_err());
        assert!(ledger.charge(&pass, 1, 40, &limits, 0).is_ok());
        assert!(ledger.charge(&pass, 1, 0, &limits, 0).is_err());
        // Another pass has its own budget.
        let (other, _) = secret().issue(PassKind::Upload, 10);
        assert!(ledger.charge(&other, 2, 100, &limits, 0).is_ok());
    }

    #[test]
    fn the_secret_parses_from_64_hex_only() {
        assert!(PassSecret::from_hex(&"ab".repeat(32)).is_some());
        assert!(PassSecret::from_hex(&"ab".repeat(31)).is_none());
        assert!(PassSecret::from_hex(&"zz".repeat(32)).is_none());
    }
}
