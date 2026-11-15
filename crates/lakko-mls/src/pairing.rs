// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Device-pairing QR payload (Phase 2a3b1; v2 since ADR-0056).
//!
//! Wraps the generated [`lakko_proto::wire::device_pairing::v1::PairingQr`]
//! type with two helpers the three clients (web / iOS / Android)
//! consume to drive the add-device handshake:
//!
//!   * [`encode`] — fresh device side. Bundle its
//!     `Identity::create_key_package()` bytes + the **login handle**
//!     of the username the person typed + its own ADR-0080 device card
//!     into the wire payload, to render as a QR or seal for the
//!     camera-less code route (ADR-0049).
//!   * [`decode`] — confirming side. Parse the bytes, **verify the
//!     embedded login handle matches the confirming device's own
//!     handle** (constant time), and surface the inner `KeyPackage`
//!     for `DeviceGroup::add_device_from_wire`.
//!
//! The wire format itself lives in
//! `schemas/wire/device_pairing_v1.proto`; this module is the
//! one place that knows the field shape so the three clients
//! don't end up with subtly different decode paths.
//!
//! ## Why a login handle and not the account id (ADR-0056)
//!
//! v1 carried the 16-byte `account_id`. A fresh device learned it through
//! a username lookup, and since ADR-0054 S3 a lookup needs a session the
//! fresh device does not have. The handle
//! (`BLAKE3-derive-key("lakko/auth/login-handle/v1", canonical_username)`,
//! `lakko_contacts::username::login_handle`) is a public function of the
//! name the person types, so the fresh device computes it locally and the
//! confirming device computes its own from its own username. Field 2 is
//! reserved; the decoder accepts version 2 only (no dual path).
//!
//! ## Trust model
//!
//! - The payload is **not** signed — see the proto docstring.
//!   The trust anchor is the user pointing the camera at their
//!   own device's screen, or, on the camera-less route, typing the
//!   six-digit code the signed-in device shows into the new device:
//!   the payload then travels sealed under a SPAKE2 key derived from
//!   that code (`lakko-device-link`, ADR-0049 section 2 as amended
//!   2026-09-28).
//! - The handle check is the only structural guard: it stops the
//!   confirming device from enrolling a device meant for another
//!   account's name into the local DeviceGroup by mistake. It does
//!   **not** stop an active attacker who can present an arbitrary
//!   payload (the handle is a public function of the username) —
//!   that is what the camera or the device-link code covers.
//! - The device card (field 17) is what the new device says about itself.
//!   The confirming device shows it to name the device it is adding
//!   (`docs/ux/second-device.md` section 2.1) and trusts nothing in it;
//!   this module only bounds its length and never decodes it
//!   (`lakko-mls` does not depend on `lakko-contacts`).
//! - The comparison is constant time ([`SecretBytes`]' equality, which is
//!   `subtle::ConstantTimeEq`). The handle is not a secret, so this is
//!   defence in depth that the ADR asks for, not a load-bearing property.

use prost::Message as _;

use lakko_crypto::secret::SecretBytes;

use crate::device_group::AccountSeed;
use crate::error::{Error, Result};

/// Current payload schema version. The decoder accepts exactly this
/// version (ADR-0056: no dual path).
pub const PAIRING_VERSION: u32 = 2;

/// Length of a login handle in bytes (ADR-0054 S5).
pub const LOGIN_HANDLE_LEN: usize = 32;

/// 32-byte login handle of the account a fresh device asks to join.
pub type LoginHandle = [u8; LOGIN_HANDLE_LEN];

/// Length of the rendezvous pseudonym in bytes. Pinned at 32
/// to mirror every other pseudonym width in Lakko (routing
/// pseudonym in `wire/v1.proto`, contact pseudonyms in
/// `lakko-contacts`).
pub const RENDEZVOUS_PSEUDONYM_LEN: usize = 32;

/// 32-byte rendezvous pseudonym. The DS mailbox at this slot
/// is the one-shot Welcome delivery channel for an add-device
/// pairing session. Generated freshly by the joining device
/// each pairing — never reused across pairings.
pub type RendezvousPseudonym = [u8; RENDEZVOUS_PSEUDONYM_LEN];

/// Longest device card a pairing payload may carry: the longest
/// well-formed ADR-0080 device-card envelope
/// (`lakko_contacts::device_card::DEVICE_CARD_ENVELOPE_MAX_LEN`). Restated
/// here because `lakko-mls` does not depend on `lakko-contacts`; the FFI
/// crates, which depend on both, pin the equality.
pub const PAIRING_DEVICE_CARD_MAX_LEN: usize = 270;

/// Decoded pairing-QR contents the scanner side acts on.
/// Returned by [`decode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPairingQr {
    /// TLS-encoded `KeyPackage` for the joining device's
    /// DeviceIdentity. Hand straight to
    /// [`crate::device_group::DeviceGroup::add_device_from_wire`].
    pub key_package: Vec<u8>,
    /// 32-byte rendezvous pseudonym the scanner posts the
    /// resulting Welcome to via the DS mailbox.
    pub rendezvous_pseudonym: RendezvousPseudonym,
    /// ADR-0081 amendment — the rejoining device's self-removal proof
    /// (exactly [`crate::SELF_REMOVAL_PROOF_LEN`] bytes) when the payload is
    /// a DeviceGroup rejoin knock that carries one. Not verified here.
    pub self_removal_proof: Option<Vec<u8>>,
    /// The new device's ADR-0080 device-card envelope bytes, empty when
    /// the payload carries none (always empty on a rejoin knock). At most
    /// [`PAIRING_DEVICE_CARD_MAX_LEN`] bytes; NOT decoded or checked here.
    /// Display only: the caller decodes it to name the device and treats a
    /// card that does not decode as absent.
    pub device_card: Vec<u8>,
}

