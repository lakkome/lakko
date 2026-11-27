// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The seal announcement and the reset seal in the identity bundle
//! ([ADR-0101] section 10, ADR-0073 "Amended 2026-11-26 (second)").
//!
//! A peer honours an account's key reset (moves its chats, groups, history
//! and channels to the new key) only when the reset is **sealed**: its new
//! key is signed by a seal key the account announced, under its old account
//! key, before the reset. Discovery carries both artefacts; clients verify
//! them, discovery cannot forge either (it holds no account key and no seal
//! secret), and stripping them only makes a reset unsealed: an availability
//! loss, never a takeover.
//!
//! Upload (`POST /discovery/identity`, both standard base64, both optional):
//!
//! - `reset_seals` — the account's newest announcement
//!   (`lakko_attest::SealAnnouncement`, 200 to 641 bytes). Checked here:
//!   it verifies under the UPLOADED identity key, names this account's
//!   `account_ref`, is at the generation the row is at after this upload,
//!   and its `seq` is strictly above every `seq` the row has held (the same
//!   bytes again are a no-op). `400 {"error":"reset-seals-invalid"}`, `409
//!   {"error":"reset-seals-stale"}`. Absent: the stored one is kept.
//! - `reset_seal` — the 214-byte seal of an attested key reset. Read only on
//!   an attested change that is a reset (a key-reset attestation, no
//!   supersession proof); ignored otherwise. Checked here: it verifies under
//!   the seal key it names, names this account, the uploaded key, the
//!   generation the row leaves and the one the attestation reaches, and the
//!   row's current announcement by `seq`, which lists its seal key under its
//!   kind. `400 {"error":"reset-seal-invalid"}`. A reset without one is
//!   unsealed (stored as none).
//!
//! When the generation moves (a reset or an ADR-0074 proven rotation) the
//! row's announcement becomes `previous_reset_seals`, the newest of the
//! previous generation, which a peer checks the reset against. A key change
//! clears a stored reset seal unless the change carries a new one.
//!
//! Why check what clients verify anyway: the KT log needs the announcement's
//! generation and `seq` for its leaf, so discovery decodes it in any case,
//! and refusing an announcement the account's own key did not sign keeps
//! garbage out of the log. The checks add nothing to what a peer trusts.
//!
//! The KT log: every newly stored announcement is appended to the
//! transparency log as an announcement leaf (best effort, like the identity
//! leaf, `logged_seals_seq` marks what was confirmed and the reconciler
//! appends the rest); on a reset, the previous generation's announcement is
//! appended BEFORE the reset's identity leaf when it was not logged yet, so
//! a peer can find it at an index below the reset.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use axum::http::StatusCode;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use lakko_attest::{key_reset_account_ref, ResetSeal, SealAnnouncement};
use lakko_crypto::signing::VerifyingKey;

use crate::key_reset::RouteError;

/// `400` body of an announcement that does not verify or names another
/// account or generation.
pub(crate) const CODE_SEALS_INVALID: &str = "reset-seals-invalid";

/// `409` body of an announcement whose `seq` is not above the row's.
pub(crate) const CODE_SEALS_STALE: &str = "reset-seals-stale";

/// `400` body of a reset seal that does not verify or does not match.
pub(crate) const CODE_SEAL_INVALID: &str = "reset-seal-invalid";

/// A decoded, signature-checked announcement with its wire bytes.
#[derive(Clone, Debug)]
pub(crate) struct Announcement {
    /// The wire bytes, stored as they came.
    pub(crate) bytes: Vec<u8>,
    /// The generation it was made at.
    pub(crate) key_generation: u64,
    /// Its `seq`.
    pub(crate) seq: u64,
}

fn refused(code: &'static str) -> RouteError {
    RouteError::Code(StatusCode::BAD_REQUEST, code)
}

/// Decode and check an uploaded announcement against the uploaded identity
/// key and the session's account. The generation is compared later, under
/// the row lock.
pub(crate) fn verified_announcement(
    b64: Option<&str>,
    account_id: &[u8; 16],
    signature_pubkey: &[u8; 32],
) -> Result<Option<Announcement>, RouteError> {
    let Some(b64) = b64 else {
        return Ok(None);
    };
    let bytes = B64.decode(b64).map_err(|_| refused(CODE_SEALS_INVALID))?;
    let key =
        VerifyingKey::from_bytes(signature_pubkey).map_err(|_| refused(CODE_SEALS_INVALID))?;
    let claims = SealAnnouncement::open(&bytes, &key).map_err(|_| refused(CODE_SEALS_INVALID))?;
    if claims.account_ref != key_reset_account_ref(account_id) {
        return Err(refused(CODE_SEALS_INVALID));
    }
    Ok(Some(Announcement {
        bytes,
        key_generation: claims.key_generation,
        seq: claims.seq,
    }))
}

