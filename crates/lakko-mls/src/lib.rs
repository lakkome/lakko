// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS (RFC 9420) wrapper for Lakko.
//!
//! Wraps OpenMLS to fix the supported ciphersuites, isolate upstream API
//! churn and expose Lakko-specific abstractions:
//!
//! - [`identity::Identity`] — signing keypair + credential + per-device
//!   storage / crypto provider
//! - [`group::Group`] — single MLS conversation group with create / join /
//!   add / remove / message / process / exporter operations
//!
//! See `docs/adr/0001-mls-only.md`.

#![forbid(unsafe_code)]

pub mod ciphersuite;
pub mod device_binding;
pub mod device_group;
pub mod device_group_sync;
pub mod error;
pub mod group;
pub mod group_kind;
pub mod group_proposals;
pub mod group_roles;
pub mod history_bundle;
pub mod history_placeholder;
pub mod identity;
pub mod key_package;
pub mod pairing;
pub mod publisher_group;
pub mod refusal;
pub mod self_removal;
pub mod welcome_rendezvous;

pub use device_binding::{account_key_of_credential, DeviceBinding, DEVICE_BINDING_LEN};
pub use device_group::{AccountId, AccountSeed, DeviceGroup, MAX_SERVICE_ENVELOPE_LEN};
pub use error::{Error, Result};
pub use group::{
    epoch_after_message, epoch_of_message, is_external_join_proposal_for, Group, Incoming,
    MemberAddition, MemberRecord, ProposalCommit, GROUP_ID_LEN,
};
pub use group_kind::{GroupKind, GROUP_KIND_BODY_LEN};
pub use group_proposals::{ProposalKind, ProposalRecord};
pub use group_roles::{account_committer_rank, committer_rank, OutgoingOwner, Role, RoleTable};
pub use identity::Identity;
pub use publisher_group::{
    publisher_group_id_for, ChannelId, PublisherGroup, CHANNEL_EXPORTER_LABEL, CHANNEL_ID_LEN,
};
pub use refusal::{RefusalDisposition, RefusalReason};
pub use self_removal::{sign_self_removal, SelfRemovalProof, SELF_REMOVAL_PROOF_LEN};
pub use welcome_rendezvous::WelcomeRendezvous;

/// Re-exported MLS leaf-node index. Surfaced so callers do not need to
/// depend on `openmls` directly.
pub use openmls::prelude::LeafNodeIndex;

/// Parser entry points for fuzz harnesses.
///
/// These are **not** part of the supported public API. They exist so
/// `cargo fuzz` targets in `crates/lakko-mls/fuzz/` can drive bytes through
/// the wire-format parsers without setting up real Identity / Group state.
/// Each function is panic-safe by contract — fuzzing failures should
/// surface as `false` (parse rejected), never as a panic.
#[doc(hidden)]
pub mod fuzz_api {
    use openmls::framing::MlsMessageIn;
    use tls_codec::Deserialize as _;

    /// Try to parse a wire-format `MlsMessageIn`. Returns `true` on success.
    #[must_use]
    pub fn parse_mls_message(bytes: &[u8]) -> bool {
        let mut slice = bytes;
        MlsMessageIn::tls_deserialize(&mut slice).is_ok()
    }

    /// Decode a DeviceGroup self-removal proof (ADR-0081 amendment) and, when
    /// it decodes, check that it re-encodes to the identical bytes and that
    /// [`crate::SelfRemovalProof::covers`] answers without panicking on the
    /// attacker-chosen fields. Returns `true` when it decoded. The bytes
    /// travel in the removal-authorisation slot of the DeviceGroup journal
    /// envelope, which a malicious device of the account or the DS chooses.
    ///
    /// # Panics
    ///
    /// When a decoded proof does not re-encode to its input (a codec bug).
    #[must_use]
    pub fn decode_self_removal_proof(bytes: &[u8]) -> bool {
        let Ok(proof) = crate::SelfRemovalProof::decode(bytes) else {
            return false;
        };
        assert_eq!(
            proof.encode(),
            bytes,
            "a decoded proof re-encodes to its input"
        );
        let _ = proof.covers(
            &[1u8; 32],
            proof.epoch,
            &proof.removed_signature_pubkey,
            &[proof.key_package_ref.to_vec()],
        );
        true
    }

    /// Try to parse and validate a wire-format `KeyPackage`. Returns `true`
    /// on successful TLS-codec parse.
    #[must_use]
    pub fn parse_key_package(bytes: &[u8]) -> bool {
        crate::key_package::deserialize(bytes).is_ok()
    }

    /// Try to decode a history-bundle per-message placeholder. Returns
    /// `true` on a successful bounded parse. The bytes are
    /// attacker-controlled: a malicious history bundle reaches this
    /// parser after the AEAD-open succeeds (e.g. a compromised peer
    /// device on the same account).
    #[must_use]
    pub fn parse_history_placeholder(bytes: &[u8]) -> bool {
        crate::history_placeholder::HistoryMessagePlaceholder::decode(bytes).is_ok()
    }

