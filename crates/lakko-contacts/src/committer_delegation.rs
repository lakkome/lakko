// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Committer delegation envelope ([ADR-0060] §2 and §6): the proposer of an
//! `Add` authorises whichever member ends up committing it to deliver the
//! Welcome to the added person's closed mailbox.
//!
//! ## Why this exists
//!
//! Under [ADR-0045] a member does not commit an `Add`; it proposes one, and
//! the acting committer (the owner, or an admin by rank) sweeps it into a
//! commit and delivers the Welcome. Since ADR-0060 the added person's
//! identity mailbox is closed: only someone holding one of that person's
//! reach keys can write to it. The **proposer** usually does (it added a
//! contact, so it holds their `contact` grant); the committer usually does
//! not. And the proposer cannot know which candidate will commit — the
//! election is a live fold over beacons ([ADR-0045] decision 2).
//!
//! So the proposer posts, in the group beside its proposal, **one delegation
//! per committer candidate** (the owner and each admin): a
//! `lakko_ds_proof::Delegation` signed with the proposer's reach key for the
//! added person's mailbox, naming that candidate's **account signature key**
//! as the delegate. Whichever candidate commits finds the delegation naming
//! its own key and writes the Welcome under it, signing the request with
//! `DsProofSigner::for_reach(its identity seed)` — the key the delegation
//! names. The Delivery Service checks the chain against the registered reach
//! key and consumes the delegation's nonce (one write per delegation).
//!
//! ## Wire format (v2, ADR-0078)
//!
//! ```text
//! COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1 || version(1) = 0x02
//!   || added_account_key(32) || added_mailbox_binding(97)
//!   || count(1, 1..=16) || count × delegation(185)
//! ```
//!
//! `added_account_key` is the added person's account signature key, and
//! `added_mailbox_binding` its [`MailboxBinding`] (the mailbox key signed by
//! that account key, ADR-0078): the proposer copies it from the grant it
//! holds for the added person. Every delegation must name
//! `mailbox_address(binding.mailbox_pk)`, and the binding must verify under
//! `added_account_key` (both checked where the delegations are parsed,
//! below). Each `delegation` is a `lakko_ds_proof::Delegation` wire blob,
//! exactly [`COMMITTER_DELEGATION_BLOB_LEN`] bytes. v1 (no binding) is
//! refused: a flag day.
//!
//! ## What this module checks, and what it cannot
//!
//! `lakko-ds-proof` depends on this crate (it names mailboxes with
//! [`crate::mailbox::mailbox_address`]), so this crate cannot depend on it
//! back. The codec here is therefore **structural only**: the domain, the
//! version, the binding's layout, the count bound and that every blob is
//! exactly [`COMMITTER_DELEGATION_BLOB_LEN`] bytes. Verifying the binding,
//! parsing each blob, checking that it names the added person's mailbox and
//! that no two name the same delegate, lives in `lakko_ds_proof::committer`,
//! which both FFI surfaces call. The client still checks that
//! `added_account_key` is the account its `Add` adds (the roster). `lakko-ds-proof` pins [`COMMITTER_DELEGATION_BLOB_LEN`] against its
//! own `DELEGATION_LEN` in a test, so the two cannot drift.
//!
//! ## Lifetime: the proposer re-sends
//!
//! The Delivery Service refuses a delegation with more than 24 hours left to
//! live (`MAX_DELEGATION_TTL_MS`), and an ADR-0045 proposal has no deadline:
//! it waits until a committer sweeps. So **the proposer re-sends a fresh
//! envelope while its proposal is still pending** — before the previous one
//! expires, and whenever the set of committer candidates changes (a new
//! admin has no delegation of its own). The receiver keeps, per
//! `(sender, added_account_key)`, the newest envelope; there is no stamp,
//! because each delegation carries its own expiry.
//!
//! ## What a member learns
//!
//! Every member reads the envelope. It learns the added person's account key
//! (which the `Add` proposal already shows), each candidate's key (already
//! in the role table), the expiry, and the reach public key that signed the
//! delegations — a public key, never a seed. Only a named candidate can use
//! a delegation, since only it can sign as the delegate, and each is good for
//! one write. Never rendered as a chat bubble.
//!
//! [ADR-0045]: ../../../docs/adr/0045-remote-commit-epoch-fork.md
//! [ADR-0060]: ../../../docs/adr/0060-scoped-discovery.md

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::mailbox::{MailboxBinding, MAILBOX_BINDING_LEN};

/// 30-byte domain separator. The trailing `\x00` keeps a UTF-8 free-text
/// payload from ever starting with it.
///
/// **The `v1` names the envelope FAMILY.** These bytes are frozen:
/// [`crate::incoming_envelope`] dispatches on this exact prefix; the
/// in-payload version byte is the knob that moves.
pub const COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/committer-delegation/v1\x00";

