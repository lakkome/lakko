// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! [`CallInviteCapability`] — link-based group-call invite
//! per ROADMAP 3c3.
//!
//! Sibling of [`crate::invite::InviteCapability`] (the
//! contact-invite capability from ADR-0005 §13.3). Same
//! single-file manual codec + Ed25519-over-the-prefix
//! signature, different payload:
//!
//! - The contact invite seeds a `ContactRequest` back to
//!   the owner so the joiner becomes a contact.
//! - The call invite seeds **a group call**: the joiner
//!   bootstraps a fresh MLS group using the embedded
//!   one-time `KeyPackage` (per the Phase 3c1
//!   [`crate::group_call::GroupCallSession`] design with
//!   `GroupCallScope::LinkBased`) and immediately becomes
//!   a member.
//!
//! Wire flow:
//!
//! ```text
//!  owner Alice                        joiner Bob
//!     │                                   │
//!     │── mint call_id (16 random bytes)  │
//!     │── mint one-time KeyPackage         │
//!     │── compose CallInviteCapability    │
//!     │       (signed by Alice's Ed25519) │
//!     │── publish via https://lakko.me/c/<base64url>
//!     │
//!     │           link out-of-band        │
//!     │ ────────────────────────────────► │
//!     │                                   │
//!     │                                   │ ── decode_and_verify
//!     │                                   │ ── check is_valid_now
//!     │                                   │ ── extract one_time_key_package
//!     │                                   │ ── join_from_welcome(...)
//!     │                                   │    (server-side flow lands in
//!     │                                   │     a later sprint; this
//!     │                                   │     module ships the codec)
//! ```
//!
//! ## What ships in this V1
//!
//! - Wire codec (compose / decode_and_verify)
//! - Signature verification + expiry check
//! - Embedded one-time `KeyPackage` (opaque bytes from
//!   `lakko-mls`; this crate doesn't know the MLS shape)
//! - Media-flags bitmap (audio / video)
//!
//! What's deferred to a follow-up sprint:
//!
//! - `services/discovery /call-invite/{blob_id}` server
//!   endpoint that holds the capability blob + enforces
//!   `max_uses`. V1 puts the bytes directly in the URL
//!   (same trade-off `invite.rs` shipped with).
//! - Joiner-side orchestration that turns the embedded
//!   KP into a live `GroupCallSession`. The codec is
//!   ready; the orchestration belongs in `lakko-calls`
//!   or `apps/*`, not here.
//! - KT-log inclusion proof for `owner_signature_pubkey`
//!   (TOFU until then, same as `invite.rs`).
//!
//! ## Wire format (manual codec, frozen at v1)
//!
//! ```text
//! u8           schema version (= 1)
//! [32 bytes]   owner_signature_pubkey            — Ed25519 verifying key
//! [16 bytes]   owner_account_id                  — UUIDv7 raw
//! [16 bytes]   call_id                           — 16 random bytes
//! u64be        expiry_ms                          — UNIX millis; 0 = no expiry
//! u32be        max_uses                           — 0 = single-use
//! u8           media_flags                        — bit 0 = audio, bit 1 = video
//! u32be        key_package_len                    — bounded by MAX_KEY_PACKAGE_LEN
//! [kp_len]     key_package_bytes                  — opaque MLS KeyPackage wire bytes
//! u16be        label_len
//! [label_len]  label_bytes (UTF-8, ≤ MAX_LABEL_LEN)
//! [64 bytes]   signature                          — Ed25519 over the above
//! ```
//!
//! Multi-byte integers are big-endian. The signature
//! covers everything from the schema-version byte through
//! `label_bytes` inclusive.

use lakko_crypto::signing::{
    Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN as ED25519_PUB_LEN, SIGNATURE_LEN,
};

use crate::error::{ContactError, Result};
use crate::invite::{MAX_LABEL_LEN, NEVER_EXPIRES};
use crate::request::ACCOUNT_ID_LEN;

/// Current schema version on the wire.
const SCHEMA_VERSION: u8 = 1;

/// Length of `call_id` in bytes. Mirror of
/// `lakko_calls::signaling::CallId`'s 16-byte shape.
pub const CALL_ID_LEN: usize = 16;