    /// Try to parse a `DeviceGroupPayload` (the receiver-side
    /// `DeviceGroup::parse_devicegroup_payload`). Returns `true` on a
    /// successful prost decode + body-variant extraction. The bytes are
    /// attacker-touchable: a DeviceGroup journal `Incoming::Application`
    /// payload arrives from the zero-trust, opaque Delivery Service, so a
    /// malicious / corrupt entry reaches this parser before any state is
    /// consulted.
    #[must_use]
    pub fn parse_devicegroup_payload(bytes: &[u8]) -> bool {
        crate::DeviceGroup::parse_devicegroup_payload(bytes).is_ok()
    }

    /// ADR-0058 oracle over the same parser: `true` unless the decoder
    /// ACCEPTED a `FolderChanges` / `StateSnapshot` that breaks a section 4
    /// bound, or accepted one whose registers the fold then refuses. The
    /// harness asserts it, so a decoder that lets an out-of-bounds register
    /// through fails the fuzz run instead of reaching a client's state.
    ///
    /// The saved-message registers of ADR-0065 section 4 ride in the same
    /// `FolderChanges` (field 6), so the same `validate` call holds them to
    /// their bounds (snapshot text lengths, at most 128 blob ids of 32
    /// bytes, none repeated) and the same fold run must accept them.
    #[must_use]
    pub fn devicegroup_payload_accepted_within_bounds(bytes: &[u8]) -> bool {
        use crate::device_group::DeviceGroupBody;
        let Ok(parsed) = crate::DeviceGroup::parse_devicegroup_payload(bytes) else {
            return true;
        };
        let bounded = match &parsed.body {
            DeviceGroupBody::FolderChanges(c) => c.validate().is_ok(),
            DeviceGroupBody::StateSnapshot(s) => s.validate().is_ok(),
            _ => true,
        };
        if !bounded {
            return false;
        }
        let mut state = lakko_sync::SyncedState::new();
        parsed
            .stamped_entries()
            .into_iter()
            .all(|(hlc, entry)| state.apply(hlc, entry).is_ok())
    }

    /// `true` unless `bytes` parse as a DeviceGroup payload whose
    /// service envelope (ADR-0060 section 7) breaks its bound. The
    /// `devicegroup_payload` fuzz target asserts it: an accepted
    /// service envelope is always 1 to `MAX_SERVICE_ENVELOPE_LEN` bytes.
    #[must_use]
    pub fn devicegroup_service_envelope_within_bounds(bytes: &[u8]) -> bool {
        match crate::DeviceGroup::parse_devicegroup_payload(bytes) {
            Ok(parsed) => match parsed.body {
                crate::device_group::DeviceGroupBody::ServiceEnvelope { envelope } => {
                    !envelope.is_empty() && envelope.len() <= crate::MAX_SERVICE_ENVELOPE_LEN
                },
                _ => true,
            },
            Err(_) => true,
        }
    }

    /// Try to decode a Welcome rendezvous envelope
    /// (`WelcomeRendezvous::decode`, ADR-0027 Phase 3). The bytes are
    /// attacker-touchable: a prospective joiner decodes group-journal entries
    /// that a malicious poster / the zero-trust DS could have crafted, before
    /// any inner Welcome is opened. The bounded, fail-closed parser must stay
    /// panic-free on arbitrary input.
    #[must_use]
    pub fn decode_welcome_rendezvous(bytes: &[u8]) -> bool {
        crate::WelcomeRendezvous::decode(bytes).is_ok()
    }

    /// Try to decode a persisted pending-external-joins row
    /// (`group::decode_pending_external_joins`, ADR-0027). The bytes come out
    /// of the device's own MLS storage snapshot — device-key-wrapped, so not
    /// attacker-writable in the ordinary case — but the snapshot also rides
    /// the phrase-sealed backup a hostile storage tier serves, and the row is
    /// the first Lakko-owned parser `Group::load` runs. The bounded,
    /// fail-closed parser must stay panic-free on arbitrary input.
    ///
    /// The validating identity is built once from a fixed seed: only the
    /// ciphersuite matters to the check, and a corpus entry that reaches the
    /// signature verify with a matching suite is what the fuzzer is for.
    #[must_use]
    pub fn decode_pending_external_joins(bytes: &[u8]) -> bool {
        use std::sync::OnceLock;
        static IDENTITY: OnceLock<Option<crate::Identity>> = OnceLock::new();
        let Some(identity) =
            IDENTITY.get_or_init(|| crate::Identity::from_seed(&[0x5Fu8; 64]).ok())
        else {
            return false;
        };
        crate::group::decode_pending_external_joins(bytes, identity).is_ok()
    }

    /// The fixed login handle [`decode_pairing_qr`] binds against: 32 bytes
    /// of `0x5A`, a value the fuzzer can reach by construction.
    pub const PAIRING_QR_EXPECTED_HANDLE: [u8; crate::pairing::LOGIN_HANDLE_LEN] =
        [0x5A; crate::pairing::LOGIN_HANDLE_LEN];

