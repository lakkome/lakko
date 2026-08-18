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
pub mod device_group;
pub mod error;
pub mod group;
pub mod group_roles;
pub mod history_bundle;
pub mod history_placeholder;
pub mod identity;
pub mod key_package;
pub mod pairing;
pub mod publisher_group;
pub mod welcome_rendezvous;

pub use device_group::{AccountId, DeviceGroup};
pub use error::{Error, Result};
pub use group::{epoch_of_message, Group, Incoming, MemberAddition, MemberRecord, GROUP_ID_LEN};
pub use group_roles::{Role, RoleTable};
pub use identity::Identity;
pub use publisher_group::{
    channel_id_from_group_id, ChannelId, PublisherGroup, CHANNEL_EXPORTER_LABEL, CHANNEL_ID_LEN,
};
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

    /// Try to decode a device-pairing QR payload (`pairing::decode`). Returns
    /// `true` on a successful prost decode + the structural binding checks
    /// (version / account_id-length + match / rendezvous-pseudonym-length). The
    /// bytes are the MOST attacker-controllable input in the system — a victim
    /// physically scans a QR a malicious actor crafted — so this parser must
    /// stay panic-free on arbitrary input. A fixed all-zero `expected_account_id`
    /// keeps the harness reproducible; the fuzzer exercises the decode + the
    /// length/version gates (the account_id-equality path is reachable for a
    /// matching embedded id).
    #[must_use]
    pub fn decode_pairing_qr(bytes: &[u8]) -> bool {
        crate::pairing::decode(bytes, &[0u8; 16]).is_ok()
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
}
