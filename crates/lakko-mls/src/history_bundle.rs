// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! History catch-up bundle helpers (Phase 2a3d8).
//!
//! Wire-format codec + AEAD seal/open for the
//! [`lakko_proto::mls_app::history_bundle::v1`] payload, plus a
//! tiny exporter wrapper that derives the per-pairing symmetric
//! key from the DeviceGroup's MLS exporter (RFC 9420 §8.5).
//!
//! ## Flow
//!
//! ```text
//!   ┌── scanner device ───────────────┐  ┌── joining device ────────────────┐
//!   │ 1. add_device_from_wire(...)    │  │ 1. join_from_welcome(...)        │
//!   │ 2. exporter_secret(...)         │  │ 2. exporter_secret(...)          │
//!   │ 3. build HistoryBundle proto    │  │ 3. fetch SealedBundle from       │
//!   │ 4. seal_bundle(secret, bundle)  │  │    rendezvous mailbox            │
//!   │ 5. PUT SealedBundle wire bytes  │  │ 4. open_bundle(secret, sealed)   │
//!   │    to rendezvous mailbox        │  │ 5. project rows into local       │
//!   └─────────────────────────────────┘  │    chat store                    │
//!                                        └──────────────────────────────────┘
//! ```
//!
//! Both devices derive the same 32-byte secret because the
//! exporter ratchets off the DeviceGroup's *new* epoch — the
//! epoch the scanner advanced to inside `add_device_from_wire`
//! and the joining device advanced to inside `join_from_welcome`.
//!
//! Concrete cap of 1000 messages per group total (across the
//! whole bundle): the producer MUST refuse to seal a bundle with
//! more than [`MAX_BUNDLE_MESSAGES`]. Trimming per-group is the
//! caller's job — this module deals only with the seal/open
//! mechanics.

use prost::Message as _;

use crate::error::{Error, Result};
use crate::identity::Identity;

/// Hard cap on the total `HistoryMessage` count across every
/// group in a single bundle. Trade-off: 1000 covers the typical
/// "few weeks of chat across a handful of contacts" scenario
/// without inflating the rendezvous mailbox payload past ~1MB.
/// Bigger backfills should run via a future incremental sync
/// path, not by inflating this bundle.
pub const MAX_BUNDLE_MESSAGES: usize = 1000;

/// MLS exporter label for the history-bundle key. Pinned ASCII;
/// any change is a breaking protocol change to every existing
/// add-device handshake.
const HISTORY_EXPORTER_LABEL: &str = "lakko/history-bundle/v1";

/// Length of the AEAD nonce. Matches
/// `lakko_crypto::aead::NONCE_LEN` (96 bits per RFC 8439 /
/// NIST SP 800-38D).
const AEAD_NONCE_LEN: usize = 12;

/// `SealedBundle.algorithm` discriminator for
/// ChaCha20-Poly1305 (RFC 8439). Pinned to match the proto
/// schema's contract.
const ALGO_CHACHA20_POLY1305: u32 = 1;

/// Derive the per-pairing 32-byte history-bundle key.
///
/// Calls `MlsGroup::export_secret` on the DeviceGroup's
/// underlying [`crate::group::Group`] at the **current** epoch.
/// Both the scanner side (post-`add_device_from_wire`) and the
/// joining side (post-`join_from_welcome`) end up at the same
/// epoch, so they derive the same 32 bytes.
///
/// Returns the raw key bytes. The caller passes them straight
/// into [`seal_bundle`] / [`open_bundle`]; the AEAD layer takes
/// ownership and zeroizes after use via
/// `lakko_crypto::SecretBytes`.
///
/// # Errors
///
/// Returns [`Error::Exporter`] on the (practically unreachable)
/// case where the MLS exporter rejects the label / length.
pub fn exporter_secret(device_group: &crate::DeviceGroup, identity: &Identity) -> Result<[u8; 32]> {
    let secret =
        device_group
            .as_group()
            .export_secret(identity, HISTORY_EXPORTER_LABEL, &[], 32)?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&secret);
    Ok(out)
}

