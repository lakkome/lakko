// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Container-tagged backup-blob framing (G-11, [ADR-0037] §4 + S4).
//!
//! The rollback defence of [ADR-0037] rides the auth-signed
//! [`BackupHead`](lakko_attest::BackupHead) *inside* the one storage backup blob
//! so the head and the envelope it commits are swapped together in a single
//! atomic `PUT` (there is no cross-object atomic swap over HTTP — a separate
//! head object would leave a torn window that bricks a legitimate restore). The
//! framed blob is:
//!
//! ```text
//! CONTAINER_TAG(1 = 0xB1)  ‖  BackupHead(BACKUP_HEAD_LEN = 129)  ‖  envelope(..)
//! ```
//!
//! [`BACKUP_CONTAINER_TAG`] = `0xB1` is chosen to **not** collide with a valid
//! envelope schema-version byte: the v1 / v2 backup envelopes begin with `0x01`
//! / `0x02` (`lakko_backup::envelope::SCHEMA_VERSION` /
//! `lakko_backup::chain::SCHEMA_VERSION_V2`). So the restore path disambiguates
//! unambiguously by peeking byte 0 ([`parse_backup_blob`]):
//!
//! - byte 0 == `0xB1` **and** the blob is long enough to hold a head →
//!   [`BackupBlob::Framed`] — parse the fixed-length head, the rest is the
//!   envelope tail.
//! - byte 0 == `0xB1` but the blob is **too short** to hold a head →
//!   [`HeadFrameError::Truncated`], the hard fail-closed case: a blob that
//!   claims the container tag but cannot carry a head is active tamper /
//!   corruption, **never** legacy (ADR-0037 §6 "Invalid").
//! - byte 0 != `0xB1` (a v1 / v2 envelope, or any other byte) →
//!   [`BackupBlob::Legacy`] — a raw, unframed envelope with no head (the
//!   ADR-0037 §6 "Absent → fail-open" lane; a legacy or pre-rollout blob, or one
//!   whose head a hostile storage tier stripped).
//!
//! `services/storage` is **untouched** by this framing — the blob stays opaque
//! to the storage tier (invariant #2); the container tag + head are just more
//! opaque bytes below its existing size ceiling. There is no storage migration.
//!
//! This module owns only the **framing** — the byte layout and the
//! legacy-vs-framed discriminator. It does **not** verify the head: the caller
//! runs [`lakko_attest::verify_backup_head`] on the parsed [`BackupBlob::Framed`]
//! head bytes and applies the ADR-0037 §6 fail-open / fail-closed / stale-
//! override policy. Keeping framing and verification apart mirrors how
//! [`chain`](crate::chain) keeps `header_of` (plaintext peek) apart from
//! `open_v2` (the authenticated open).
//!
//! The head length is authoritatively [`lakko_attest::BACKUP_HEAD_LEN`] — this
//! module tracks whatever length the attest primitive fixes rather than pinning
//! its own copy of `129`, so a future head-layout change flows through the
//! framing without a second edit.
//!
//! [ADR-0037]: ../../../docs/adr/0037-backup-head-rollback-commitment.md

use lakko_attest::BACKUP_HEAD_LEN;
use thiserror::Error;

/// Container tag prefixed onto a framed backup blob. Deliberately **not** a
/// valid envelope schema-version byte (v1 = `0x01`, v2 = `0x02`), so the restore
/// path tells a framed blob from a legacy raw envelope by peeking byte 0.
pub const BACKUP_CONTAINER_TAG: u8 = 0xB1;

/// Why framing or parsing a backup blob failed. Both variants are hard
/// fail-closed signals (ADR-0037 §6 "Invalid") — there is no legitimate cause,
/// so a caller must refuse rather than fall back.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HeadFrameError {
    /// [`frame_backup_blob`] was handed a head whose length is not
    /// [`BACKUP_HEAD_LEN`]. A programming error on the mint side — the auth
    /// service always returns a fixed-length head.
    #[error("backup head has wrong length: {got} bytes (expected {BACKUP_HEAD_LEN})")]
    BadHeadLength {
        /// The actual head length received.
        got: usize,
    },
    /// The blob carries the [`BACKUP_CONTAINER_TAG`] but is too short to hold a
    /// [`BACKUP_HEAD_LEN`]-byte head — active tamper or corruption of a framed
    /// blob. Distinct from a legacy (non-`0xB1`) blob, which is a fail-open case.
    #[error("framed backup blob is truncated: {got} bytes, need >= {}", 1 + BACKUP_HEAD_LEN)]
    Truncated {
        /// The actual blob length received.
        got: usize,
    },
}

