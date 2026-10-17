// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The link-record register ([ADR-0079](../../../../docs/adr/0079-invite-link-makes-contacts.md)
//! section 4): the account's own invite links, one register per link, kept in
//! step across the account's devices so that any of them can recognise a
//! request sent through one of its links.
//!
//! ## The register
//!
//! Keyed by the link's 16-byte id. The value is what a device needs to decide
//! about a request ([`LinkRecordEntry::verdict`]) and to show the link again:
//! the link secret, the kind, the ledger id, the expiry, the use limit, the
//! uses so far, the revoked flag, the label, when it was minted, and the keys
//! of the requesters already accepted through it.
//!
//! It is last-writer-wins per entry on the stamp, with one exception that
//! makes a revocation final: **a revoked entry beats a live one whatever the
//! stamps** ([`super::state::SyncedState`] orders link records by
//! `(revoked, stamp, canonical bytes)`). That order is total, so the fold
//! stays commutative, associative and idempotent, and an entry revoked on
//! one device can never come back live.
//!
//! Two devices that accept two different requests through one multi-use link
//! at the same time both write `uses + 1`; one write wins, so the count can
//! fall one short and the link can serve one more request than its limit.
//! ADR-0079 accepts that: the limit is a courtesy.
//!
//! ## Wire
//!
//! Like the lineage register it rides the DeviceGroup's opaque
//! `ServiceEnvelope` carrier, so `lakko-mls` and `devicegroup_v1.proto` do not
//! change:
//!
//! ```text
//! domain(26) "lakko/sync/link-record/v1\0"
//! version(1) = 0x01
//! count(2, BE)              1..=LINK_RECORD_ENVELOPE_MAX_ENTRIES
//! count × {
//!     hlc(8, BE)            the register's ORIGINAL stamp
//!     body                  see `LinkRecordEntry::encode_body`
//! }
//! ```
//!
//! Canonical: entries are strictly ascending by link id and nothing may
//! trail. Whatever decodes re-encodes to the identical bytes; every violation
//! refuses the whole envelope.
//!
//! ## Trust
//!
//! The bytes arrive from another device of the same account inside an MLS
//! application message of the DeviceGroup, or from a backup. The decoder is
//! bounded and total on arbitrary input. The link secret inside is SECRET:
//! it never reaches a log (the `Debug` output omits it).

use lakko_contacts::invite::LINK_ID_LEN;
use lakko_contacts::link_seal::{link_secrets_match, LINK_SECRET_LEN};
use thiserror::Error;

use super::entry::{take_n, take_u32, take_u64, take_u8, BoundsError, CodecError, Stamped};

/// Domain separator of the link-record envelope, trailing NUL included.
/// Frozen.
pub const LINK_RECORD_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/link-record/v1\0";

/// The version byte this build writes and accepts.
pub const LINK_RECORD_ENVELOPE_VERSION_V1: u8 = 0x01;

/// The most registers one envelope carries. A state holding more is sent as
/// several envelopes.
pub const LINK_RECORD_ENVELOPE_MAX_ENTRIES: usize = 128;

/// Length of a link id.
pub const LINK_RECORD_ID_LEN: usize = LINK_ID_LEN;

/// Length of a link secret.
pub const LINK_RECORD_SECRET_LEN: usize = LINK_SECRET_LEN;

/// Length of a discovery-ledger id (`lakko_contacts::invite::BLOB_ID_LEN`).
pub const LINK_RECORD_LEDGER_ID_LEN: usize = lakko_contacts::invite::BLOB_ID_LEN;

/// Longest label (`lakko_contacts::invite::MAX_LABEL_LEN`).
pub const LINK_RECORD_LABEL_MAX_BYTES: usize = lakko_contacts::invite::MAX_LABEL_LEN;

/// Most requester keys one record remembers. The newest are kept; an older
/// requester that sends again after falling off the list is answered by the
/// contact check every client runs first.
pub const LINK_RECORD_ANSWERED_MAX: usize = 16;

/// Length of a requester key (an Ed25519 account key).
pub const LINK_RECORD_REQUESTER_KEY_LEN: usize = 32;