/// Hard cap on the embedded `KeyPackage` wire bytes. A
/// real OpenMLS `KeyPackage` for the production ciphersuite
/// (`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`) lands
/// around 250–400 bytes; we cap at 4 KiB to keep URL
/// lengths sane and to bound parse-time work without
/// having an exact upstream upper bound to point at.
pub const MAX_KEY_PACKAGE_LEN: usize = 4 * 1024;

/// Re-export the contact-invite constants so callers that
/// only `use crate::call_invite::*` get a coherent surface.
/// `NEVER_EXPIRES` (= 0) means no expiry; `SINGLE_USE` (= 0)
/// means the link can be consumed exactly once.
pub use crate::invite::{NEVER_EXPIRES as CALL_NEVER_EXPIRES, SINGLE_USE as CALL_SINGLE_USE};

/// Media kinds the invited call supports. Wire bitmap:
/// `bit 0 = audio`, `bit 1 = video`. Other bits are
/// reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallInviteMedia {
    /// `true` when audio is supported.
    pub audio: bool,
    /// `true` when video is supported.
    pub video: bool,
}

impl CallInviteMedia {
    /// `audio` + `video`.
    #[must_use]
    pub const fn audio_video() -> Self {
        Self {
            audio: true,
            video: true,
        }
    }

    /// `audio` only.
    #[must_use]
    pub const fn audio_only() -> Self {
        Self {
            audio: true,
            video: false,
        }
    }

    /// Encode to the wire byte. Bit 0 = audio, bit 1 = video.
    #[must_use]
    pub fn into_byte(self) -> u8 {
        let mut bits = 0_u8;
        if self.audio {
            bits |= 0b01;
        }
        if self.video {
            bits |= 0b10;
        }
        bits
    }

    /// Decode from the wire byte. Unknown bits are
    /// silently ignored (forward-compatible).
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        Self {
            audio: b & 0b01 != 0,
            video: b & 0b10 != 0,
        }
    }
}

// ============================================================================
// CallInviteCapability — wire shape
// ============================================================================

/// Capability granted by the link owner to a single joiner
/// (or `max_uses` joiners). Decoded + verified from the
/// base64url-encoded slug at the end of the
/// `https://lakko.me/c/<slug>` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallInviteCapability {
    /// Owner's long-term Ed25519 verifying key. Signature
    /// over the canonical bytes is verified against this.
    pub owner_signature_pubkey: VerifyingKey,
    /// Owner's UUIDv7 raw bytes. The joiner uses this to
    /// route signaling traffic back to the owner once the
    /// call group is bootstrapped.
    pub owner_account_id: [u8; ACCOUNT_ID_LEN],
    /// 16-byte random call id. The owner mints this when
    /// composing the capability; every joiner uses the
    /// **same** id so the group call ledger
    /// (`GroupCallSession`) shares its membership across
    /// all joiners.
    pub call_id: [u8; CALL_ID_LEN],
    /// UNIX milliseconds when the invite stops being valid.
    /// `CALL_NEVER_EXPIRES` (= 0) means no expiry — UI
    /// MUST surface this as a stalking-risk warning.
    pub expiry_ms: u64,
    /// Maximum number of joiners the invite supports.
    /// `CALL_SINGLE_USE` (= 0) is the typical case. V1
    /// keeps this as a hint; server-side enforcement
    /// lands when the discovery endpoint does.
    pub max_uses: u32,
    /// Media flags — audio / video toggles.
    pub media: CallInviteMedia,
    /// Owner's one-time `KeyPackage` wire bytes. The
    /// joiner uses this to bootstrap the call MLS group
    /// with the owner already on board (the joiner adds
    /// themselves as the first call participant after the
    /// owner's KP is consumed). Opaque from this crate's
    /// perspective — interop with `lakko-mls` happens at
    /// the caller.
    pub one_time_key_package: Vec<u8>,
    /// Optional human-visible label, UTF-8, capped at
    /// [`MAX_LABEL_LEN`]. Empty string is allowed.
    pub label: String,
}