/// The outcome of peeking a stored backup blob's container framing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackupBlob {
    /// A `0xB1`-framed blob: the fixed-length auth-signed head (still to be
    /// verified by the caller via [`lakko_attest::verify_backup_head`]) followed
    /// by the raw envelope tail (still to be AEAD-opened under the recovery
    /// phrase).
    Framed {
        /// The [`BACKUP_HEAD_LEN`]-byte backup-head bytes, unverified.
        head: [u8; BACKUP_HEAD_LEN],
        /// The envelope portion (everything after the tag + head).
        envelope: Vec<u8>,
    },
    /// A legacy, unframed blob — no head present (byte 0 was not the container
    /// tag, or the blob was empty). The whole blob is the envelope. The ADR-0037
    /// §6 restore policy treats this as "Absent → fail-open".
    Legacy(Vec<u8>),
}

/// Frame an auth-signed backup head and its envelope into the single storage
/// blob `CONTAINER_TAG ‖ head ‖ envelope`. Called on the mint / upload path
/// after the head has been signed by `services/auth`.
///
/// # Errors
///
/// [`HeadFrameError::BadHeadLength`] if `head` is not exactly
/// [`BACKUP_HEAD_LEN`] bytes.
pub fn frame_backup_blob(head: &[u8], envelope: &[u8]) -> Result<Vec<u8>, HeadFrameError> {
    if head.len() != BACKUP_HEAD_LEN {
        return Err(HeadFrameError::BadHeadLength { got: head.len() });
    }
    let mut out = Vec::with_capacity(1 + BACKUP_HEAD_LEN + envelope.len());
    out.push(BACKUP_CONTAINER_TAG);
    out.extend_from_slice(head);
    out.extend_from_slice(envelope);
    Ok(out)
}