/// `max_uses` value meaning "no limit" (the `/i/` capability's sentinel).
pub const LINK_RECORD_UNLIMITED: u32 = u32::MAX;

/// Longest encoded body of one entry.
pub const LINK_RECORD_BODY_MAX_LEN: usize = LINK_RECORD_ID_LEN
    + 1
    + LINK_RECORD_SECRET_LEN
    + 1
    + 1
    + LINK_RECORD_LEDGER_ID_LEN
    + 8
    + 4
    + 4
    + 8
    + 1
    + LINK_RECORD_LABEL_MAX_BYTES
    + 1
    + LINK_RECORD_ANSWERED_MAX * LINK_RECORD_REQUESTER_KEY_LEN;

/// Shortest valid envelope: the header and one entry with no ledger id, no
/// label and no requester.
pub const LINK_RECORD_ENVELOPE_MIN_LEN: usize =
    LINK_RECORD_ENVELOPE_DOMAIN_V1.len() + 1 + 2 + 8 + LINK_RECORD_BODY_MAX_LEN
        - LINK_RECORD_LEDGER_ID_LEN
        - LINK_RECORD_LABEL_MAX_BYTES
        - LINK_RECORD_ANSWERED_MAX * LINK_RECORD_REQUESTER_KEY_LEN;

/// Longest valid envelope. Below the DeviceGroup service-envelope cap.
pub const LINK_RECORD_ENVELOPE_MAX_LEN: usize = LINK_RECORD_ENVELOPE_DOMAIN_V1.len()
    + 1
    + 2
    + LINK_RECORD_ENVELOPE_MAX_ENTRIES * (8 + LINK_RECORD_BODY_MAX_LEN);

/// Which ledger a link lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkRecordKind {
    /// A contact invite link (`/i/`).
    Contact,
    /// A call invite link (`/c/`).
    Call,
}

impl LinkRecordKind {
    /// The byte on the wire and in the canonical encoding. Frozen.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Contact => 1,
            Self::Call => 2,
        }
    }

    /// Read the wire byte back.
    ///
    /// # Errors
    ///
    /// [`BoundsError::LinkRecordKind`] for any other byte.
    pub const fn from_wire(b: u8) -> Result<Self, BoundsError> {
        match b {
            1 => Ok(Self::Contact),
            2 => Ok(Self::Call),
            _ => Err(BoundsError::LinkRecordKind),
        }
    }
}

/// What a device decides about a contact request that carries a link answer
/// (ADR-0079 section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkAnswerVerdict {
    /// The answer names a live contact link of this account and carries its
    /// secret: accept the request without asking.
    Accept,
    /// The requester was already accepted through this link (by this device
    /// or another): the request is answered, show nothing.
    Answered,
    /// The answer names no live link (unknown, wrong secret, revoked,
    /// expired, used up, or a call link): show an ordinary request card
    /// with the "no longer works" note.
    Dead,
}

impl LinkAnswerVerdict {
    /// Stable, language-neutral name for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Answered => "answered",
            Self::Dead => "dead",
        }
    }
}

/// One invite link of the account (ADR-0079 section 4).
#[derive(Clone, PartialEq, Eq)]
pub struct LinkRecordEntry {
    /// The link's id: the register key and the link grant's scope.
    pub link_id: [u8; LINK_RECORD_ID_LEN],
    /// The link's secret (the URL fragment). SECRET.
    pub link_secret: [u8; LINK_RECORD_SECRET_LEN],
    /// Which ledger the link lives in.
    pub kind: LinkRecordKind,
    /// The ledger id, empty for a link that has no ledger row (an inline
    /// link).
    pub ledger_id: Vec<u8>,
    /// UNIX millis after which the link no longer works; 0 = never.
    pub expiry_ms: u64,
    /// The use limit as the capability carries it: 0 = single use,
    /// [`LINK_RECORD_UNLIMITED`] = no limit, else the number.
    pub max_uses: u32,
    /// Requests accepted through the link so far.
    pub uses: u32,
    /// Revoked. Final: a revoked entry beats any live one.
    pub revoked: bool,
    /// UNIX millis at which the link was minted.
    pub minted_at_ms: u64,
    /// The owner's label (at most [`LINK_RECORD_LABEL_MAX_BYTES`]), or empty.
    pub label: String,
    /// The account keys of the requesters accepted through the link, oldest
    /// first, each once, at most [`LINK_RECORD_ANSWERED_MAX`].
    pub answered: Vec<[u8; LINK_RECORD_REQUESTER_KEY_LEN]>,
}