/// Schema-version byte carried first after the separator (v2: the added
/// person's mailbox binding, ADR-0078).
pub const COMMITTER_DELEGATION_VERSION_V2: u8 = 0x02;

/// Length of one delegation blob: `lakko_ds_proof::DELEGATION_LEN`
/// (`version(1) || mailbox(32) || delegate_pk(32) || expires_at_ms(8) ||
/// nonce(16) || issuer_pk(32) || sig(64)`). Pinned against that constant by
/// a test in `lakko-ds-proof`.
pub const COMMITTER_DELEGATION_BLOB_LEN: usize = 185;

/// Most delegations one envelope carries: one per committer candidate (the
/// owner and each admin). Chosen, not measured — a group with more than
/// fifteen admins gets delegations for the first sixteen candidates by
/// committer rank, which the proposer picks.
pub const MAX_COMMITTER_DELEGATIONS: usize = 16;

/// Length of the added person's account signature key.
pub const ADDED_ACCOUNT_KEY_LEN: usize = 32;

/// Bytes after the separator before the first delegation: version,
/// `added_account_key`, the binding, count.
const HEADER_LEN: usize = 1 + ADDED_ACCOUNT_KEY_LEN + MAILBOX_BINDING_LEN + 1;

/// Shortest well-formed envelope: one delegation.
pub const COMMITTER_DELEGATION_ENVELOPE_MIN_LEN: usize =
    COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1.len() + HEADER_LEN + COMMITTER_DELEGATION_BLOB_LEN;

/// Longest well-formed envelope: [`MAX_COMMITTER_DELEGATIONS`] delegations.
pub const COMMITTER_DELEGATION_ENVELOPE_MAX_LEN: usize = COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1
    .len()
    + HEADER_LEN
    + MAX_COMMITTER_DELEGATIONS * COMMITTER_DELEGATION_BLOB_LEN;

/// A decoded committer-delegation envelope (structurally checked only; see
/// the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitterDelegationEnvelope {
    /// The added person's account signature key.
    pub added_account_key: [u8; ADDED_ACCOUNT_KEY_LEN],
    /// The added person's mailbox binding (ADR-0078): every delegation must
    /// name its mailbox, and it must verify under `added_account_key`
    /// (checked in `lakko_ds_proof::committer`, not here).
    pub added_mailbox_binding: MailboxBinding,
    /// 1 to [`MAX_COMMITTER_DELEGATIONS`] `lakko_ds_proof::Delegation` wire
    /// blobs, one per committer candidate.
    pub delegations: Vec<[u8; COMMITTER_DELEGATION_BLOB_LEN]>,
}

/// Why a committer-delegation envelope was refused. A reason code; nothing
/// from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CommitterDelegationError {
    /// The plaintext does not carry the domain separator. Only from
    /// [`decode_committer_delegation`]; [`try_unwrap_committer_delegation`]
    /// returns `Ok(None)`.
    #[error("plaintext does not carry the committer-delegation domain separator")]
    WrongDomain,
    /// The version byte is not [`COMMITTER_DELEGATION_VERSION_V2`].
    #[error("committer delegation carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The count is 0 or above [`MAX_COMMITTER_DELEGATIONS`].
    #[error(
        "committer delegation carries {0} delegations, 1 to {MAX_COMMITTER_DELEGATIONS} allowed"
    )]
    BadCount(usize),
    /// The payload is not exactly the length its header declares.
    #[error("committer delegation has {actual} payload bytes, expected {expected}")]
    BadLength {
        /// Payload bytes after the separator.
        actual: usize,
        /// What the header (or the minimum header) requires.
        expected: usize,
    },
    /// The mailbox binding's layout is malformed.
    #[error("committer delegation carries a malformed mailbox binding")]
    BadBinding,
}

impl CommitterDelegationError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadCount(_) => "badCount",
            Self::BadLength { .. } => "badLength",
            Self::BadBinding => "badBinding",
        }
    }
}