impl CallInviteCapability {
    /// `true` iff the capability has not expired at
    /// `now_ms`. `CALL_NEVER_EXPIRES` capabilities always
    /// return `true` (stalking-risk surfacing is the UI's
    /// job).
    #[must_use]
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.expiry_ms == NEVER_EXPIRES || now_ms < self.expiry_ms
    }

    /// Signal-style safety number derived from the owner's
    /// long-term Ed25519 identity key. 71-byte ASCII string
    /// (12 groups of 5 digits, single space between
    /// groups) — same format as
    /// [`crate::safety::safety_number`].
    ///
    /// **Purpose: anti-phishing.** Joiner UI shows this
    /// before letting the human click Accept. The human
    /// compares the displayed digits against what the
    /// owner shared out-of-band (in a Signal chat, on a
    /// business card, over the phone). If they don't
    /// match, the link is from someone else — typically a
    /// substitution attack where an attacker swapped a
    /// real `/c/<slug>` URL for one pointing at their own
    /// account.
    ///
    /// Order-independent on the inputs (the function reuses
    /// the two-party `safety_number(a, b)` with both args
    /// equal to the owner pubkey; deterministic from any
    /// 32-byte key). This is the same Signal-protocol
    /// fingerprint format already used for contact
    /// verification, so the human only learns one shape.
    #[must_use]
    pub fn owner_fingerprint(&self) -> String {
        crate::safety::safety_number(&self.owner_signature_pubkey, &self.owner_signature_pubkey)
    }
}

/// Compose + sign a call-invite capability. The
/// `one_time_key_package` bytes are stored opaque — the
/// caller (a `lakko-mls` consumer) is responsible for
/// minting them.
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when:
///   - `label.len() > MAX_LABEL_LEN`
///   - `one_time_key_package.len() == 0`
///   - `one_time_key_package.len() > MAX_KEY_PACKAGE_LEN`
pub fn compose(
    owner_signing: &SigningKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    call_id: &[u8; CALL_ID_LEN],
    expiry_ms: u64,
    max_uses: u32,
    media: CallInviteMedia,
    one_time_key_package: &[u8],
    label: &str,
) -> Result<Vec<u8>> {
    if label.len() > MAX_LABEL_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "call-invite label is {} bytes, max is {MAX_LABEL_LEN}",
            label.len(),
        )));
    }
    if one_time_key_package.is_empty() {
        return Err(ContactError::InvalidArgument(
            "call-invite one_time_key_package must be non-empty".into(),
        ));
    }
    if one_time_key_package.len() > MAX_KEY_PACKAGE_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "call-invite one_time_key_package is {} bytes, max is {MAX_KEY_PACKAGE_LEN}",
            one_time_key_package.len(),
        )));
    }

    let owner_pubkey = owner_signing.verifying_key();
    let mut wire = encode_signing_prefix(
        &owner_pubkey,
        owner_account_id,
        call_id,
        expiry_ms,
        max_uses,
        media,
        one_time_key_package,
        label,
    );
    let signature = owner_signing.sign(&wire);
    wire.extend_from_slice(&signature.to_bytes());
    Ok(wire)
}