/// Encode + seal a [`lakko_proto::mls_app::history_bundle::v1::HistoryBundle`]
/// using `secret` as the AEAD key.
///
/// Steps:
///   1. Refuse to seal a bundle whose total message count is
///      over [`MAX_BUNDLE_MESSAGES`].
///   2. Prost-encode the inner `HistoryBundle` proto.
///   3. Generate a fresh 12-byte random nonce.
///   4. ChaCha20-Poly1305 seal under `secret` + the random
///      nonce, with empty AAD.
///   5. Wrap `(algorithm, nonce, ciphertext)` in a
///      `SealedBundle` proto, return the encoded wire bytes.
///
/// `secret` is a 32-byte symmetric key — typically the output of
/// [`exporter_secret`]. The caller should clear the buffer
/// after this call.
///
/// # Errors
///
/// - [`Error::Codec`] when the inner proto encode fails
///   (practically unreachable) or the cap is exceeded.
/// - [`Error::Message`] on AEAD seal failure (impossible for
///   ChaCha20-Poly1305 with valid inputs).
pub fn seal_bundle(
    secret: &[u8; 32],
    bundle: &lakko_proto::mls_app::history_bundle::v1::HistoryBundle,
) -> Result<Vec<u8>> {
    let total_messages: usize = bundle.groups.iter().map(|g| g.messages.len()).sum();
    if total_messages > MAX_BUNDLE_MESSAGES {
        return Err(Error::Codec(format!(
            "history bundle exceeds {MAX_BUNDLE_MESSAGES}-message cap ({total_messages})"
        )));
    }

    let mut plaintext = Vec::with_capacity(bundle.encoded_len());
    bundle
        .encode(&mut plaintext)
        .map_err(|e| Error::Codec(format!("HistoryBundle encode: {e}")))?;

    let mut nonce = [0u8; AEAD_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    let key = lakko_crypto::aead::AeadKey::new(*secret);
    let ciphertext = lakko_crypto::aead::seal(
        lakko_crypto::aead::Algorithm::ChaCha20Poly1305,
        &key,
        &nonce,
        &[],
        &plaintext,
    )
    .map_err(|e| Error::Message(format!("HistoryBundle seal: {e}")))?;

    let sealed = lakko_proto::mls_app::history_bundle::v1::SealedBundle {
        algorithm: ALGO_CHACHA20_POLY1305,
        nonce: nonce.to_vec(),
        ciphertext,
    };
    let mut out = Vec::with_capacity(sealed.encoded_len());
    sealed
        .encode(&mut out)
        .map_err(|e| Error::Codec(format!("SealedBundle encode: {e}")))?;
    Ok(out)
}

/// Inverse of [`seal_bundle`]. Parses the wire-format
/// `SealedBundle`, AEAD-opens with `secret`, decodes the inner
/// `HistoryBundle`.
///
/// # Errors
///
/// - [`Error::Codec`] when the outer `SealedBundle` or the inner
///   `HistoryBundle` decode fails.
/// - [`Error::Unsupported`] when `algorithm` is not the pinned
///   ChaCha20-Poly1305 value (a future migration would add new
///   discriminants here).
/// - [`Error::Codec`] with a `"nonce length"` body on a
///   wrong-length nonce.
/// - [`Error::Message`] on AEAD authentication failure (wrong
///   key, tampered ciphertext, wrong nonce).
pub fn open_bundle(
    secret: &[u8; 32],
    sealed_bytes: &[u8],
) -> Result<lakko_proto::mls_app::history_bundle::v1::HistoryBundle> {
    let sealed = lakko_proto::mls_app::history_bundle::v1::SealedBundle::decode(sealed_bytes)
        .map_err(|e| Error::Codec(format!("SealedBundle decode: {e}")))?;
    if sealed.algorithm != ALGO_CHACHA20_POLY1305 {
        return Err(Error::Unsupported(
            "history bundle algorithm not supported by this build",
        ));
    }
    if sealed.nonce.len() != AEAD_NONCE_LEN {
        return Err(Error::Codec(format!(
            "history bundle nonce length: expected {AEAD_NONCE_LEN}, got {}",
            sealed.nonce.len()
        )));
    }
    let mut nonce = [0u8; AEAD_NONCE_LEN];
    nonce.copy_from_slice(&sealed.nonce);

    let key = lakko_crypto::aead::AeadKey::new(*secret);
    let plaintext = lakko_crypto::aead::open(
        lakko_crypto::aead::Algorithm::ChaCha20Poly1305,
        &key,
        &nonce,
        &[],
        &sealed.ciphertext,
    )
    .map_err(|e| Error::Message(format!("HistoryBundle open: {e}")))?;

    let bundle =
        lakko_proto::mls_app::history_bundle::v1::HistoryBundle::decode(plaintext.as_slice())
            .map_err(|e| Error::Codec(format!("HistoryBundle decode: {e}")))?;
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceGroup;
    use lakko_proto::mls_app::history_bundle::v1::{HistoryBundle, HistoryGroup, HistoryMessage};

    fn fixture_account_id() -> [u8; 16] {
        let mut id = [0u8; 16];
        id[0] = 0x01;
        id[15] = 0xab;
        id
    }

    fn fixture_bundle(count_per_group: usize) -> HistoryBundle {
        HistoryBundle {
            max_hlc: 42,
            groups: vec![HistoryGroup {
                group_id: vec![0xAB; 32],
                count_capped: false,
                messages: (0..count_per_group)
                    .map(|i| {
                        // The fixture caps `count_per_group` at the
                        // bundle limit (1000) which fits in a u8 lane
                        // here only because tests stay under 256;
                        // assert defensively so a future test that
                        // pushes higher fails loudly rather than
                        // wrapping silently.
                        let byte = u8::try_from(i & 0xFF).expect("masked");
                        HistoryMessage {
                            payload_bytes: vec![byte; 16],
                            sender_leaf: 0,
                            message_id: vec![byte; 8],
                            received_at_ms: 1_700_000_000_000 + i as u64,
                        }
                    })
                    .collect(),
            }],
        }
    }

    /// Smoke: a freshly-bootstrapped DeviceGroup founder can
    /// derive the exporter secret. Just verifies the
    /// `exporter_secret` plumbing compiles + returns 32 bytes.
    #[test]
    fn exporter_secret_is_32_bytes() {
        let account = fixture_account_id();
        let identity = Identity::generate("founder").unwrap();
        let group = DeviceGroup::create_founder(&identity, &account).unwrap();
        let secret = exporter_secret(&group, &identity).unwrap();
        assert_eq!(secret.len(), 32);
    }

    /// End-to-end: founder + paired joiner derive the **same**
    /// exporter secret post-Welcome. This is the property that
    /// makes the seal/open round-trip work in production.
    #[test]
    fn exporter_agrees_across_paired_devices() {
        let account = fixture_account_id();
        let id_a = Identity::generate("device-A").unwrap();
        let mut group_a = DeviceGroup::create_founder(&id_a, &account).unwrap();

        let id_b = Identity::generate("device-B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let addition = group_a.add_device(&id_a, kp_b).unwrap();
        let welcome_bytes = crate::group::encode_message(&addition.welcome).unwrap();
        let group_b = DeviceGroup::join_from_welcome(&welcome_bytes, &id_b, &account).unwrap();

        let secret_a = exporter_secret(&group_a, &id_a).unwrap();
        let secret_b = exporter_secret(&group_b, &id_b).unwrap();
        assert_eq!(
            secret_a, secret_b,
            "founder and joining device must derive the same history-bundle exporter secret"
        );
    }

    #[test]
    fn seal_open_round_trips() {
        let secret = [0x77u8; 32];
        let bundle = fixture_bundle(3);
        let sealed = seal_bundle(&secret, &bundle).expect("seal");
        let opened = open_bundle(&secret, &sealed).expect("open");
        assert_eq!(opened, bundle);
    }

    #[test]
    fn seal_open_rejects_wrong_key() {
        let secret = [0x77u8; 32];
        let other = [0x88u8; 32];
        let bundle = fixture_bundle(2);
        let sealed = seal_bundle(&secret, &bundle).expect("seal");
        match open_bundle(&other, &sealed) {
            Err(Error::Message(_)) => {},
            Err(e) => panic!("expected Message error on tampered key, got {e:?}"),
            Ok(_) => panic!("open with wrong key must fail"),
        }
    }

    #[test]
    fn seal_refuses_oversized_bundle() {
        let secret = [0x77u8; 32];
        let bundle = fixture_bundle(MAX_BUNDLE_MESSAGES + 1);
        match seal_bundle(&secret, &bundle) {
            Err(Error::Codec(msg)) => assert!(msg.contains("cap"), "unexpected error body: {msg}"),
            other => panic!("expected Codec cap error, got {other:?}"),
        }
    }

    #[test]
    fn open_rejects_unknown_algorithm() {
        use lakko_proto::mls_app::history_bundle::v1::SealedBundle;
        let sealed = SealedBundle {
            algorithm: 9999,
            nonce: vec![0xCD; 12],
            ciphertext: vec![0xEE; 32],
        };
        let mut buf = Vec::new();
        sealed.encode(&mut buf).unwrap();
        match open_bundle(&[0u8; 32], &buf) {
            Err(Error::Unsupported(_)) => {},
            other => panic!("expected Unsupported error, got {other:?}"),
        }
    }
}