/// Peek a stored backup blob's container framing and split it into a head +
/// envelope (framed) or return the whole blob as a legacy envelope. Runs on the
/// restore path before any head verification or AEAD-open.
///
/// Byte-0 semantics (ADR-0037 §4):
///
/// - **empty** → [`BackupBlob::Legacy`] with an empty envelope. An empty blob
///   has no container-tag byte, so it cannot be framed (and, in particular, is
///   **not** the framed-but-truncated hard-fail case, which requires the tag).
///   It falls into the same fail-open "absent head" lane as any non-`0xB1`
///   blob; the downstream envelope-open is the real validity gate (it already
///   rejects empty / malformed input), so the framing layer stays a pure
///   disambiguator.
/// - **byte 0 == `0xB1`, long enough** → [`BackupBlob::Framed`].
/// - **byte 0 == `0xB1`, too short for a head** → [`HeadFrameError::Truncated`]
///   (hard fail-closed).
/// - **byte 0 != `0xB1`** → [`BackupBlob::Legacy`] (fail-open).
///
/// # Errors
///
/// [`HeadFrameError::Truncated`] when the blob claims the container tag but is
/// shorter than `1 + BACKUP_HEAD_LEN` bytes.
pub fn parse_backup_blob(blob: &[u8]) -> Result<BackupBlob, HeadFrameError> {
    match blob.first() {
        // Empty blob: no tag byte → legacy / absent-head lane (documented above).
        None => Ok(BackupBlob::Legacy(Vec::new())),
        // Framed: the container tag is present.
        Some(&BACKUP_CONTAINER_TAG) => {
            // `.get` keeps the slice arithmetic bounded — a blob too short to
            // hold a head yields `None` → the hard fail-closed `Truncated`,
            // never a panic on a hostile length.
            let head_slice = blob
                .get(1..1 + BACKUP_HEAD_LEN)
                .ok_or(HeadFrameError::Truncated { got: blob.len() })?;
            let mut head = [0u8; BACKUP_HEAD_LEN];
            head.copy_from_slice(head_slice);
            // The successful `get` above proves `blob.len() >= 1 +
            // BACKUP_HEAD_LEN`, so this tail slice exists; `unwrap_or_default`
            // keeps the access checked (an exactly-head-length blob → empty
            // envelope tail).
            let envelope = blob.get(1 + BACKUP_HEAD_LEN..).unwrap_or_default().to_vec();
            Ok(BackupBlob::Framed { head, envelope })
        },
        // Any other leading byte (a v1 / v2 envelope, or anything else) is a raw
        // legacy envelope with no head.
        Some(_) => Ok(BackupBlob::Legacy(blob.to_vec())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(byte: u8) -> [u8; BACKUP_HEAD_LEN] {
        [byte; BACKUP_HEAD_LEN]
    }

    #[test]
    fn frame_then_parse_round_trips_head_and_envelope() {
        let h = head(0xAB);
        let envelope = vec![0xCDu8; 200];
        let blob = frame_backup_blob(&h, &envelope).expect("frame");
        assert_eq!(blob[0], BACKUP_CONTAINER_TAG);
        assert_eq!(blob.len(), 1 + BACKUP_HEAD_LEN + envelope.len());
        match parse_backup_blob(&blob).expect("parse") {
            BackupBlob::Framed {
                head: got_head,
                envelope: got_env,
            } => {
                assert_eq!(got_head, h);
                assert_eq!(got_env, envelope);
            },
            BackupBlob::Legacy(_) => panic!("a framed blob must parse as Framed"),
        }
    }

    #[test]
    fn parse_frame_is_the_identity_for_arbitrary_envelope_lengths() {
        // Property-style: parse(frame(h, e)) == Framed { h, e } for a spread of
        // envelope lengths, including the empty tail (blob == tag + head only).
        let h = head(0x11);
        for len in [0usize, 1, 33, 129, 512, 4096] {
            let envelope = vec![0x7Eu8; len];
            let blob = frame_backup_blob(&h, &envelope).expect("frame");
            let parsed = parse_backup_blob(&blob).expect("parse");
            assert_eq!(
                parsed,
                BackupBlob::Framed { head: h, envelope },
                "round-trip must hold for envelope length {len}"
            );
        }
    }

    #[test]
    fn v1_leading_blob_is_legacy() {
        // A v1 envelope begins with schema-version byte 0x01.
        let raw = vec![0x01u8, 0x02, 0x03, 0x04];
        assert_eq!(
            parse_backup_blob(&raw).expect("parse"),
            BackupBlob::Legacy(raw.clone())
        );
    }

    #[test]
    fn v2_leading_blob_is_legacy() {
        // A v2 chained envelope begins with schema-version byte 0x02.
        let raw = vec![0x02u8, 0xAA, 0xBB];
        assert_eq!(
            parse_backup_blob(&raw).expect("parse"),
            BackupBlob::Legacy(raw.clone())
        );
    }

    #[test]
    fn arbitrary_non_tag_leading_blob_is_legacy() {
        // Any byte that is not the container tag classifies as legacy.
        let raw = vec![0x42u8; 300];
        assert_eq!(
            parse_backup_blob(&raw).expect("parse"),
            BackupBlob::Legacy(raw.clone())
        );
    }

    #[test]
    fn framed_but_one_byte_too_short_is_truncated() {
        // Tag present, but the blob cannot hold a full head → hard fail-closed.
        // Exactly `1 + BACKUP_HEAD_LEN - 1` bytes: tag + head-minus-one.
        let mut blob = vec![BACKUP_CONTAINER_TAG];
        blob.extend_from_slice(&[0x00u8; BACKUP_HEAD_LEN - 1]);
        assert_eq!(blob.len(), BACKUP_HEAD_LEN); // == 1 + head - 1
        assert_eq!(
            parse_backup_blob(&blob),
            Err(HeadFrameError::Truncated {
                got: BACKUP_HEAD_LEN
            })
        );
    }

    #[test]
    fn framed_lone_tag_byte_is_truncated() {
        // Just the tag with no head at all is still the truncated hard-fail case.
        assert_eq!(
            parse_backup_blob(&[BACKUP_CONTAINER_TAG]),
            Err(HeadFrameError::Truncated { got: 1 })
        );
    }

    #[test]
    fn framed_exactly_tag_plus_head_has_empty_envelope() {
        // The minimum framed blob: tag + head, no envelope tail.
        let h = head(0x5A);
        let mut blob = vec![BACKUP_CONTAINER_TAG];
        blob.extend_from_slice(&h);
        assert_eq!(blob.len(), 1 + BACKUP_HEAD_LEN);
        assert_eq!(
            parse_backup_blob(&blob).expect("parse"),
            BackupBlob::Framed {
                head: h,
                envelope: Vec::new(),
            }
        );
    }

    #[test]
    fn empty_blob_is_legacy_with_empty_envelope() {
        assert_eq!(
            parse_backup_blob(&[]).expect("parse"),
            BackupBlob::Legacy(Vec::new())
        );
    }

    #[test]
    fn frame_rejects_a_wrong_length_head() {
        assert_eq!(
            frame_backup_blob(&[0u8; BACKUP_HEAD_LEN - 1], &[]),
            Err(HeadFrameError::BadHeadLength {
                got: BACKUP_HEAD_LEN - 1
            })
        );
        assert_eq!(
            frame_backup_blob(&[0u8; BACKUP_HEAD_LEN + 1], &[]),
            Err(HeadFrameError::BadHeadLength {
                got: BACKUP_HEAD_LEN + 1
            })
        );
        assert_eq!(
            frame_backup_blob(&[], &[0xFF; 4]),
            Err(HeadFrameError::BadHeadLength { got: 0 })
        );
    }
}
