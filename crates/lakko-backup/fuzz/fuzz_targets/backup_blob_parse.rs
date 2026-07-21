// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `parse_backup_blob` (ADR-0037 §4 container
//! framing discriminator).
//!
//! `parse_backup_blob` runs on the restore path before any head
//! verification or AEAD-open — it peeks byte 0 of the stored
//! backup blob to disambiguate a `0xB1`-framed blob (tag + head +
//! envelope) from a legacy raw envelope. A malicious storage tier
//! crafts the WHOLE blob, so these bytes are fully attacker-
//! controlled and reach this bounded parser first. Any panic /
//! overflow / runaway allocation on a hostile length is a
//! vulnerability on the fresh-install rollback-defence path.
//!
//! Asserts: no panic on arbitrary input, and round-trip stability
//! on any `Framed` result — re-framing the parsed head + envelope
//! must reproduce a blob that re-parses to the identical `Framed`
//! pair (the framing is a total, invertible bijection on framed
//! blobs).

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_backup::{frame_backup_blob, parse_backup_blob, BackupBlob};

fuzz_target!(|data: &[u8]| {
    match parse_backup_blob(data) {
        Ok(BackupBlob::Framed { head, envelope }) => {
            // A parsed head is exactly `BACKUP_HEAD_LEN` bytes, so re-framing
            // must succeed, and the re-framed blob must re-parse identically.
            let reframed = frame_backup_blob(&head, &envelope)
                .expect("a parsed fixed-length head must re-frame");
            match parse_backup_blob(&reframed) {
                Ok(BackupBlob::Framed {
                    head: head2,
                    envelope: envelope2,
                }) => {
                    assert_eq!(head2, head, "re-framed head must be stable");
                    assert_eq!(envelope2, envelope, "re-framed envelope must be stable");
                },
                other => panic!("a re-framed blob must re-parse as Framed, got {other:?}"),
            }
        },
        // Legacy classification and hard fail-closed errors are both expected
        // outcomes on arbitrary input; the target only asserts no panic here.
        Ok(BackupBlob::Legacy(_)) | Err(_) => {},
    }
});