/// Decode + verify a call-invite capability.
///
/// Does **not** check `expiry_ms` — call
/// [`CallInviteCapability::is_valid_now`] separately so the
/// UI can render "this invite expired 3 days ago" rather
/// than a generic failure.
///
/// # Errors
///
/// - [`ContactError::InvalidEncoding`] — wire format
///   malformed, unsupported schema version, EOF, KP length
///   beyond [`MAX_KEY_PACKAGE_LEN`], label beyond
///   [`MAX_LABEL_LEN`], or signature verification failed.
pub fn decode_and_verify(wire: &[u8]) -> Result<CallInviteCapability> {
    let mut cur = wire;
    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported CallInviteCapability schema version {version}",
        )));
    }
    let owner_signature_pubkey_bytes = take_n(&mut cur, ED25519_PUB_LEN)?;
    let owner_account_id_bytes = take_n(&mut cur, ACCOUNT_ID_LEN)?;
    let call_id_bytes = take_n(&mut cur, CALL_ID_LEN)?;
    let expiry_ms_bytes = take_n(&mut cur, 8)?;
    let max_uses_bytes = take_n(&mut cur, 4)?;
    let media_byte = take_u8(&mut cur)?;
    let kp_len_bytes = take_n(&mut cur, 4)?;
    let kp_len = u32::from_be_bytes([
        kp_len_bytes[0],
        kp_len_bytes[1],
        kp_len_bytes[2],
        kp_len_bytes[3],
    ]);
    if kp_len == 0 {
        return Err(ContactError::InvalidEncoding(
            "call-invite key_package_len is zero".into(),
        ));
    }
    let kp_len_usize = usize::try_from(kp_len).map_err(|_| {
        ContactError::InvalidEncoding("call-invite key_package_len overflows usize".into())
    })?;
    if kp_len_usize > MAX_KEY_PACKAGE_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "call-invite key_package_len {kp_len_usize} > MAX_KEY_PACKAGE_LEN {MAX_KEY_PACKAGE_LEN}"
        )));
    }
    let key_package = take_n(&mut cur, kp_len_usize)?.to_vec();
    let label_len_bytes = take_n(&mut cur, 2)?;
    let label_len = u16::from_be_bytes([label_len_bytes[0], label_len_bytes[1]]);
    let label_len_usize = usize::from(label_len);
    if label_len_usize > MAX_LABEL_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "call-invite label_len {label_len_usize} > MAX_LABEL_LEN {MAX_LABEL_LEN}"
        )));
    }
    let label_bytes = take_n(&mut cur, label_len_usize)?.to_vec();
    let signature_bytes = take_n(&mut cur, SIGNATURE_LEN)?;
    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "trailing {} bytes after call-invite signature",
            cur.len(),
        )));
    }

    // Verify the Ed25519 signature against the embedded
    // public key. The signed bytes are everything before
    // the signature itself.
    let signing_input = &wire[..wire.len() - SIGNATURE_LEN];
    let owner_pubkey = VerifyingKey::from_bytes(owner_signature_pubkey_bytes).map_err(|_| {
        ContactError::InvalidEncoding("call-invite owner_signature_pubkey malformed".into())
    })?;
    let sig = Signature::from_bytes(signature_bytes)
        .map_err(|_| ContactError::InvalidEncoding("call-invite signature length wrong".into()))?;
    owner_pubkey
        .verify(signing_input, &sig)
        .map_err(|_| ContactError::InvalidEncoding("call-invite signature invalid".into()))?;

    let mut owner_account_id = [0_u8; ACCOUNT_ID_LEN];
    owner_account_id.copy_from_slice(owner_account_id_bytes);
    let mut call_id = [0_u8; CALL_ID_LEN];
    call_id.copy_from_slice(call_id_bytes);
    let expiry_ms = u64::from_be_bytes([
        expiry_ms_bytes[0],
        expiry_ms_bytes[1],
        expiry_ms_bytes[2],
        expiry_ms_bytes[3],
        expiry_ms_bytes[4],
        expiry_ms_bytes[5],
        expiry_ms_bytes[6],
        expiry_ms_bytes[7],
    ]);
    let max_uses = u32::from_be_bytes([
        max_uses_bytes[0],
        max_uses_bytes[1],
        max_uses_bytes[2],
        max_uses_bytes[3],
    ]);
    let media = CallInviteMedia::from_byte(media_byte);
    let label = String::from_utf8(label_bytes)
        .map_err(|e| ContactError::InvalidEncoding(format!("call-invite label not UTF-8: {e}")))?;

    Ok(CallInviteCapability {
        owner_signature_pubkey: owner_pubkey,
        owner_account_id,
        call_id,
        expiry_ms,
        max_uses,
        media,
        one_time_key_package: key_package,
        label,
    })
}

// ============================================================================
// Internal codec helpers
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn encode_signing_prefix(
    owner_signature_pubkey: &VerifyingKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    call_id: &[u8; CALL_ID_LEN],
    expiry_ms: u64,
    max_uses: u32,
    media: CallInviteMedia,
    one_time_key_package: &[u8],
    label: &str,
) -> Vec<u8> {
    let label_len_u16 =
        u16::try_from(label.len()).expect("label fits in u16; caller checked MAX_LABEL_LEN");
    let kp_len_u32 = u32::try_from(one_time_key_package.len())
        .expect("kp fits in u32; caller checked MAX_KEY_PACKAGE_LEN");
    let mut out = Vec::with_capacity(
        1 + ED25519_PUB_LEN
            + ACCOUNT_ID_LEN
            + CALL_ID_LEN
            + 8
            + 4
            + 1
            + 4
            + one_time_key_package.len()
            + 2
            + label.len(),
    );
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&owner_signature_pubkey.to_bytes());
    out.extend_from_slice(owner_account_id);
    out.extend_from_slice(call_id);
    out.extend_from_slice(&expiry_ms.to_be_bytes());
    out.extend_from_slice(&max_uses.to_be_bytes());
    out.push(media.into_byte());
    out.extend_from_slice(&kp_len_u32.to_be_bytes());
    out.extend_from_slice(one_time_key_package);
    out.extend_from_slice(&label_len_u16.to_be_bytes());
    out.extend_from_slice(label.as_bytes());
    out
}