/// Decode and check an uploaded reset seal's own signature, account and key.
/// Whether it matches the row's announcement and generations is
/// [`seal_matches`]'s question, under the row lock.
pub(crate) fn verified_seal(
    b64: Option<&str>,
    account_id: &[u8; 16],
    signature_pubkey: &[u8; 32],
) -> Result<Option<(ResetSeal, Vec<u8>)>, RouteError> {
    let Some(b64) = b64 else {
        return Ok(None);
    };
    let bytes = B64.decode(b64).map_err(|_| refused(CODE_SEAL_INVALID))?;
    let seal = ResetSeal::open(&bytes).map_err(|_| refused(CODE_SEAL_INVALID))?;
    if seal.account_ref != key_reset_account_ref(account_id)
        || &seal.new_signature_pubkey != signature_pubkey
    {
        return Err(refused(CODE_SEAL_INVALID));
    }
    Ok(Some((seal, bytes)))
}

/// Whether `seal` seals a reset from `from_generation` to `to_generation`
/// under `announcement` (the row's current one, as stored).
pub(crate) fn seal_matches(
    seal: &ResetSeal,
    announcement: Option<&[u8]>,
    from_generation: u64,
    to_generation: u64,
) -> bool {
    let Some(claims) = announcement.and_then(|a| SealAnnouncement::decode_unverified(a).ok())
    else {
        return false;
    };
    seal.from_generation == from_generation
        && seal.to_generation == to_generation
        && seal.announcement_seq == claims.seq
        && claims.key_generation == from_generation
        && claims.lists(seal.kind, &seal.seal_pk)
}

/// The `seq` of a stored announcement, when it decodes.
pub(crate) fn stored_seq(stored: Option<&[u8]>) -> Option<u64> {
    stored.and_then(|a| SealAnnouncement::decode_unverified(a).ok().map(|c| c.seq))
}

/// The upload's announcement against the row: `Ok(true)` to store it,
/// `Ok(false)` when it is the row's current one byte for byte (a retry),
/// `Err` when it is at another generation or not newer than every `seq` the
/// row holds.
pub(crate) fn admit(
    announcement: &Announcement,
    target_generation: u64,
    current: Option<&[u8]>,
    previous: Option<&[u8]>,
) -> Result<bool, RouteError> {
    if announcement.key_generation != target_generation {
        return Err(refused(CODE_SEALS_INVALID));
    }
    if current == Some(announcement.bytes.as_slice()) {
        return Ok(false);
    }
    let highest = stored_seq(current).max(stored_seq(previous));
    if highest.is_some_and(|h| announcement.seq <= h) {
        return Err(RouteError::Code(StatusCode::CONFLICT, CODE_SEALS_STALE));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_attest::{password_seal_key, SealEntry, SealKind};
    use lakko_crypto::signing::SigningKey;

    const ACCOUNT: [u8; 16] = [7; 16];

    fn announcement(key: &SigningKey, generation: u64, seq: u64) -> Vec<u8> {
        let seal = password_seal_key(&[3u8; 64]);
        SealAnnouncement::new(
            key_reset_account_ref(&ACCOUNT),
            generation,
            seq,
            1,
            vec![SealEntry {
                kind: SealKind::Password,
                tag: [0; 16],
                seal_pk: seal.verifying_key().to_bytes(),
            }],
        )
        .sign(key)
        .unwrap()
    }

    #[test]
    fn an_announcement_must_be_signed_by_the_uploaded_key_for_this_account() {
        let key = SigningKey::from_seed(&[1; 32]);
        let other = SigningKey::from_seed(&[2; 32]);
        let bytes = announcement(&key, 1, 5);
        let pk = key.verifying_key().to_bytes();
        let got = verified_announcement(Some(&B64.encode(&bytes)), &ACCOUNT, &pk)
            .unwrap()
            .unwrap();
        assert_eq!((got.key_generation, got.seq), (1, 5));
        assert!(verified_announcement(
            Some(&B64.encode(&bytes)),
            &ACCOUNT,
            &other.verifying_key().to_bytes()
        )
        .is_err());
        assert!(verified_announcement(Some(&B64.encode(&bytes)), &[8; 16], &pk).is_err());
        assert!(verified_announcement(Some("%%"), &ACCOUNT, &pk).is_err());
        assert!(verified_announcement(None, &ACCOUNT, &pk)
            .unwrap()
            .is_none());
    }

    #[test]
    fn admission_wants_the_generation_and_a_higher_seq() {
        let key = SigningKey::from_seed(&[1; 32]);
        let pk = key.verifying_key().to_bytes();
        let parse = |b: &[u8]| {
            verified_announcement(Some(&B64.encode(b)), &ACCOUNT, &pk)
                .unwrap()
                .unwrap()
        };
        let a5 = announcement(&key, 1, 5);
        let a6 = parse(&announcement(&key, 1, 6));
        assert!(admit(&a6, 1, Some(&a5), None).unwrap());
        assert!(!admit(&parse(&a5), 1, Some(&a5), None).unwrap());
        assert!(admit(&parse(&announcement(&key, 1, 4)), 1, Some(&a5), None).is_err());
        assert!(admit(&a6, 2, Some(&a5), None).is_err());
        assert!(admit(&parse(&announcement(&key, 2, 5)), 2, None, Some(&a5)).is_err());
        assert!(admit(&parse(&announcement(&key, 2, 6)), 2, None, Some(&a5)).unwrap());
    }
}