/// Fresh-device side: build the wire-format payload.
///
/// `login_handle` is the handle of the username the person typed
/// (`lakko_contacts::username::login_handle`). `key_package_bytes` is the
/// TLS-encoded `KeyPackage` from `Identity::create_key_package()`.
/// `rendezvous_pseudonym` is a fresh random 32-byte slot the joining
/// device chose for this pairing session; the confirming device PUTs the
/// Welcome to the DS mailbox keyed by it. `device_card` is the joining
/// device's own ADR-0080 device-card envelope (`composeDeviceCardEnvelope`),
/// or empty for none; it is carried for display and not checked here.
///
/// # Errors
///
/// Returns [`Error::Codec`] when `device_card` is longer than
/// [`PAIRING_DEVICE_CARD_MAX_LEN`], or on the unlikely prost-encode
/// failure.
pub fn encode(
    login_handle: &LoginHandle,
    key_package_bytes: &[u8],
    rendezvous_pseudonym: &RendezvousPseudonym,
    device_card: &[u8],
) -> Result<Vec<u8>> {
    if device_card.len() > PAIRING_DEVICE_CARD_MAX_LEN {
        return Err(Error::Codec(format!(
            "pairing payload: a device card is at most {PAIRING_DEVICE_CARD_MAX_LEN} bytes, got {}",
            device_card.len()
        )));
    }
    encode_payload(
        login_handle,
        key_package_bytes,
        rendezvous_pseudonym,
        Vec::new(),
        device_card.to_vec(),
    )
}

/// G-56 (ADR-0074 "Joining after a rotation") — the plain pairing payload of
/// [`encode`], carrying the joining device's self-removal proof
/// ([`crate::sign_self_removal`], exactly [`crate::SELF_REMOVAL_PROOF_LEN`]
/// bytes) for `key_package_bytes`, or none when `self_removal_proof` is
/// empty. A device that is already in the account's DeviceGroup (it came in
/// by pairing and is parked for the account's earlier keys) pairs again
/// under the same device identity: the confirming device cannot add a second
/// leaf under that key, and replaces the old one in one commit, which peers
/// accept under the removal policy on this proof (ADR-0081 amendment). The
/// payload is not sealed: it travels on the QR shown on screen or inside the
/// ADR-0049 code exchange, as every pairing payload does.
///
/// # Errors
///
/// [`Error::Codec`] when `self_removal_proof` is neither empty nor exactly
/// [`crate::SELF_REMOVAL_PROOF_LEN`] bytes, when `device_card` is longer than
/// [`PAIRING_DEVICE_CARD_MAX_LEN`], or on the unlikely prost-encode failure.
pub fn encode_with_self_removal_proof(
    login_handle: &LoginHandle,
    key_package_bytes: &[u8],
    rendezvous_pseudonym: &RendezvousPseudonym,
    device_card: &[u8],
    self_removal_proof: &[u8],
) -> Result<Vec<u8>> {
    if !self_removal_proof.is_empty() && self_removal_proof.len() != crate::SELF_REMOVAL_PROOF_LEN {
        return Err(Error::Codec(format!(
            "pairing payload: a self-removal proof is {} bytes, got {}",
            crate::SELF_REMOVAL_PROOF_LEN,
            self_removal_proof.len()
        )));
    }
    if device_card.len() > PAIRING_DEVICE_CARD_MAX_LEN {
        return Err(Error::Codec(format!(
            "pairing payload: a device card is at most {PAIRING_DEVICE_CARD_MAX_LEN} bytes, got {}",
            device_card.len()
        )));
    }
    encode_payload(
        login_handle,
        key_package_bytes,
        rendezvous_pseudonym,
        self_removal_proof.to_vec(),
        device_card.to_vec(),
    )
}

/// BLAKE3 derive-key context of the rejoin-knock seal key (G-55). Frozen.
pub const REJOIN_KNOCK_KEY_CONTEXT: &str = "lakko/device-group/knock-wrap/v1";

/// First bytes of a sealed rejoin knock. Frozen.
pub const REJOIN_KNOCK_MAGIC: &[u8] = b"lakko/rejoin-knock/v1\0";

/// The one sealed-knock version.
pub const REJOIN_KNOCK_VERSION: u8 = 0x01;

/// The sealed plaintext (`len(4 BE) ‖ pairing payload ‖ zeros`) is padded to
/// a multiple of this size, so the knock's length does not vary with the
/// KeyPackage. Chosen: measured on 2026-11-14 in the
/// `a_knock_is_one_padded_size` test, a bound device's KeyPackage was 417
/// bytes and the whole payload with a self-removal proof 631, so it fits one
/// step; a larger payload takes more steps (the DS takes 16 KiB per knock).
pub const REJOIN_KNOCK_PAD_STEP: usize = 1024;

const KNOCK_HEADER_LEN: usize = REJOIN_KNOCK_MAGIC.len() + 1;

/// Length of a sealed knock whose payload fits one padding step.
pub const REJOIN_KNOCK_SEALED_LEN: usize = KNOCK_HEADER_LEN
    + lakko_crypto::aead::NONCE_LEN
    + REJOIN_KNOCK_PAD_STEP
    + lakko_crypto::aead::TAG_LEN;

fn knock_key(lineage_account_seed: &AccountSeed) -> lakko_crypto::aead::AeadKey {
    lakko_crypto::aead::AeadKey::new(lakko_crypto::hash::blake3_derive_key_from_material(
        REJOIN_KNOCK_KEY_CONTEXT,
        lineage_account_seed,
    ))
}