impl core::fmt::Debug for LinkRecordEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The link id and the secret are left out: either opens the link.
        f.debug_struct("LinkRecordEntry")
            .field("kind", &self.kind)
            .field("expiry_ms", &self.expiry_ms)
            .field("max_uses", &self.max_uses)
            .field("uses", &self.uses)
            .field("revoked", &self.revoked)
            .field("answered", &self.answered.len())
            .finish_non_exhaustive()
    }
}

impl LinkRecordEntry {
    /// Check the bounds a decoder checks.
    ///
    /// # Errors
    ///
    /// [`BoundsError::LinkRecordLedgerIdLength`],
    /// [`BoundsError::LinkRecordLabel`] or [`BoundsError::LinkRecordAnswered`].
    pub fn validate(&self) -> Result<(), BoundsError> {
        if !self.ledger_id.is_empty() && self.ledger_id.len() != LINK_RECORD_LEDGER_ID_LEN {
            return Err(BoundsError::LinkRecordLedgerIdLength);
        }
        if self.label.len() > LINK_RECORD_LABEL_MAX_BYTES {
            return Err(BoundsError::LinkRecordLabel);
        }
        if self.answered.len() > LINK_RECORD_ANSWERED_MAX {
            return Err(BoundsError::LinkRecordAnswered);
        }
        let mut sorted = self.answered.clone();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1]) {
            return Err(BoundsError::LinkRecordAnswered);
        }
        Ok(())
    }

    /// How many requests the link may serve; `None` for no limit.
    #[must_use]
    pub const fn effective_limit(&self) -> Option<u32> {
        match self.max_uses {
            0 => Some(1),
            LINK_RECORD_UNLIMITED => None,
            n => Some(n),
        }
    }

    /// `true` when the link still works at `now_ms`: not revoked, not
    /// expired, and uses left.
    #[must_use]
    pub const fn is_live(&self, now_ms: u64) -> bool {
        if self.revoked {
            return false;
        }
        if self.expiry_ms != 0 && now_ms >= self.expiry_ms {
            return false;
        }
        match self.effective_limit() {
            Some(limit) => self.uses < limit,
            None => true,
        }
    }

    /// The one rule (ADR-0079 section 3, UX section 6) for a request whose
    /// link answer names this record. The secret is compared in constant
    /// time; nothing but a matching secret on a live contact link accepts.
    #[must_use]
    pub fn verdict(
        &self,
        link_secret: &[u8; LINK_RECORD_SECRET_LEN],
        requester_key: &[u8; LINK_RECORD_REQUESTER_KEY_LEN],
        now_ms: u64,
    ) -> LinkAnswerVerdict {
        if !link_secrets_match(&self.link_secret, link_secret) {
            return LinkAnswerVerdict::Dead;
        }
        if self.kind != LinkRecordKind::Contact {
            return LinkAnswerVerdict::Dead;
        }
        if self.answered.contains(requester_key) {
            return LinkAnswerVerdict::Answered;
        }
        if self.is_live(now_ms) {
            LinkAnswerVerdict::Accept
        } else {
            LinkAnswerVerdict::Dead
        }
    }

    /// This record after one more accepted request from `requester_key`:
    /// `uses + 1` (saturating), the key appended (once; the oldest dropped
    /// past [`LINK_RECORD_ANSWERED_MAX`]).
    #[must_use]
    pub fn with_use(&self, requester_key: &[u8; LINK_RECORD_REQUESTER_KEY_LEN]) -> Self {
        let mut next = self.clone();
        next.uses = next.uses.saturating_add(1);
        next.answered.retain(|k| k != requester_key);
        next.answered.push(*requester_key);
        while next.answered.len() > LINK_RECORD_ANSWERED_MAX {
            next.answered.remove(0);
        }
        next
    }

    /// The body: everything but the stamp. `revoked` follows the link id so
    /// the canonical bytes of the two states of one register differ there
    /// first.
    pub(crate) fn encode_body(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.link_id);
        out.push(u8::from(self.revoked));
        out.extend_from_slice(&self.link_secret);
        out.push(self.kind.to_wire());
        // Validated: 0 or 6 bytes, a label of at most 64, at most 16 keys.
        out.push(u8::try_from(self.ledger_id.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(&self.ledger_id);
        out.extend_from_slice(&self.expiry_ms.to_be_bytes());
        out.extend_from_slice(&self.max_uses.to_be_bytes());
        out.extend_from_slice(&self.uses.to_be_bytes());
        out.extend_from_slice(&self.minted_at_ms.to_be_bytes());
        out.push(u8::try_from(self.label.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(self.label.as_bytes());
        out.push(u8::try_from(self.answered.len()).unwrap_or(u8::MAX));
        for k in &self.answered {
            out.extend_from_slice(k);
        }
    }

    /// Read a body back, every length checked against its bound before the
    /// bytes are taken, then [`Self::validate`].
    pub(crate) fn take_body(cur: &mut &[u8]) -> Result<Self, CodecError> {
        let mut link_id = [0u8; LINK_RECORD_ID_LEN];
        link_id.copy_from_slice(take_n(cur, LINK_RECORD_ID_LEN)?);
        let revoked = match take_u8(cur)? {
            0 => false,
            1 => true,
            _ => return Err(CodecError::BadBool),
        };
        let mut link_secret = [0u8; LINK_RECORD_SECRET_LEN];
        link_secret.copy_from_slice(take_n(cur, LINK_RECORD_SECRET_LEN)?);
        let kind = LinkRecordKind::from_wire(take_u8(cur)?)?;
        let ledger_len = usize::from(take_u8(cur)?);
        if ledger_len != 0 && ledger_len != LINK_RECORD_LEDGER_ID_LEN {
            return Err(BoundsError::LinkRecordLedgerIdLength.into());
        }
        let ledger_id = take_n(cur, ledger_len)?.to_vec();
        let expiry_ms = take_u64(cur)?;
        let max_uses = take_u32(cur)?;
        let uses = take_u32(cur)?;
        let minted_at_ms = take_u64(cur)?;
        let label_len = usize::from(take_u8(cur)?);
        if label_len > LINK_RECORD_LABEL_MAX_BYTES {
            return Err(BoundsError::LinkRecordLabel.into());
        }
        let label =
            String::from_utf8(take_n(cur, label_len)?.to_vec()).map_err(|_| CodecError::NotUtf8)?;
        let answered_count = usize::from(take_u8(cur)?);
        if answered_count > LINK_RECORD_ANSWERED_MAX {
            return Err(BoundsError::LinkRecordAnswered.into());
        }
        let mut answered = Vec::with_capacity(answered_count);
        for _ in 0..answered_count {
            let mut k = [0u8; LINK_RECORD_REQUESTER_KEY_LEN];
            k.copy_from_slice(take_n(cur, LINK_RECORD_REQUESTER_KEY_LEN)?);
            answered.push(k);
        }
        let entry = Self {
            link_id,
            link_secret,
            kind,
            ledger_id,
            expiry_ms,
            max_uses,
            uses,
            revoked,
            minted_at_ms,
            label,
            answered,
        };
        entry.validate()?;
        Ok(entry)
    }
}

/// Why a link-record envelope was refused. A reason, never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LinkRecordEnvelopeError {
    /// The bytes do not open with [`LINK_RECORD_ENVELOPE_DOMAIN_V1`].
    #[error("not a link-record envelope")]
    WrongDomain,
    /// The version byte is not [`LINK_RECORD_ENVELOPE_VERSION_V1`].
    #[error("unknown link-record envelope version")]
    UnknownVersion,
    /// The bytes ended early.
    #[error("link-record envelope is truncated")]
    Truncated,
    /// Bytes remained after the last entry.
    #[error("trailing bytes in link-record envelope")]
    Trailing,
    /// The entry count is zero or above [`LINK_RECORD_ENVELOPE_MAX_ENTRIES`].
    #[error("link-record envelope entry count is out of bounds")]
    Count,
    /// Entries are not strictly ascending by link id.
    #[error("link-record envelope entries are not in canonical order")]
    NotCanonical,
    /// An entry was malformed or broke a register bound.
    #[error("link-record entry is malformed or out of bounds")]
    Entry,
}

impl LinkRecordEnvelopeError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion => "unknownVersion",
            Self::Truncated => "truncated",
            Self::Trailing => "trailing",
            Self::Count => "count",
            Self::NotCanonical => "notCanonical",
            Self::Entry => "entry",
        }
    }
}