/// Compose the envelope: the inner plaintext handed to the MLS
/// application-message send path on the group's journal.
///
/// # Errors
///
/// [`CommitterDelegationError::BadCount`] for no delegation or more than
/// [`MAX_COMMITTER_DELEGATIONS`] — the same bound [`try_unwrap_committer_delegation`]
/// refuses, so a conforming sender never emits what a receiver drops.
pub fn compose_committer_delegation(
    envelope: &CommitterDelegationEnvelope,
) -> CoreResult<Vec<u8>, CommitterDelegationError> {
    let count = envelope.delegations.len();
    if count == 0 || count > MAX_COMMITTER_DELEGATIONS {
        return Err(CommitterDelegationError::BadCount(count));
    }
    let count_byte = u8::try_from(count).map_err(|_| CommitterDelegationError::BadCount(count))?;
    let mut out = Vec::with_capacity(
        COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1.len()
            + HEADER_LEN
            + count * COMMITTER_DELEGATION_BLOB_LEN,
    );
    out.extend_from_slice(COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1);
    out.push(COMMITTER_DELEGATION_VERSION_V2);
    out.extend_from_slice(&envelope.added_account_key);
    out.extend_from_slice(&envelope.added_mailbox_binding.encode());
    out.push(count_byte);
    for blob in &envelope.delegations {
        out.extend_from_slice(blob);
    }
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(_))` — the separator matched and the envelope is well-formed.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the payload is malformed. The caller
///   drops it (fail-closed), never renders it.
///
/// # Errors
///
/// * [`CommitterDelegationError::UnknownVersion`] — checked BEFORE any
///   length, so a later revision reports as a version.
/// * [`CommitterDelegationError::BadLength`] — shorter than the header, or
///   not exactly `header + count × 185`.
/// * [`CommitterDelegationError::BadCount`] — 0 or more than
///   [`MAX_COMMITTER_DELEGATIONS`].
pub fn try_unwrap_committer_delegation(
    plaintext: &[u8],
) -> CoreResult<Option<CommitterDelegationEnvelope>, CommitterDelegationError> {
    let Some(payload) = plaintext.strip_prefix(COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload
        .split_first()
        .ok_or(CommitterDelegationError::BadLength {
            actual: payload.len(),
            expected: HEADER_LEN,
        })?;
    if version != COMMITTER_DELEGATION_VERSION_V2 {
        return Err(CommitterDelegationError::UnknownVersion(version));
    }
    if rest.len() < ADDED_ACCOUNT_KEY_LEN + MAILBOX_BINDING_LEN + 1 {
        return Err(CommitterDelegationError::BadLength {
            actual: payload.len(),
            expected: HEADER_LEN,
        });
    }
    let (key_bytes, rest) = rest.split_at(ADDED_ACCOUNT_KEY_LEN);
    let mut added_account_key = [0u8; ADDED_ACCOUNT_KEY_LEN];
    added_account_key.copy_from_slice(key_bytes);
    let (binding_bytes, rest) = rest.split_at(MAILBOX_BINDING_LEN);
    let added_mailbox_binding =
        MailboxBinding::decode(binding_bytes).map_err(|_| CommitterDelegationError::BadBinding)?;
    let (&count_byte, blobs) = rest
        .split_first()
        .ok_or(CommitterDelegationError::BadLength {
            actual: payload.len(),
            expected: HEADER_LEN,
        })?;
    let count = usize::from(count_byte);
    if count == 0 || count > MAX_COMMITTER_DELEGATIONS {
        return Err(CommitterDelegationError::BadCount(count));
    }
    let expected = count * COMMITTER_DELEGATION_BLOB_LEN;
    if blobs.len() != expected {
        return Err(CommitterDelegationError::BadLength {
            actual: payload.len(),
            expected: HEADER_LEN + expected,
        });
    }
    let delegations = blobs
        .chunks_exact(COMMITTER_DELEGATION_BLOB_LEN)
        .map(|chunk| {
            let mut blob = [0u8; COMMITTER_DELEGATION_BLOB_LEN];
            blob.copy_from_slice(chunk);
            blob
        })
        .collect();
    Ok(Some(CommitterDelegationEnvelope {
        added_account_key,
        added_mailbox_binding,
        delegations,
    }))
}

/// Strict standalone decoder: a plaintext without the domain is a hard
/// error.
///
/// # Errors
///
/// * [`CommitterDelegationError::WrongDomain`] — no domain separator.
/// * Every error [`try_unwrap_committer_delegation`] returns.
pub fn decode_committer_delegation(
    bytes: &[u8],
) -> CoreResult<CommitterDelegationEnvelope, CommitterDelegationError> {
    try_unwrap_committer_delegation(bytes)?.ok_or(CommitterDelegationError::WrongDomain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(count: usize) -> CommitterDelegationEnvelope {
        CommitterDelegationEnvelope {
            added_account_key: [0xA0; 32],
            added_mailbox_binding: MailboxBinding::for_account_seed(&[0xA0; 32]),
            delegations: (0..count)
                .map(|i| {
                    let mut blob = [0u8; COMMITTER_DELEGATION_BLOB_LEN];
                    blob[0] = 1;
                    blob[1] = u8::try_from(i).unwrap();
                    blob
                })
                .collect(),
        }
    }

    #[test]
    fn round_trips_at_every_count_and_pins_the_layout() {
        for count in 1..=MAX_COMMITTER_DELEGATIONS {
            let env = envelope(count);
            let wire = compose_committer_delegation(&env).unwrap();
            let d = COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1.len();
            assert_eq!(&wire[..d], COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1);
            assert_eq!(wire[d], COMMITTER_DELEGATION_VERSION_V2);
            assert_eq!(&wire[d + 1..d + 33], &[0xA0; 32]);
            assert_eq!(&wire[d + 33..d + 130], &env.added_mailbox_binding.encode());
            assert_eq!(usize::from(wire[d + 130]), count);
            assert_eq!(wire.len(), d + HEADER_LEN + count * 185);
            assert_eq!(decode_committer_delegation(&wire).unwrap(), env);
        }
        assert_eq!(COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1.len(), 30);
        assert_eq!(COMMITTER_DELEGATION_ENVELOPE_MIN_LEN, 30 + 131 + 185);
        assert_eq!(COMMITTER_DELEGATION_ENVELOPE_MAX_LEN, 30 + 131 + 16 * 185);
        assert_eq!(
            compose_committer_delegation(&envelope(1)).unwrap().len(),
            COMMITTER_DELEGATION_ENVELOPE_MIN_LEN
        );
        assert_eq!(
            compose_committer_delegation(&envelope(MAX_COMMITTER_DELEGATIONS))
                .unwrap()
                .len(),
            COMMITTER_DELEGATION_ENVELOPE_MAX_LEN
        );
    }

    #[test]
    fn the_count_bound_is_refused_by_compose_and_decode_alike() {
        for count in [0, MAX_COMMITTER_DELEGATIONS + 1] {
            assert_eq!(
                compose_committer_delegation(&envelope(count)),
                Err(CommitterDelegationError::BadCount(count))
            );
            let mut wire = compose_committer_delegation(&envelope(1)).unwrap();
            let at = COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1.len() + 130;
            wire[at] = u8::try_from(count).unwrap();
            wire.truncate(at + 1);
            wire.extend(core::iter::repeat_n(0u8, count * 185));
            assert_eq!(
                decode_committer_delegation(&wire),
                Err(CommitterDelegationError::BadCount(count))
            );
        }
    }

    #[test]
    fn every_length_but_the_declared_one_is_refused() {
        let wire = compose_committer_delegation(&envelope(2)).unwrap();
        let d = COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1.len();
        for len in d + 1..wire.len() {
            assert!(
                matches!(
                    decode_committer_delegation(&wire[..len]),
                    Err(CommitterDelegationError::BadLength { .. })
                ),
                "{len}"
            );
        }
        let mut long = wire;
        long.push(0);
        assert!(matches!(
            decode_committer_delegation(&long),
            Err(CommitterDelegationError::BadLength { .. })
        ));
        // The bare separator: a broken envelope, never a fall-through.
        assert!(matches!(
            try_unwrap_committer_delegation(COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1),
            Err(CommitterDelegationError::BadLength { .. })
        ));
    }

    #[test]
    fn the_version_is_checked_before_the_length() {
        for version in [0x01, 0x03] {
            let mut wire = COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1.to_vec();
            wire.push(version);
            assert_eq!(
                decode_committer_delegation(&wire),
                Err(CommitterDelegationError::UnknownVersion(version))
            );
        }
    }

    #[test]
    fn a_malformed_binding_is_refused() {
        let mut wire = compose_committer_delegation(&envelope(1)).unwrap();
        wire[COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1.len() + 33] = 0x07;
        assert_eq!(
            decode_committer_delegation(&wire),
            Err(CommitterDelegationError::BadBinding)
        );
    }

    #[test]
    fn other_plaintexts_fall_through_or_are_refused_by_the_strict_decoder() {
        for other in [
            b"hello".as_slice(),
            b"",
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
        ] {
            assert_eq!(try_unwrap_committer_delegation(other), Ok(None));
            assert_eq!(
                decode_committer_delegation(other),
                Err(CommitterDelegationError::WrongDomain)
            );
        }
    }

    #[test]
    fn codes_are_frozen() {
        assert_eq!(CommitterDelegationError::WrongDomain.code(), "wrongDomain");
        assert_eq!(
            CommitterDelegationError::UnknownVersion(2).code(),
            "unknownVersion"
        );
        assert_eq!(CommitterDelegationError::BadCount(0).code(), "badCount");
        assert_eq!(CommitterDelegationError::BadBinding.code(), "badBinding");
        assert_eq!(
            CommitterDelegationError::BadLength {
                actual: 0,
                expected: 0
            }
            .code(),
            "badLength"
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_GID_DERIVE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::group_closed::GROUP_CLOSED_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTION_OFFER_ENVELOPE_DOMAIN_V1,
            crate::introduction::INTRODUCTIONS_ENVELOPE_DOMAIN_V1,
            crate::visibility::VISIBILITY_ENVELOPE_DOMAIN_V1,
            crate::visibility::REACH_STATE_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        let mine = COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1;
        for d in other_domains {
            assert_ne!(*d, mine, "{d:?}");
            assert!(
                !d.starts_with(mine) && !mine.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }
}