fn knock_aad(lineage_account_seed: &AccountSeed) -> Vec<u8> {
    let mut aad = Vec::with_capacity(KNOCK_HEADER_LEN + 32);
    aad.extend_from_slice(REJOIN_KNOCK_MAGIC);
    aad.push(REJOIN_KNOCK_VERSION);
    aad.extend_from_slice(&crate::DeviceGroup::group_id_for(lineage_account_seed));
    aad
}

fn seal_knock(lineage_account_seed: &AccountSeed, payload: &[u8]) -> Result<Vec<u8>> {
    let len = u32::try_from(payload.len())
        .map_err(|_| Error::Codec("rejoin knock: payload too long".into()))?;
    let framed = 4 + payload.len();
    let padded_len = framed.div_ceil(REJOIN_KNOCK_PAD_STEP) * REJOIN_KNOCK_PAD_STEP;
    let mut plaintext = zeroize::Zeroizing::new(Vec::with_capacity(padded_len));
    plaintext.extend_from_slice(&len.to_be_bytes());
    plaintext.extend_from_slice(payload);
    plaintext.resize(padded_len, 0);
    let mut nonce = [0u8; lakko_crypto::aead::NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    let sealed = lakko_crypto::aead::seal(
        lakko_crypto::aead::Algorithm::ChaCha20Poly1305,
        &knock_key(lineage_account_seed),
        &nonce,
        &knock_aad(lineage_account_seed),
        &plaintext,
    )
    .map_err(|e| Error::Codec(format!("rejoin knock: seal: {e}")))?;
    let mut out = Vec::with_capacity(KNOCK_HEADER_LEN + nonce.len() + sealed.len());
    out.extend_from_slice(REJOIN_KNOCK_MAGIC);
    out.push(REJOIN_KNOCK_VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// ADR-0081 amendment — a DeviceGroup rejoin knock: the pairing payload
/// plus, when the journal names an epoch, the device's self-removal proof for
/// `key_package_bytes` (empty for none). A knock carries no device card: the
/// rejoining device is already known to its siblings.
///
/// **Sealed (G-55, ADR-0078 section 4).** The knock row is read by the
/// Delivery Service, and the payload names the account's login handle and
/// carries a KeyPackage whose binding names the account key. It is sealed
/// under `BLAKE3-derive-key(REJOIN_KNOCK_KEY_CONTEXT, lineage_account_seed)`,
/// the seed the DeviceGroup id derives from (every device of the account
/// holds it; [`crate::DeviceGroup::group_id_for`]), with the DeviceGroup id
/// in the AAD, and padded to [`REJOIN_KNOCK_PAD_STEP`]:
/// `REJOIN_KNOCK_MAGIC ‖ version(1) ‖ nonce(12) ‖ ChaCha20-Poly1305(len(4 BE)
/// ‖ payload ‖ zeros)`. A sibling opens it with [`open_rejoin_knock`].
///
/// # Errors
///
/// [`Error::Codec`] when `self_removal_proof` is neither empty nor exactly
/// [`crate::SELF_REMOVAL_PROOF_LEN`] bytes, or on an encode or seal failure.
pub fn encode_rejoin_knock(
    lineage_account_seed: &AccountSeed,
    login_handle: &LoginHandle,
    key_package_bytes: &[u8],
    rendezvous_pseudonym: &RendezvousPseudonym,
    self_removal_proof: &[u8],
) -> Result<Vec<u8>> {
    if !self_removal_proof.is_empty() && self_removal_proof.len() != crate::SELF_REMOVAL_PROOF_LEN {
        return Err(Error::Codec(format!(
            "rejoin knock: a self-removal proof is {} bytes, got {}",
            crate::SELF_REMOVAL_PROOF_LEN,
            self_removal_proof.len()
        )));
    }
    let payload = zeroize::Zeroizing::new(encode_payload(
        login_handle,
        key_package_bytes,
        rendezvous_pseudonym,
        self_removal_proof.to_vec(),
        Vec::new(),
    )?);
    seal_knock(lineage_account_seed, &payload)
}

/// A sibling's side of [`encode_rejoin_knock`]: open a knock row read from
/// the account's DeviceGroup knock queue under the lineage seed, then
/// [`decode`] the pairing payload against `expected_login_handle`.
///
/// # Errors
///
/// [`Error::Codec`] when the row is not a sealed knock of the known version,
/// does not open under this account's key (another account's knock, tampered
/// bytes), or its padding is malformed; then whatever [`decode`] returns.
pub fn open_rejoin_knock(
    lineage_account_seed: &AccountSeed,
    row: &[u8],
    expected_login_handle: &LoginHandle,
) -> Result<DecodedPairingQr> {
    let malformed = || Error::Codec("rejoin knock: not a sealed knock".into());
    if !row.starts_with(REJOIN_KNOCK_MAGIC) {
        return Err(malformed());
    }
    if row.get(REJOIN_KNOCK_MAGIC.len()) != Some(&REJOIN_KNOCK_VERSION) {
        return Err(Error::Unsupported(
            "rejoin knock version is not supported by this client",
        ));
    }
    let nonce_end = KNOCK_HEADER_LEN + lakko_crypto::aead::NONCE_LEN;
    let nonce: [u8; lakko_crypto::aead::NONCE_LEN] = row
        .get(KNOCK_HEADER_LEN..nonce_end)
        .and_then(|n| n.try_into().ok())
        .ok_or_else(malformed)?;
    let sealed = row.get(nonce_end..).ok_or_else(malformed)?;
    let plaintext = zeroize::Zeroizing::new(
        lakko_crypto::aead::open(
            lakko_crypto::aead::Algorithm::ChaCha20Poly1305,
            &knock_key(lineage_account_seed),
            &nonce,
            &knock_aad(lineage_account_seed),
            sealed,
        )
        .map_err(|_| Error::Codec("rejoin knock: does not open under this account's key".into()))?,
    );
    let payload = unpad_knock(&plaintext)?;
    decode(payload, expected_login_handle)
}

pub(crate) fn unpad_knock(plaintext: &[u8]) -> Result<&[u8]> {
    let malformed = || Error::Codec("rejoin knock: malformed padding".into());
    if plaintext.is_empty() || plaintext.len() % REJOIN_KNOCK_PAD_STEP != 0 {
        return Err(malformed());
    }
    let len_bytes: [u8; 4] = plaintext
        .get(..4)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(malformed)?;
    let len = usize::try_from(u32::from_be_bytes(len_bytes)).map_err(|_| malformed())?;
    let end = 4usize.checked_add(len).ok_or_else(malformed)?;
    let payload = plaintext.get(4..end).ok_or_else(malformed)?;
    let rest = plaintext.get(end..).ok_or_else(malformed)?;
    if rest.iter().any(|b| *b != 0) {
        return Err(malformed());
    }
    Ok(payload)
}

fn encode_payload(
    login_handle: &LoginHandle,
    key_package_bytes: &[u8],
    rendezvous_pseudonym: &RendezvousPseudonym,
    self_removal_proof: Vec<u8>,
    device_card: Vec<u8>,
) -> Result<Vec<u8>> {
    let payload = lakko_proto::wire::device_pairing::v1::PairingQr {
        version: PAIRING_VERSION,
        key_package: key_package_bytes.to_vec(),
        rendezvous_pseudonym: rendezvous_pseudonym.to_vec(),
        login_handle: login_handle.to_vec(),
        self_removal_proof,
        device_card,
    };
    let mut buf = Vec::with_capacity(payload.encoded_len());
    payload
        .encode(&mut buf)
        .map_err(|e| Error::Codec(format!("PairingQr encode: {e}")))?;
    Ok(buf)
}

/// Confirming side: parse the wire-format payload and bind it to
/// `expected_login_handle`, the handle of the confirming device's own
/// username. Returns the embedded `KeyPackage` bytes + rendezvous
/// pseudonym ready to drive
/// [`crate::device_group::DeviceGroup::add_device_from_wire`] and the
/// subsequent Welcome PUT.
///
/// # Errors
///
/// - [`Error::Codec`] when the bytes don't decode, the embedded
///   `login_handle` / `rendezvous_pseudonym` is the wrong length
///   (structurally suspicious), or the device card is longer than
///   [`PAIRING_DEVICE_CARD_MAX_LEN`]. A v1 payload carries no field 5 and so
///   fails here too if it somehow claims version 2.
/// - [`Error::Unsupported`] when the payload's `version` is anything other
///   than [`PAIRING_VERSION`] (older or newer).
/// - [`Error::Group`] with a `"login_handle mismatch"` body when the
///   embedded handle is not equal to `expected_login_handle`. The
///   confirming device refuses to enrol the fresh device.
pub fn decode(bytes: &[u8], expected_login_handle: &LoginHandle) -> Result<DecodedPairingQr> {
    let payload = lakko_proto::wire::device_pairing::v1::PairingQr::decode(bytes)
        .map_err(|e| Error::Codec(format!("PairingQr decode: {e}")))?;

    if payload.version != PAIRING_VERSION {
        return Err(Error::Unsupported(
            "pairing payload version is not supported by this client",
        ));
    }
    let embedded: LoginHandle = payload.login_handle.as_slice().try_into().map_err(|_| {
        Error::Codec(format!(
            "PairingQr login_handle length {} != {LOGIN_HANDLE_LEN}",
            payload.login_handle.len()
        ))
    })?;
    if SecretBytes::new(embedded) != SecretBytes::new(*expected_login_handle) {
        return Err(Error::Group(
            "pairing payload login_handle mismatch (meant for another account's name)".into(),
        ));
    }
    let rendezvous: RendezvousPseudonym = payload
        .rendezvous_pseudonym
        .as_slice()
        .try_into()
        .map_err(|_| {
            Error::Codec(format!(
                "PairingQr rendezvous_pseudonym length {} != {RENDEZVOUS_PSEUDONYM_LEN}",
                payload.rendezvous_pseudonym.len()
            ))
        })?;
    let self_removal_proof = match payload.self_removal_proof.len() {
        0 => None,
        crate::SELF_REMOVAL_PROOF_LEN => Some(payload.self_removal_proof),
        other => {
            return Err(Error::Codec(format!(
                "PairingQr self_removal_proof length {other} != {}",
                crate::SELF_REMOVAL_PROOF_LEN
            )))
        },
    };
    if payload.device_card.len() > PAIRING_DEVICE_CARD_MAX_LEN {
        return Err(Error::Codec(format!(
            "PairingQr device_card length {} > {PAIRING_DEVICE_CARD_MAX_LEN}",
            payload.device_card.len()
        )));
    }
    Ok(DecodedPairingQr {
        key_package: payload.key_package,
        rendezvous_pseudonym: rendezvous,
        self_removal_proof,
        device_card: payload.device_card,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_proto::wire::device_pairing::v1::PairingQr;
    use proptest::prelude::*;

    fn fixture_handle() -> LoginHandle {
        let mut h = [0u8; LOGIN_HANDLE_LEN];
        h[0] = 0xab;
        h[LOGIN_HANDLE_LEN - 1] = 0xcd;
        h
    }

    fn fixture_rendezvous() -> RendezvousPseudonym {
        let mut p = [0u8; RENDEZVOUS_PSEUDONYM_LEN];
        p[0] = 0xfe;
        p[RENDEZVOUS_PSEUDONYM_LEN - 1] = 0xed;
        p
    }

    fn encode_raw(payload: &PairingQr) -> Vec<u8> {
        let mut buf = Vec::new();
        payload.encode(&mut buf).unwrap();
        buf
    }

    /// Append a length-delimited protobuf field (wire type 2) by hand, for
    /// field numbers the generated type no longer knows.
    fn push_bytes_field(buf: &mut Vec<u8>, field: u32, value: &[u8]) {
        let key = (field << 3) | 2;
        prost::encoding::encode_varint(u64::from(key), buf);
        prost::encoding::encode_varint(value.len() as u64, buf);
        buf.extend_from_slice(value);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let handle = fixture_handle();
        let kp_bytes = vec![0x42u8; 768]; // shape-realistic stub
        let rendezvous = fixture_rendezvous();
        let qr = encode(&handle, &kp_bytes, &rendezvous, &[]).expect("encode");
        let recovered = decode(&qr, &handle).expect("decode");
        assert_eq!(recovered.key_package, kp_bytes);
        assert_eq!(recovered.rendezvous_pseudonym, rendezvous);
    }

    #[test]
    fn encoded_payload_carries_version_two_and_no_account_id() {
        let qr = encode(&fixture_handle(), &[1, 2, 3], &fixture_rendezvous(), &[]).unwrap();
        let raw = PairingQr::decode(qr.as_slice()).unwrap();
        assert_eq!(raw.version, 2);
        assert_eq!(raw.login_handle, fixture_handle().to_vec());
        // Field 2 (the retired account_id) is never written: its key byte
        // (2 << 3 | 2 = 0x12) never appears at a field boundary. Walk the
        // top-level fields to check.
        let mut rest = qr.as_slice();
        while !rest.is_empty() {
            let key = prost::encoding::decode_varint(&mut rest).unwrap();
            assert_ne!(key >> 3, 2, "field 2 must not be emitted");
            match key & 7 {
                0 => {
                    prost::encoding::decode_varint(&mut rest).unwrap();
                },
                2 => {
                    let len = usize::try_from(prost::encoding::decode_varint(&mut rest).unwrap())
                        .unwrap();
                    rest = &rest[len..];
                },
                other => panic!("unexpected wire type {other}"),
            }
        }
    }

    #[test]
    fn decode_rejects_wrong_handle() {
        let handle_a = fixture_handle();
        let mut handle_b = fixture_handle();
        handle_b[0] ^= 0xFF;
        let qr = encode(&handle_a, &[0x42u8; 128], &fixture_rendezvous(), &[]).expect("encode");
        match decode(&qr, &handle_b) {
            Ok(_) => panic!("decode should reject a payload for another name"),
            Err(Error::Group(msg)) => {
                assert!(msg.contains("login_handle mismatch"), "got: {msg}");
            },
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_every_version_but_two() {
        let handle = fixture_handle();
        for version in [0u32, 1, 3, 999] {
            let buf = encode_raw(&PairingQr {
                version,
                key_package: vec![0u8; 32],
                rendezvous_pseudonym: fixture_rendezvous().to_vec(),
                login_handle: handle.to_vec(),
                self_removal_proof: Vec::new(),
                device_card: Vec::new(),
            });
            match decode(&buf, &handle) {
                Ok(_) => panic!("decode should reject version {version}"),
                Err(Error::Unsupported(_)) => {},
                Err(other) => panic!("version {version}: unexpected error: {other:?}"),
            }
        }
    }

    /// A v1 payload as a v1 encoder wrote it: version 1, the 16-byte
    /// account id in field 2, no handle. Refused on the version.
    #[test]
    fn decode_rejects_a_v1_payload() {
        let mut buf = Vec::new();
        prost::encoding::encode_key(1, prost::encoding::WireType::Varint, &mut buf);
        prost::encoding::encode_varint(1, &mut buf);
        push_bytes_field(&mut buf, 2, &[0xAB; 16]);
        push_bytes_field(&mut buf, 3, &[0u8; 32]);
        push_bytes_field(&mut buf, 4, &fixture_rendezvous());
        assert!(matches!(
            decode(&buf, &fixture_handle()),
            Err(Error::Unsupported(_))
        ));
    }

    /// A payload that claims version 2 but carries the old field 2 and no
    /// handle: the unknown field is skipped, the missing handle is a
    /// zero-length field, refused on the length.
    #[test]
    fn decode_rejects_old_account_id_field_without_handle() {
        let mut buf = Vec::new();
        prost::encoding::encode_key(1, prost::encoding::WireType::Varint, &mut buf);
        prost::encoding::encode_varint(u64::from(PAIRING_VERSION), &mut buf);
        push_bytes_field(&mut buf, 2, &[0xAB; 16]);
        push_bytes_field(&mut buf, 3, &[0u8; 32]);
        push_bytes_field(&mut buf, 4, &fixture_rendezvous());
        match decode(&buf, &fixture_handle()) {
            Ok(_) => panic!("decode should reject a payload with no handle"),
            Err(Error::Codec(msg)) => assert!(msg.contains("login_handle"), "got: {msg}"),
            Err(other) => panic!("unexpected error: {other:?}"),
        }
        // Not even the all-zero expected handle matches a missing one.
        assert!(decode(&buf, &[0u8; LOGIN_HANDLE_LEN]).is_err());
    }

    #[test]
    fn decode_rejects_bad_handle_length() {
        let handle = fixture_handle();
        for len in [0usize, 16, 31, 33, 64] {
            let buf = encode_raw(&PairingQr {
                version: PAIRING_VERSION,
                key_package: vec![0u8; 32],
                rendezvous_pseudonym: fixture_rendezvous().to_vec(),
                login_handle: vec![0xab; len],
                self_removal_proof: Vec::new(),
                device_card: Vec::new(),
            });
            match decode(&buf, &handle) {
                Ok(_) => panic!("decode should reject a {len}-byte handle"),
                Err(Error::Codec(msg)) => assert!(msg.contains("login_handle"), "got: {msg}"),
                Err(other) => panic!("len {len}: unexpected error: {other:?}"),
            }
        }
    }

    /// Phase 2a3b — wrong-length rendezvous_pseudonym must be
    /// rejected before any DS-mailbox interaction.
    #[test]
    fn decode_rejects_bad_rendezvous_pseudonym_length() {
        let handle = fixture_handle();
        let buf = encode_raw(&PairingQr {
            version: PAIRING_VERSION,
            key_package: vec![0u8; 32],
            rendezvous_pseudonym: vec![0xee; 16],
            login_handle: handle.to_vec(),
            self_removal_proof: Vec::new(),
            device_card: Vec::new(),
        });
        match decode(&buf, &handle) {
            Ok(_) => panic!("decode should reject short rendezvous_pseudonym"),
            Err(Error::Codec(msg)) => {
                assert!(
                    msg.contains("rendezvous_pseudonym"),
                    "expected error to mention rendezvous_pseudonym, got: {msg}"
                );
            },
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    proptest! {
        /// Round trip for any handle, KeyPackage bytes and rendezvous, with
        /// no device card and with any card within the bound.
        #[test]
        fn prop_round_trip(
            handle in any::<[u8; 32]>(),
            kp in proptest::collection::vec(any::<u8>(), 0..2048),
            rendezvous in any::<[u8; 32]>(),
            card in proptest::option::of(
                proptest::collection::vec(any::<u8>(), 1..=PAIRING_DEVICE_CARD_MAX_LEN),
            ),
        ) {
            let card = card.unwrap_or_default();
            let qr = encode(&handle, &kp, &rendezvous, &card).unwrap();
            let got = decode(&qr, &handle).unwrap();
            prop_assert_eq!(got.key_package, kp);
            prop_assert_eq!(got.rendezvous_pseudonym, rendezvous);
            prop_assert_eq!(got.self_removal_proof, None);
            prop_assert_eq!(got.device_card, card);
        }

        /// A card longer than the bound is refused by the encoder, and a
        /// payload that carries one anyway is refused by the decoder.
        #[test]
        fn prop_over_long_card_refused(
            extra in 1usize..1024,
            byte in any::<u8>(),
        ) {
            let handle = fixture_handle();
            let card = vec![byte; PAIRING_DEVICE_CARD_MAX_LEN + extra];
            let refused_encode =
                matches!(encode(&handle, &[1], &fixture_rendezvous(), &card), Err(Error::Codec(_)));
            prop_assert!(refused_encode);
            let buf = encode_raw(&PairingQr {
                version: PAIRING_VERSION,
                key_package: vec![1],
                rendezvous_pseudonym: fixture_rendezvous().to_vec(),
                login_handle: handle.to_vec(),
                self_removal_proof: Vec::new(),
                device_card: card,
            });
            let refused_decode = matches!(decode(&buf, &handle), Err(Error::Codec(_)));
            prop_assert!(refused_decode);
        }

        /// Any handle that differs from the embedded one is refused with the
        /// mismatch error, whichever byte differs.
        #[test]
        fn prop_wrong_handle_refused(
            handle in any::<[u8; 32]>(),
            other in any::<[u8; 32]>(),
            kp in proptest::collection::vec(any::<u8>(), 0..256),
        ) {
            prop_assume!(handle != other);
            let qr = encode(&handle, &kp, &fixture_rendezvous(), &[]).unwrap();
            let refused = matches!(decode(&qr, &other), Err(Error::Group(_)));
            prop_assert!(refused);
        }

        /// Arbitrary bytes never panic, and anything that decodes carries
        /// version 2, the expected handle and a device card within the bound
        /// (the fuzz oracle, run here too).
        #[test]
        fn prop_arbitrary_bytes_never_panic(
            bytes in proptest::collection::vec(any::<u8>(), 0..512),
        ) {
            let _ = decode(&bytes, &fixture_handle());
            prop_assert!(crate::fuzz_api::pairing_qr_acceptance_is_bound(&bytes));
        }
    }

    // ---- docs/ux/second-device.md 2.1: the payload names the new device ----

    #[test]
    fn a_pairing_payload_carries_the_device_card_verbatim() {
        let handle = fixture_handle();
        let card = b"lakko/device-card/v1\x00 any bytes, not decoded here".to_vec();
        let qr = encode(&handle, &[1, 2, 3], &fixture_rendezvous(), &card).unwrap();
        let raw = PairingQr::decode(qr.as_slice()).unwrap();
        assert_eq!(raw.device_card, card);
        let decoded = decode(&qr, &handle).unwrap();
        assert_eq!(decoded.device_card, card);
        assert_eq!(decoded.self_removal_proof, None);
    }

    #[test]
    fn a_payload_without_a_card_decodes_to_an_empty_card() {
        let handle = fixture_handle();
        let qr = encode(&handle, &[1, 2, 3], &fixture_rendezvous(), &[]).unwrap();
        assert!(decode(&qr, &handle).unwrap().device_card.is_empty());
    }

    #[test]
    fn the_card_bound_is_inclusive_and_one_more_byte_is_refused() {
        let handle = fixture_handle();
        let at = vec![0x6c; PAIRING_DEVICE_CARD_MAX_LEN];
        let qr = encode(&handle, &[1], &fixture_rendezvous(), &at).unwrap();
        assert_eq!(decode(&qr, &handle).unwrap().device_card, at);
        let over = vec![0x6c; PAIRING_DEVICE_CARD_MAX_LEN + 1];
        assert!(matches!(
            encode(&handle, &[1], &fixture_rendezvous(), &over),
            Err(Error::Codec(_))
        ));
        let buf = encode_raw(&PairingQr {
            version: PAIRING_VERSION,
            key_package: vec![1],
            rendezvous_pseudonym: fixture_rendezvous().to_vec(),
            login_handle: handle.to_vec(),
            self_removal_proof: Vec::new(),
            device_card: over,
        });
        match decode(&buf, &handle) {
            Err(Error::Codec(msg)) => assert!(msg.contains("device_card"), "got: {msg}"),
            other => panic!("expected a device_card refusal, got {other:?}"),
        }
    }

    const LINEAGE: AccountSeed = [0x3c; 32];

    #[test]
    fn a_rejoin_knock_carries_no_device_card() {
        let handle = fixture_handle();
        let proof = vec![0x5a; crate::SELF_REMOVAL_PROOF_LEN];
        let knock =
            encode_rejoin_knock(&LINEAGE, &handle, &[1, 2, 3], &fixture_rendezvous(), &proof)
                .unwrap();
        let opened = open_rejoin_knock(&LINEAGE, &knock, &handle).unwrap();
        assert!(opened.device_card.is_empty());
    }

    // ---- ADR-0081 amendment: the rejoin knock carries a self-removal proof ----

    #[test]
    fn a_rejoin_knock_carries_its_proof_and_a_pairing_payload_none() {
        let handle = fixture_handle();
        let proof = vec![0x5a; crate::SELF_REMOVAL_PROOF_LEN];
        let knock =
            encode_rejoin_knock(&LINEAGE, &handle, &[1, 2, 3], &fixture_rendezvous(), &proof)
                .unwrap();
        let decoded = open_rejoin_knock(&LINEAGE, &knock, &handle).unwrap();
        assert_eq!(decoded.self_removal_proof, Some(proof));
        assert_eq!(decoded.key_package, vec![1, 2, 3]);
        let plain = encode(&handle, &[1, 2, 3], &fixture_rendezvous(), &[]).unwrap();
        assert_eq!(decode(&plain, &handle).unwrap().self_removal_proof, None);
        let unproven =
            encode_rejoin_knock(&LINEAGE, &handle, &[1, 2, 3], &fixture_rendezvous(), &[]).unwrap();
        assert_eq!(
            open_rejoin_knock(&LINEAGE, &unproven, &handle)
                .unwrap()
                .self_removal_proof,
            None
        );
    }

    // ---- G-55: the knock is sealed under the lineage seed ----

    #[test]
    fn the_knock_constants_are_pinned() {
        assert_eq!(REJOIN_KNOCK_KEY_CONTEXT, "lakko/device-group/knock-wrap/v1");
        assert_eq!(REJOIN_KNOCK_MAGIC, b"lakko/rejoin-knock/v1\0");
        assert_eq!(REJOIN_KNOCK_SEALED_LEN, 22 + 1 + 12 + 1024 + 16);
    }

    /// A realistic knock (a bound device's KeyPackage and a proof) is one
    /// padded size, and so is a small one: the length says nothing about the
    /// KeyPackage.
    #[test]
    fn a_knock_is_one_padded_size() {
        let device = crate::Identity::from_seed_bound(&[0x71; 64], &[0x72; 32], 1).unwrap();
        let kp = crate::key_package::serialize(&device.create_key_package().unwrap()).unwrap();
        let proof = vec![0x5a; crate::SELF_REMOVAL_PROOF_LEN];
        let big = encode_rejoin_knock(
            &LINEAGE,
            &fixture_handle(),
            &kp,
            &fixture_rendezvous(),
            &proof,
        )
        .unwrap();
        let small = encode_rejoin_knock(
            &LINEAGE,
            &fixture_handle(),
            &[1],
            &fixture_rendezvous(),
            &[],
        )
        .unwrap();
        assert_eq!(big.len(), REJOIN_KNOCK_SEALED_LEN);
        assert_eq!(small.len(), REJOIN_KNOCK_SEALED_LEN);
        let opened = open_rejoin_knock(&LINEAGE, &big, &fixture_handle()).unwrap();
        assert_eq!(opened.key_package, kp);
    }

    #[test]
    fn a_knock_names_neither_the_handle_nor_the_account_key() {
        let account = [0x72u8; 32];
        let device = crate::Identity::from_seed_bound(&[0x71; 64], &account, 1).unwrap();
        let kp = crate::key_package::serialize(&device.create_key_package().unwrap()).unwrap();
        let handle = fixture_handle();
        let knock =
            encode_rejoin_knock(&LINEAGE, &handle, &kp, &fixture_rendezvous(), &[]).unwrap();
        let account_key = device.account_public_key().unwrap();
        let has = |needle: &[u8]| knock.windows(needle.len()).any(|w| w == needle);
        assert!(!has(&handle), "no login handle");
        assert!(!has(&account_key), "no account key");
        assert!(!has(device.device_binding().unwrap()), "no device binding");
        assert!(!has(&fixture_rendezvous()), "no rendezvous");
    }

    #[test]
    fn a_knock_does_not_open_under_another_seed_or_as_another_row() {
        let handle = fixture_handle();
        let knock =
            encode_rejoin_knock(&LINEAGE, &handle, &[1, 2], &fixture_rendezvous(), &[]).unwrap();
        assert!(matches!(
            open_rejoin_knock(&[0x3d; 32], &knock, &handle),
            Err(Error::Codec(_))
        ));
        let plain = encode(&handle, &[1, 2], &fixture_rendezvous(), &[]).unwrap();
        assert!(
            open_rejoin_knock(&LINEAGE, &plain, &handle).is_err(),
            "no plain fallback"
        );
        let mut tampered = knock.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open_rejoin_knock(&LINEAGE, &tampered, &handle).is_err());
        let mut version = knock;
        version[REJOIN_KNOCK_MAGIC.len()] = 0x02;
        assert!(matches!(
            open_rejoin_knock(&LINEAGE, &version, &handle),
            Err(Error::Unsupported(_))
        ));
    }

    proptest! {
        #[test]
        fn a_sealed_knock_round_trips_under_its_seed_only(
            seed in any::<[u8; 32]>(),
            other in any::<[u8; 32]>(),
            kp in proptest::collection::vec(any::<u8>(), 0..6000),
            with_proof in any::<bool>(),
        ) {
            let handle = fixture_handle();
            let proof = if with_proof { vec![0x11; crate::SELF_REMOVAL_PROOF_LEN] } else { Vec::new() };
            let knock = encode_rejoin_knock(&seed, &handle, &kp, &fixture_rendezvous(), &proof).unwrap();
            prop_assert_eq!((knock.len() - (REJOIN_KNOCK_SEALED_LEN - REJOIN_KNOCK_PAD_STEP)) % REJOIN_KNOCK_PAD_STEP, 0);
            let opened = open_rejoin_knock(&seed, &knock, &handle).unwrap();
            prop_assert_eq!(opened.key_package, kp);
            prop_assert_eq!(opened.self_removal_proof.is_some(), with_proof);
            if other != seed {
                prop_assert!(open_rejoin_knock(&other, &knock, &handle).is_err());
            }
        }

        #[test]
        fn arbitrary_rows_never_open(row in proptest::collection::vec(any::<u8>(), 0..300)) {
            let mut sealed = REJOIN_KNOCK_MAGIC.to_vec();
            sealed.push(REJOIN_KNOCK_VERSION);
            sealed.extend_from_slice(&row);
            prop_assert!(open_rejoin_knock(&LINEAGE, &sealed, &fixture_handle()).is_err());
            prop_assert!(open_rejoin_knock(&LINEAGE, &row, &fixture_handle()).is_err());
        }

        #[test]
        fn malformed_padding_is_refused(pad in proptest::collection::vec(any::<u8>(), 0..64), len in any::<u32>()) {
            let mut plaintext = len.to_be_bytes().to_vec();
            plaintext.extend_from_slice(&pad);
            plaintext.resize(REJOIN_KNOCK_PAD_STEP, 0);
            let ok = usize::try_from(len).is_ok_and(|l| l + 4 <= REJOIN_KNOCK_PAD_STEP);
            let res = unpad_knock(&plaintext);
            if !ok {
                prop_assert!(res.is_err());
            } else if let Ok(p) = res {
                prop_assert_eq!(p.len(), usize::try_from(len).unwrap());
            }
        }
    }

    #[test]
    fn a_plain_payload_carries_a_self_removal_proof_through_decode() {
        let handle = [3u8; 32];
        let proof = vec![0x5a; crate::SELF_REMOVAL_PROOF_LEN];
        let card = vec![0x42; 12];
        let wire =
            encode_with_self_removal_proof(&handle, &[7u8; 64], &[9u8; 32], &card, &proof).unwrap();
        let decoded = decode(&wire, &handle).unwrap();
        assert_eq!(decoded.self_removal_proof, Some(proof));
        assert_eq!(decoded.key_package, vec![7u8; 64]);
        assert_eq!(decoded.device_card, card);
        // An empty proof is the plain payload of `encode`.
        let none =
            encode_with_self_removal_proof(&handle, &[7u8; 64], &[9u8; 32], &card, &[]).unwrap();
        assert_eq!(
            none,
            encode(&handle, &[7u8; 64], &[9u8; 32], &card).unwrap()
        );
        assert_eq!(decode(&none, &handle).unwrap().self_removal_proof, None);
    }

    #[test]
    fn a_plain_payload_refuses_a_proof_of_the_wrong_length() {
        let handle = [3u8; 32];
        for len in [
            1,
            crate::SELF_REMOVAL_PROOF_LEN - 1,
            crate::SELF_REMOVAL_PROOF_LEN + 1,
        ] {
            assert!(
                encode_with_self_removal_proof(
                    &handle,
                    &[7u8; 64],
                    &[9u8; 32],
                    &[],
                    &vec![0u8; len]
                )
                .is_err(),
                "{len} bytes"
            );
        }
        assert!(encode_with_self_removal_proof(
            &handle,
            &[7u8; 64],
            &[9u8; 32],
            &vec![0u8; PAIRING_DEVICE_CARD_MAX_LEN + 1],
            &[0u8; crate::SELF_REMOVAL_PROOF_LEN],
        )
        .is_err());
    }

    #[test]
    fn a_self_removal_proof_of_the_wrong_length_is_refused_both_ways() {
        let handle = fixture_handle();
        assert!(
            encode_rejoin_knock(&LINEAGE, &handle, &[1], &fixture_rendezvous(), &[0u8; 10])
                .is_err()
        );
        for len in [
            1usize,
            64,
            crate::SELF_REMOVAL_PROOF_LEN - 1,
            crate::SELF_REMOVAL_PROOF_LEN + 1,
        ] {
            let buf = encode_raw(&PairingQr {
                version: PAIRING_VERSION,
                key_package: vec![0u8; 32],
                rendezvous_pseudonym: fixture_rendezvous().to_vec(),
                login_handle: handle.to_vec(),
                self_removal_proof: vec![0u8; len],
                device_card: Vec::new(),
            });
            assert!(
                matches!(decode(&buf, &handle), Err(Error::Codec(_))),
                "len {len}"
            );
        }
    }
}
