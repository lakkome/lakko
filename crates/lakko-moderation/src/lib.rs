// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Reporting and moderation ([ADR-0099]).
//!
//! The pure half of moderation, shared by the clients (report sheet), the
//! moderators' panel (in the browser, through wasm) and every service that
//! applies a moderator's action:
//!
//! - [`reason`]: the closed list of report reasons, with frozen bytes;
//! - [`target`]: what a report or an action is about;
//! - [`report`]: the anonymous report, its clear part and its sealed content;
//! - [`seal`]: sealing to the moderator list's X25519 keys, and the moderator
//!   report key derived from a security key's PRF output;
//! - [`moderators`]: the moderator list the offline root signs;
//! - [`webauthn`]: the check of a moderator's WebAuthn assertion;
//! - [`statement`]: a moderator's action, as signed and as logged;
//! - [`evidence`]: the check that a reported message was written by the
//!   reported account;
//! - [`snapshot`]: the periodic snapshot of active suspensions and channel
//!   removals, its bucket answer, the client's check of it and the proof of
//!   a mark from the moderator's signed statement;
//! - [`apply`]: the one check every applying service runs on a statement
//!   (signature, freshness, action, reason) and the list floor it keeps.
//!
//! Nothing here talks to a network or holds state.
//!
//! [ADR-0099]: ../../../docs/adr/0099-reporting-and-moderation.md

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod appeal;
pub mod apply;
pub mod evidence;
pub mod moderators;
pub mod reason;
pub mod report;
pub mod seal;
pub mod snapshot;
#[cfg(feature = "software-authenticator")]
pub mod software_authenticator;
pub mod statement;
pub mod target;
pub mod webauthn;
mod wire;

pub use evidence::{verify_message_evidence, EvidenceVerdict};
pub use moderators::{ModeratorEntry, ModeratorKey, ModeratorList};
pub use reason::Reason;
pub use report::{ConversationKind, EvidenceItem, ReportClear, ReportContent};
pub use statement::{Action, SignedStatement, Statement};
pub use target::{Target, TargetKind};