/// `true` when `bytes` open with the link-record envelope domain: the cheap
/// routing question a client asks of a DeviceGroup service envelope. Says
/// nothing about validity.
#[must_use]
pub fn is_link_record_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(LINK_RECORD_ENVELOPE_DOMAIN_V1)
}

/// Compose one envelope from stamped registers. The entries are sorted
/// here, so the caller's order does not matter.
///
/// # Errors
///
/// * [`LinkRecordEnvelopeError::Count`] — none, or more than
///   [`LINK_RECORD_ENVELOPE_MAX_ENTRIES`].
/// * [`LinkRecordEnvelopeError::NotCanonical`] — a link id appears twice.
/// * [`LinkRecordEnvelopeError::Entry`] — an entry breaks a bound.
pub fn compose_link_record_envelope(
    entries: &[Stamped<LinkRecordEntry>],
) -> Result<Vec<u8>, LinkRecordEnvelopeError> {
    if entries.is_empty() || entries.len() > LINK_RECORD_ENVELOPE_MAX_ENTRIES {
        return Err(LinkRecordEnvelopeError::Count);
    }
    let count = u16::try_from(entries.len()).map_err(|_| LinkRecordEnvelopeError::Count)?;
    let mut sorted: Vec<&Stamped<LinkRecordEntry>> = entries.iter().collect();
    sorted.sort_by_key(|e| e.value.link_id);
    let mut out = Vec::with_capacity(LINK_RECORD_ENVELOPE_DOMAIN_V1.len() + 3 + entries.len() * 80);
    out.extend_from_slice(LINK_RECORD_ENVELOPE_DOMAIN_V1);
    out.push(LINK_RECORD_ENVELOPE_VERSION_V1);
    out.extend_from_slice(&count.to_be_bytes());
    let mut previous: Option<&[u8; LINK_RECORD_ID_LEN]> = None;
    for e in sorted {
        e.value
            .validate()
            .map_err(|_| LinkRecordEnvelopeError::Entry)?;
        if previous == Some(&e.value.link_id) {
            return Err(LinkRecordEnvelopeError::NotCanonical);
        }
        previous = Some(&e.value.link_id);
        out.extend_from_slice(&e.hlc.to_be_bytes());
        e.value.encode_body(&mut out);
    }
    Ok(out)
}