fn take_u8(cur: &mut &[u8]) -> Result<u8> {
    if cur.is_empty() {
        return Err(ContactError::InvalidEncoding("unexpected EOF".to_string()));
    }
    let b = cur[0];
    *cur = &cur[1..];
    Ok(b)
}

fn take_n<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if cur.len() < n {
        return Err(ContactError::InvalidEncoding(format!(
            "need {n} bytes, have {}",
            cur.len(),
        )));
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_signing() -> SigningKey {
        SigningKey::from_seed(&[0xAB; 32])
    }

    fn fixture_account_id() -> [u8; ACCOUNT_ID_LEN] {
        let mut id = [0_u8; ACCOUNT_ID_LEN];
        id[0] = 0x01;
        id[15] = 0xAB;
        id
    }

    fn fixture_call_id() -> [u8; CALL_ID_LEN] {
        let mut id = [0_u8; CALL_ID_LEN];
        id[0] = 0x10;
        id[15] = 0x10;
        id
    }

    fn fixture_kp() -> Vec<u8> {
        // Production OpenMLS KeyPackage is ~ 300–400 bytes
        // of TLS-codec'd wire; we use a fixture pattern
        // that's recognisable in hex dumps.
        let mut kp = vec![0x42; 256];
        kp[0] = 0xCA;
        kp[1] = 0xFE;
        kp
    }

    #[test]
    fn compose_decode_round_trips() {
        let signing = fixture_signing();
        let account_id = fixture_account_id();
        let call_id = fixture_call_id();
        let kp = fixture_kp();
        let label = "Alice's standup";

        let wire = compose(
            &signing,
            &account_id,
            &call_id,
            1_700_000_000_000,
            5,
            CallInviteMedia::audio_video(),
            &kp,
            label,
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();

        assert_eq!(
            parsed.owner_signature_pubkey.to_bytes(),
            signing.verifying_key().to_bytes()
        );
        assert_eq!(parsed.owner_account_id, account_id);
        assert_eq!(parsed.call_id, call_id);
        assert_eq!(parsed.expiry_ms, 1_700_000_000_000);
        assert_eq!(parsed.max_uses, 5);
        assert_eq!(parsed.media, CallInviteMedia::audio_video());
        assert_eq!(parsed.one_time_key_package, kp);
        assert_eq!(parsed.label, label);
    }

    #[test]
    fn audio_only_media_round_trips() {
        let wire = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            CALL_SINGLE_USE,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert_eq!(parsed.media, CallInviteMedia::audio_only());
    }

    #[test]
    fn never_expires_is_valid_at_any_time() {
        let wire = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            CALL_NEVER_EXPIRES,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert!(parsed.is_valid_now(0));
        assert!(parsed.is_valid_now(u64::MAX));
    }

    #[test]
    fn expired_capability_is_invalid_after_expiry() {
        let wire = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            1_000,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert!(parsed.is_valid_now(999));
        assert!(!parsed.is_valid_now(1_000));
        assert!(!parsed.is_valid_now(2_000));
    }

    #[test]
    fn tampered_payload_rejected() {
        let mut wire = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_video(),
            &fixture_kp(),
            "x",
        )
        .unwrap();
        // Flip a byte inside the KP region (~position 60).
        wire[60] ^= 0xFF;
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("signature"), "got: {msg}");
            },
            other => panic!("expected signature-invalid, got {other:?}"),
        }
    }

    #[test]
    fn truncated_wire_rejected() {
        let wire = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        // Drop the trailing signature.
        let truncated = &wire[..wire.len() - SIGNATURE_LEN - 1];
        assert!(decode_and_verify(truncated).is_err());
    }

    #[test]
    fn unsupported_schema_version_rejected() {
        let mut wire = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        wire[0] = 2; // bump version byte
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("schema version 2"), "got: {msg}");
            },
            other => panic!("expected unsupported-version, got {other:?}"),
        }
    }

    #[test]
    fn empty_key_package_rejected_at_compose() {
        let err = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &[],
            "",
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn oversize_key_package_rejected_at_compose() {
        let too_big = vec![0_u8; MAX_KEY_PACKAGE_LEN + 1];
        let err = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &too_big,
            "",
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn oversize_label_rejected_at_compose() {
        let too_long = "x".repeat(MAX_LABEL_LEN + 1);
        let err = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            &too_long,
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn cross_signing_key_rejected() {
        // Sign with one key, replace the embedded pubkey
        // with another's — signature verify must fail.
        let signing_a = SigningKey::from_seed(&[0xAA; 32]);
        let signing_b = SigningKey::from_seed(&[0xBB; 32]);
        let mut wire = compose(
            &signing_a,
            &fixture_account_id(),
            &fixture_call_id(),
            0,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "",
        )
        .unwrap();
        // Splice B's pubkey at the schema-version + 1
        // offset. The signature was computed over A's
        // pubkey, so swapping in B's must invalidate it.
        let b_pubkey = signing_b.verifying_key().to_bytes();
        wire[1..=ED25519_PUB_LEN].copy_from_slice(&b_pubkey);
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("signature"), "got: {msg}");
            },
            other => panic!("expected signature-invalid, got {other:?}"),
        }
    }

    #[test]
    fn owner_fingerprint_is_deterministic_per_pubkey() {
        // Same pubkey → same fingerprint, regardless of
        // the capability's other fields.
        let cap_wire_a = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &fixture_call_id(),
            1_000,
            5,
            CallInviteMedia::audio_video(),
            &fixture_kp(),
            "Alice",
        )
        .unwrap();
        let mut another_call_id = fixture_call_id();
        another_call_id[0] = 0xFF;
        let cap_wire_b = compose(
            &fixture_signing(),
            &fixture_account_id(),
            &another_call_id,
            999_999,
            0,
            CallInviteMedia::audio_only(),
            &fixture_kp(),
            "Different label",
        )
        .unwrap();
        let cap_a = decode_and_verify(&cap_wire_a).unwrap();
        let cap_b = decode_and_verify(&cap_wire_b).unwrap();
        assert_eq!(
            cap_a.owner_fingerprint(),
            cap_b.owner_fingerprint(),
            "fingerprint must depend ONLY on the owner pubkey"
        );
    }

    #[test]
    fn different_owners_have_different_fingerprints() {
        let signing_a = SigningKey::from_seed(&[0xAA; 32]);
        let signing_b = SigningKey::from_seed(&[0xBB; 32]);
        let cap_a = decode_and_verify(
            &compose(
                &signing_a,
                &fixture_account_id(),
                &fixture_call_id(),
                0,
                0,
                CallInviteMedia::audio_only(),
                &fixture_kp(),
                "",
            )
            .unwrap(),
        )
        .unwrap();
        let cap_b = decode_and_verify(
            &compose(
                &signing_b,
                &fixture_account_id(),
                &fixture_call_id(),
                0,
                0,
                CallInviteMedia::audio_only(),
                &fixture_kp(),
                "",
            )
            .unwrap(),
        )
        .unwrap();
        assert_ne!(cap_a.owner_fingerprint(), cap_b.owner_fingerprint());
    }

    #[test]
    fn owner_fingerprint_matches_safety_number_format() {
        let cap = decode_and_verify(
            &compose(
                &fixture_signing(),
                &fixture_account_id(),
                &fixture_call_id(),
                0,
                0,
                CallInviteMedia::audio_only(),
                &fixture_kp(),
                "",
            )
            .unwrap(),
        )
        .unwrap();
        let fingerprint = cap.owner_fingerprint();
        // 12 groups × 5 digits + 11 spaces = 71 chars.
        assert_eq!(fingerprint.len(), 71);
        // Each chunk must be ASCII digits.
        for chunk in fingerprint.split(' ') {
            assert_eq!(chunk.len(), 5);
            assert!(chunk.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn media_byte_codec_is_stable() {
        for case in [
            CallInviteMedia {
                audio: false,
                video: false,
            },
            CallInviteMedia::audio_only(),
            CallInviteMedia::audio_video(),
            CallInviteMedia {
                audio: false,
                video: true,
            },
        ] {
            assert_eq!(CallInviteMedia::from_byte(case.into_byte()), case);
        }
    }

    #[test]
    fn unknown_media_bits_are_silently_ignored() {
        // Forward-compatibility — a future schema bump
        // adding bit 2 must not break v1 decoders.
        let decoded = CallInviteMedia::from_byte(0b1111_1111);
        assert!(decoded.audio);
        assert!(decoded.video);
    }
}