    /// Try to decode a device-pairing payload (`pairing::decode`, v2 since
    /// ADR-0056). Returns `true` on a successful prost decode + the
    /// structural binding checks (version == 2 / login-handle length +
    /// constant-time match / rendezvous-pseudonym length / device-card
    /// length). The bytes are the
    /// MOST attacker-controllable input in the system — a victim physically
    /// scans a QR a malicious actor crafted, or a relay serves the posted
    /// payload — so this parser must stay panic-free on arbitrary input. The
    /// fixed [`PAIRING_QR_EXPECTED_HANDLE`] keeps the harness reproducible.
    #[must_use]
    pub fn decode_pairing_qr(bytes: &[u8]) -> bool {
        crate::pairing::decode(bytes, &PAIRING_QR_EXPECTED_HANDLE).is_ok()
    }

    /// The guard oracle for the `pairing_qr` target: `false` exactly when
    /// [`decode_pairing_qr`] accepts a payload whose version is not 2, whose
    /// embedded handle is not [`PAIRING_QR_EXPECTED_HANDLE`], or whose device
    /// card is longer than [`crate::pairing::PAIRING_DEVICE_CARD_MAX_LEN`].
    /// The harness asserts it; it never panics itself.
    #[must_use]
    pub fn pairing_qr_acceptance_is_bound(bytes: &[u8]) -> bool {
        use prost::Message as _;
        if !decode_pairing_qr(bytes) {
            return true;
        }
        lakko_proto::wire::device_pairing::v1::PairingQr::decode(bytes).is_ok_and(|p| {
            p.version == crate::pairing::PAIRING_VERSION
                && p.login_handle.as_slice() == PAIRING_QR_EXPECTED_HANDLE.as_slice()
                && p.device_card.len() <= crate::pairing::PAIRING_DEVICE_CARD_MAX_LEN
        })
    }

    /// Try to open a history-bundle (`history_bundle::open_bundle`), the
    /// receiver-side I-6/I-7 path: the OUTER `SealedBundle` prost-decode + the
    /// pinned-`algorithm` discriminator gate + the nonce-length check + the
    /// AEAD-open + the inner `HistoryBundle` decode. `sealed_bytes` is
    /// attacker-controlled — a malicious / corrupt bundle reaches this parser
    /// before any authentication — so a bad `algorithm`, a wrong nonce length,
    /// or a forged ciphertext must surface as a graceful `Err`, never a panic.
    /// A fixed all-zero secret keeps the harness reproducible (the fuzzer cannot
    /// forge a valid AEAD tag against it, so it exercises the decode + the gates
    /// + the auth-failure path, which is exactly I-6/I-7).
    #[must_use]
    pub fn open_history_bundle(bytes: &[u8]) -> bool {
        crate::history_bundle::open_bundle(&[0u8; 32], bytes).is_ok()
    }

    /// Decode the ADR-0022 `0xFF02` role-table body (`RoleTable::decode`). The
    /// bytes are attacker-touchable: the role table travels inside a Welcome or
    /// a `GroupContextExtensions` commit a malicious group founder / member can
    /// craft, reaching this bounded, fail-closed parser before any role lookup.
    /// Any panic / overflow / runaway allocation here is a vulnerability.
    #[must_use]
    pub fn decode_role_table(bytes: &[u8]) -> bool {
        crate::group_roles::RoleTable::decode(bytes).is_ok()
    }

    /// Decode the ADR-0062 `0xFF03` group-kind body (`GroupKind::decode`). The
    /// bytes are attacker-touchable: the kind travels inside every Welcome a
    /// group founder can craft, and `join_from_welcome` decodes it before the
    /// group is kept. Returns `true` on a successful decode, and **panics**
    /// (the harness's failure signal) if anything that decodes does not
    /// re-encode to the identical bytes.
    #[must_use]
    pub fn decode_group_kind(bytes: &[u8]) -> bool {
        match crate::group_kind::GroupKind::decode(bytes) {
            Ok(kind) => {
                assert_eq!(
                    kind.encode().as_slice(),
                    bytes,
                    "a decoded group kind must re-encode to the identical bytes"
                );
                true
            },
            Err(_) => false,
        }
    }

    /// Decode + verify an ADR-0050 device binding (`DeviceBinding::decode`).
    /// The bytes are attacker-touchable: they are a leaf's `BasicCredential`
    /// identity, which travels inside every KeyPackage, Add proposal and
    /// Welcome a stranger or a malicious member can craft, and reach this
    /// bounded, fail-closed parser at every admission gate before any state
    /// is consulted. The fuzzer cannot forge a valid Ed25519 signature, so it
    /// exercises the length / version / key-parse / verify-fail path; a valid
    /// input is unreachable and the resolver is asserted to answer `None` for
    /// any non-empty credential that does not verify.
    #[must_use]
    pub fn decode_device_binding(bytes: &[u8]) -> bool {
        let ok = crate::device_binding::DeviceBinding::decode(bytes).is_ok();
        if !ok && !bytes.is_empty() {
            assert!(
                crate::device_binding::account_key_of_credential(bytes, &[0u8; 32]).is_none(),
                "an unverifiable non-empty credential must resolve to no account"
            );
        }
        ok
    }
}