/// Every refusal in this crate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModerationError {
    /// The bytes are too short, too long, or a length does not fit.
    #[error("malformed: {0}")]
    Malformed(&'static str),
    /// The domain separator does not match.
    #[error("wrong domain")]
    WrongDomain,
    /// An unknown version byte.
    #[error("unknown version {0}")]
    UnknownVersion(u8),
    /// A value outside its closed list (reason, action, kind).
    #[error("unknown value: {0}")]
    UnknownValue(&'static str),
    /// A field breaks a bound or a rule of its kind.
    #[error("invalid: {0}")]
    Invalid(&'static str),
    /// A signature does not verify.
    #[error("signature does not verify")]
    BadSignature,
    /// A WebAuthn assertion fails one of its checks.
    #[error("assertion: {0}")]
    Assertion(&'static str),
    /// The moderator is not on the list, or was revoked.
    #[error("moderator not authorised")]
    NotAuthorised,
    /// A sealed part does not open for this key.
    #[error("cannot open")]
    CannotOpen,
}

impl ModerationError {
    /// A stable code for the FFI surfaces and service responses.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Malformed(_) => "malformed",
            Self::WrongDomain => "wrong-domain",
            Self::UnknownVersion(_) => "unknown-version",
            Self::UnknownValue(_) => "unknown-value",
            Self::Invalid(_) => "invalid",
            Self::BadSignature => "bad-signature",
            Self::Assertion(_) => "assertion",
            Self::NotAuthorised => "not-authorised",
            Self::CannotOpen => "cannot-open",
        }
    }
}

/// Fuzz-only entry points. Not part of the supported API.
#[doc(hidden)]
pub mod fuzz_api {
    use crate::{moderators, report, statement};

    /// Run every decoder on arbitrary bytes; whatever decodes must re-encode
    /// to the identical bytes.
    pub fn decode_all(bytes: &[u8]) {
        if let Ok(c) = report::ReportClear::decode(bytes) {
            assert_eq!(c.encode(), bytes);
        }
        if let Ok(c) = report::ReportContent::decode(bytes) {
            assert_eq!(c.encode().ok().as_deref(), Some(bytes));
        }
        if let Ok(s) = statement::Statement::decode(bytes) {
            assert_eq!(s.encode(), bytes);
        }
        if let Ok(s) = statement::SignedStatement::decode(bytes) {
            assert_eq!(s.encode().ok().as_deref(), Some(bytes));
        }
        let _ = moderators::ModeratorList::decode_unverified(bytes);
        let _ = report::split_sealed(bytes);
    }

    /// Decode arbitrary bytes as a snapshot bucket, a snapshot root and a
    /// snapshot log leaf, and check them as a bucket answer for an account
    /// and for a channel (and for a target of the wrong length) under fixed
    /// keys; whatever decodes must re-encode to the identical bytes. Then
    /// split them into a bucket answer and a log entry answer and prove a
    /// mark from them under a fixed moderation root, with the bytes as the
    /// moderator list too: the fuzzer holds no key, so nothing may prove.
    pub fn snapshot_check(bytes: &[u8]) {
        use crate::snapshot::{check_account, check_target, prove_standing, Bucket, SnapshotRoot};
        if let Ok(b) = Bucket::decode(bytes) {
            assert_eq!(b.encode().ok().as_deref(), Some(bytes));
        }
        if let Ok(r) = SnapshotRoot::decode(bytes) {
            assert_eq!(r.encode(), bytes);
        }
        if let Ok(r) = SnapshotRoot::decode_log_leaf(bytes) {
            assert_eq!(r.log_leaf(), bytes);
        }
        let operator = lakko_crypto::signing::SigningKey::from_seed(&[11; 32])
            .verifying_key()
            .to_bytes();
        let witness: &[u8] = &[12; 32];
        // The fuzzer holds neither key, so nothing it writes may verify.
        assert!(check_account(bytes, &[5; 32], &operator, &[], 1, 1_000).is_err());
        assert!(check_account(bytes, &[5; 32], &operator, &[witness], 1, 1_000).is_err());
        for (kind, len) in [
            (crate::TargetKind::Channel, 16),
            (crate::TargetKind::Account, 32),
            (crate::TargetKind::Channel, 32),
        ] {
            let target = vec![5u8; len];
            assert!(check_target(bytes, kind, &target, &operator, &[], 1, 1_000).is_err());
            assert!(check_target(bytes, kind, &target, &operator, &[witness], 1, 1_000).is_err());
        }
        let root = lakko_crypto::signing::SigningKey::from_seed(&[13; 32])
            .verifying_key()
            .to_bytes();
        let split = usize::from(bytes.first().copied().unwrap_or(0)).min(bytes.len());
        let (answer, entry) = bytes.split_at(split);
        for (kind, len) in [
            (crate::TargetKind::Account, 32),
            (crate::TargetKind::Channel, 16),
            (crate::TargetKind::Blob, 32),
        ] {
            let target = vec![5u8; len];
            assert!(prove_standing(answer, entry, bytes, &root, kind, &target, 1_000).is_err());
            assert!(prove_standing(bytes, bytes, entry, &root, kind, &target, 1_000).is_err());
        }
    }

    /// Open arbitrary bytes as a report under a fixed moderator key; nothing
    /// unsealed by that key may open.
    pub fn open_report_fixed(bytes: &[u8]) {
        let Ok(secret) = crate::seal::moderator_report_secret(&[0x5a; 32]) else {
            return;
        };
        if let Ok((clear, content)) = report::open_report(bytes, &[0x5a; 16], &secret) {
            assert_eq!(usize::from(clear.evidence_count), content.evidence.len());
        }
    }

    /// Verify arbitrary bytes as a signed statement against a fixed list
    /// holding one Ed25519 and one P-256 moderator; the fuzzer holds neither
    /// key, so nothing may verify.
    pub fn verify_statement_fixed(bytes: &[u8]) {
        let list = moderators::ModeratorList {
            list_version: 1,
            issued_at_ms: 0,
            rp_id: "moderation.example".into(),
            origin: "https://moderation.example".into(),
            entries: vec![
                moderators::ModeratorEntry {
                    moderator_id: [1; 16],
                    name: "a".into(),
                    key: moderators::ModeratorKey::Ed25519(
                        lakko_crypto::signing::SigningKey::from_seed(&[7; 32])
                            .verifying_key()
                            .to_bytes(),
                    ),
                    credential_id: vec![1],
                    x25519_public: [1; 32],
                    added_at_ms: 0,
                    revoked_at_ms: 0,
                },
                moderators::ModeratorEntry {
                    moderator_id: [2; 16],
                    name: "b".into(),
                    // The P-256 generator, a valid point nobody holds the
                    // scalar 1 for in a real list.
                    key: moderators::ModeratorKey::P256(P256_GENERATOR),
                    credential_id: vec![2],
                    x25519_public: [2; 32],
                    added_at_ms: 0,
                    revoked_at_ms: 0,
                },
            ],
        };
        if let Ok(signed) = statement::SignedStatement::decode(bytes) {
            assert!(signed.verify(&list).is_err());
        }
    }

    const P256_GENERATOR: [u8; 65] = [
        0x04, 0x6b, 0x17, 0xd1, 0xf2, 0xe1, 0x2c, 0x42, 0x47, 0xf8, 0xbc, 0xe6, 0xe5, 0x63, 0xa4,
        0x40, 0xf2, 0x77, 0x03, 0x7d, 0x81, 0x2d, 0xeb, 0x33, 0xa0, 0xf4, 0xa1, 0x39, 0x45, 0xd8,
        0x98, 0xc2, 0x96, 0x4f, 0xe3, 0x42, 0xe2, 0xfe, 0x1a, 0x7f, 0x9b, 0x8e, 0xe7, 0xeb, 0x4a,
        0x7c, 0x0f, 0x9e, 0x16, 0x2b, 0xce, 0x33, 0x57, 0x6b, 0x31, 0x5e, 0xce, 0xcb, 0xb6, 0x40,
        0x68, 0x37, 0xbf, 0x51, 0xf5,
    ];
}

#[cfg(test)]
mod fuzz_api_tests {
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn the_fuzz_entry_points_hold_on_random_and_prefixed_bytes(
            bytes in proptest::collection::vec(any::<u8>(), 0..600),
            prefix in 0usize..4,
        ) {
            let domain: &[u8] = match prefix {
                0 => b"",
                1 => crate::report::REPORT_DOMAIN,
                2 => crate::statement::STATEMENT_DOMAIN,
                _ => crate::statement::SIGNED_DOMAIN,
            };
            let mut input = domain.to_vec();
            input.push(1);
            input.extend_from_slice(&bytes);
            for b in [&bytes, &input] {
                crate::fuzz_api::decode_all(b);
                crate::fuzz_api::open_report_fixed(b);
                crate::fuzz_api::verify_statement_fixed(b);
                crate::fuzz_api::snapshot_check(b);
            }
        }
    }
}