/// Decode one envelope, fail-closed: the whole envelope or nothing.
///
/// # Errors
///
/// Every [`LinkRecordEnvelopeError`]; the domain is checked first, then the
/// version, then the count, then each entry.
pub fn decode_link_record_envelope(
    bytes: &[u8],
) -> Result<Vec<Stamped<LinkRecordEntry>>, LinkRecordEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(LINK_RECORD_ENVELOPE_DOMAIN_V1)
        .ok_or(LinkRecordEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| LinkRecordEnvelopeError::Truncated)?;
    if version != LINK_RECORD_ENVELOPE_VERSION_V1 {
        return Err(LinkRecordEnvelopeError::UnknownVersion);
    }
    let count_bytes = take_n(&mut cur, 2).map_err(|_| LinkRecordEnvelopeError::Truncated)?;
    let count = usize::from(u16::from_be_bytes([count_bytes[0], count_bytes[1]]));
    if count == 0 || count > LINK_RECORD_ENVELOPE_MAX_ENTRIES {
        return Err(LinkRecordEnvelopeError::Count);
    }
    // Grown, not pre-sized from the count.
    let mut out: Vec<Stamped<LinkRecordEntry>> = Vec::new();
    for _ in 0..count {
        let hlc = take_u64(&mut cur).map_err(|_| LinkRecordEnvelopeError::Truncated)?;
        let value = LinkRecordEntry::take_body(&mut cur).map_err(|e| match e {
            CodecError::Truncated => LinkRecordEnvelopeError::Truncated,
            _ => LinkRecordEnvelopeError::Entry,
        })?;
        if let Some(last) = out.last() {
            if last.value.link_id >= value.link_id {
                return Err(LinkRecordEnvelopeError::NotCanonical);
            }
        }
        out.push(Stamped { hlc, value });
    }
    if !cur.is_empty() {
        return Err(LinkRecordEnvelopeError::Trailing);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    pub(crate) fn record(id: u8) -> LinkRecordEntry {
        LinkRecordEntry {
            link_id: [id; LINK_RECORD_ID_LEN],
            link_secret: [0x5C; LINK_RECORD_SECRET_LEN],
            kind: LinkRecordKind::Contact,
            ledger_id: vec![7; LINK_RECORD_LEDGER_ID_LEN],
            expiry_ms: 1_000,
            max_uses: 0,
            uses: 0,
            revoked: false,
            minted_at_ms: 10,
            label: "home".into(),
            answered: vec![],
        }
    }

    fn st(hlc: u64, v: LinkRecordEntry) -> Stamped<LinkRecordEntry> {
        Stamped { hlc, value: v }
    }

    #[test]
    fn the_domain_is_pinned() {
        assert_eq!(LINK_RECORD_ENVELOPE_DOMAIN_V1.len(), 26);
        assert_eq!(LINK_RECORD_ENVELOPE_DOMAIN_V1.last(), Some(&0));
        let wire = compose_link_record_envelope(&[st(1, record(1))]).unwrap();
        assert!(is_link_record_envelope(&wire));
        assert!(!is_link_record_envelope(b"lakko/sync/lineage/v1\0"));
        assert!(wire.len() >= LINK_RECORD_ENVELOPE_MIN_LEN);
    }

    #[test]
    fn compose_sorts_and_decode_returns_the_same_entries() {
        let mut full = record(9);
        full.answered = (0u8..16).map(|i| [i; 32]).collect();
        full.label = "x".repeat(LINK_RECORD_LABEL_MAX_BYTES);
        full.revoked = true;
        let mut inline = record(3);
        inline.ledger_id.clear();
        inline.kind = LinkRecordKind::Call;
        let entries = [st(5, full.clone()), st(2, inline.clone())];
        let wire = compose_link_record_envelope(&entries).unwrap();
        let back = decode_link_record_envelope(&wire).unwrap();
        assert_eq!(back, vec![st(2, inline), st(5, full)]);
        assert_eq!(compose_link_record_envelope(&back).unwrap(), wire);
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        assert_eq!(
            compose_link_record_envelope(&[]),
            Err(LinkRecordEnvelopeError::Count)
        );
        assert_eq!(
            compose_link_record_envelope(&[st(1, record(1)), st(2, record(1))]),
            Err(LinkRecordEnvelopeError::NotCanonical)
        );
        let mut bad = record(1);
        bad.ledger_id = vec![1; 5];
        assert_eq!(
            compose_link_record_envelope(&[st(1, bad)]),
            Err(LinkRecordEnvelopeError::Entry)
        );
        let mut dup = record(1);
        dup.answered = vec![[1; 32], [1; 32]];
        assert_eq!(dup.validate(), Err(BoundsError::LinkRecordAnswered));
        let many: Vec<_> = (0..=LINK_RECORD_ENVELOPE_MAX_ENTRIES)
            .map(|i| {
                let mut r = record(0);
                r.link_id[..8].copy_from_slice(&(i as u64).to_be_bytes());
                st(1, r)
            })
            .collect();
        assert_eq!(
            compose_link_record_envelope(&many),
            Err(LinkRecordEnvelopeError::Count)
        );
        let max = compose_link_record_envelope(&many[..LINK_RECORD_ENVELOPE_MAX_ENTRIES]).unwrap();
        assert!(max.len() <= LINK_RECORD_ENVELOPE_MAX_LEN);
    }

    #[test]
    fn decode_is_fail_closed() {
        let good = compose_link_record_envelope(&[st(1, record(1)), st(2, record(2))]).unwrap();
        for len in 0..good.len() {
            assert!(decode_link_record_envelope(&good[..len]).is_err(), "{len}");
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(
            decode_link_record_envelope(&trailing),
            Err(LinkRecordEnvelopeError::Trailing)
        );
        let header = LINK_RECORD_ENVELOPE_DOMAIN_V1.len();
        let mut version = good.clone();
        version[header] = 2;
        assert_eq!(
            decode_link_record_envelope(&version),
            Err(LinkRecordEnvelopeError::UnknownVersion)
        );
        let mut zero = good.clone();
        zero[header + 1] = 0;
        zero[header + 2] = 0;
        assert_eq!(
            decode_link_record_envelope(&zero),
            Err(LinkRecordEnvelopeError::Count)
        );
        // The kind byte sits after hlc(8) + id(16) + revoked(1) + secret(12).
        let kind_at = header + 3 + 8 + 16 + 1 + 12;
        let mut kind = good.clone();
        kind[kind_at] = 9;
        assert_eq!(
            decode_link_record_envelope(&kind),
            Err(LinkRecordEnvelopeError::Entry)
        );
        let mut revoked = good.clone();
        revoked[header + 3 + 8 + 16] = 2;
        assert_eq!(
            decode_link_record_envelope(&revoked),
            Err(LinkRecordEnvelopeError::Entry)
        );
        // Descending ids.
        let swapped = {
            let mut a = compose_link_record_envelope(&[st(1, record(2))]).unwrap();
            let b = compose_link_record_envelope(&[st(1, record(1))]).unwrap();
            a[header + 2] = 2;
            a.extend_from_slice(&b[header + 3..]);
            a
        };
        assert_eq!(
            decode_link_record_envelope(&swapped),
            Err(LinkRecordEnvelopeError::NotCanonical)
        );
    }

    #[test]
    fn the_verdict_follows_the_rule() {
        let r = record(1);
        let key = [0xB0; 32];
        let secret = [0x5C; LINK_RECORD_SECRET_LEN];
        assert_eq!(r.verdict(&secret, &key, 0), LinkAnswerVerdict::Accept);
        // A wrong secret, by one bit.
        let mut wrong = secret;
        wrong[0] ^= 1;
        assert_eq!(r.verdict(&wrong, &key, 0), LinkAnswerVerdict::Dead);
        // Expired.
        assert_eq!(r.verdict(&secret, &key, 1_000), LinkAnswerVerdict::Dead);
        // Revoked.
        let mut revoked = r.clone();
        revoked.revoked = true;
        assert_eq!(revoked.verdict(&secret, &key, 0), LinkAnswerVerdict::Dead);
        // Used up (single use), and the one who used it is answered.
        let used = r.with_use(&key);
        assert_eq!(used.uses, 1);
        assert_eq!(
            used.verdict(&secret, &[0xB1; 32], 0),
            LinkAnswerVerdict::Dead
        );
        assert_eq!(used.verdict(&secret, &key, 0), LinkAnswerVerdict::Answered);
        // A call link never accepts a contact request.
        let mut call = r.clone();
        call.kind = LinkRecordKind::Call;
        assert_eq!(call.verdict(&secret, &key, 0), LinkAnswerVerdict::Dead);
        // Never expiring, unlimited.
        let mut open = r;
        open.expiry_ms = 0;
        open.max_uses = LINK_RECORD_UNLIMITED;
        open.uses = u32::MAX - 1;
        assert_eq!(
            open.verdict(&secret, &key, u64::MAX),
            LinkAnswerVerdict::Accept
        );
        // Three uses.
        let mut three = record(1);
        three.max_uses = 3;
        let three = three.with_use(&[1; 32]).with_use(&[2; 32]);
        assert_eq!(three.verdict(&secret, &key, 0), LinkAnswerVerdict::Accept);
        let three = three.with_use(&[3; 32]);
        assert_eq!(three.verdict(&secret, &key, 0), LinkAnswerVerdict::Dead);
    }

    #[test]
    fn with_use_keeps_the_newest_requesters_once() {
        let mut r = record(1);
        r.max_uses = LINK_RECORD_UNLIMITED;
        for i in 0..20u8 {
            r = r.with_use(&[i; 32]);
        }
        assert_eq!(r.answered.len(), LINK_RECORD_ANSWERED_MAX);
        assert_eq!(r.answered[0], [4; 32]);
        assert_eq!(r.answered[15], [19; 32]);
        let again = r.with_use(&[10; 32]);
        assert_eq!(again.answered.len(), LINK_RECORD_ANSWERED_MAX);
        assert_eq!(again.answered[15], [10; 32]);
        assert!(again.validate().is_ok());
        let shown = format!("{r:?}");
        assert!(!shown.contains("92"), "{shown}");
    }

    fn arb_record() -> impl Strategy<Value = LinkRecordEntry> {
        (
            any::<[u8; LINK_RECORD_ID_LEN]>(),
            any::<[u8; LINK_RECORD_SECRET_LEN]>(),
            prop::sample::select(vec![LinkRecordKind::Contact, LinkRecordKind::Call]),
            any::<u64>(),
            prop::sample::select(vec![0u32, 1, 3, LINK_RECORD_UNLIMITED]),
            0u32..5,
            any::<bool>(),
            prop::collection::btree_set(any::<[u8; 32]>(), 0..4),
        )
            .prop_map(
                |(link_id, link_secret, kind, expiry_ms, max_uses, uses, revoked, answered)| {
                    LinkRecordEntry {
                        link_id,
                        link_secret,
                        kind,
                        ledger_id: vec![],
                        expiry_ms,
                        max_uses,
                        uses,
                        revoked,
                        minted_at_ms: 0,
                        label: String::new(),
                        answered: answered.into_iter().collect(),
                    }
                },
            )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// ADR-0079 section 3: a request whose answer does not carry the
        /// record's secret is never accepted, whatever the record holds.
        #[test]
        fn a_mismatched_secret_is_never_accepted(
            r in arb_record(),
            other in any::<[u8; LINK_RECORD_SECRET_LEN]>(),
            key in any::<[u8; 32]>(),
            now in any::<u64>(),
        ) {
            prop_assume!(other != r.link_secret);
            prop_assert_ne!(r.verdict(&other, &key, now), LinkAnswerVerdict::Accept);
            prop_assert_ne!(r.verdict(&other, &key, now), LinkAnswerVerdict::Answered);
        }

        /// The matching secret accepts exactly a live, unanswered contact
        /// link.
        #[test]
        fn the_matching_secret_accepts_exactly_a_live_contact_link(
            r in arb_record(),
            key in any::<[u8; 32]>(),
            now in any::<u64>(),
        ) {
            let v = r.verdict(&r.link_secret, &key, now);
            let expect_accept = r.kind == LinkRecordKind::Contact
                && !r.answered.contains(&key)
                && r.is_live(now);
            prop_assert_eq!(v == LinkAnswerVerdict::Accept, expect_accept);
        }

        /// Any byte string decodes or is refused, never panics, and what
        /// decodes re-encodes to the same bytes.
        #[test]
        fn decode_is_total_and_canonical(bytes in prop::collection::vec(any::<u8>(), 0..400)) {
            let mut input = LINK_RECORD_ENVELOPE_DOMAIN_V1.to_vec();
            input.extend_from_slice(&bytes);
            if let Ok(entries) = decode_link_record_envelope(&input) {
                prop_assert_eq!(compose_link_record_envelope(&entries).unwrap(), input);
            }
        }
    }
}
