// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `ChannelPost` — the wire object a publisher emits.
//!
//! A post is the concatenation of:
//!
//! 1. Header — `(epoch_id, sender_pubkey, message_counter)`.
//! 2. AEAD ciphertext over the plaintext, with AAD bound to the
//!    header and to the `channel_id`.
//! 3. Ed25519 signature over the same byte string the AAD commits
//!    to, plus the ciphertext.
//!
//! ## Why an Ed25519 signature on top of an AEAD
//!
//! The AEAD authenticates the ciphertext against *anyone who knows
//! the seed*. The Ed25519 signature authenticates against the
//! sender's identity key — a property the seed cannot offer
//! because every publisher in the PublisherGroup shares the same
//! seed. Without the signature, publisher Bob could craft a
//! ciphertext that decrypts cleanly but appears to be from
//! publisher Alice; the subscriber would not detect the forgery.
//! Adding the signature costs 64 bytes per post and one Ed25519
//! verify per receive.
//!
//! ## Wire layout (frozen at v1)
//!
//! ```text
//!   offset  size  field
//!   ─────────────────────────────────────────────
//!   0       4     epoch_id              (u32 BE)
//!   4       32    sender_pubkey         (Ed25519)
//!   36      8     message_counter       (u64 BE)
//!   44      4     ciphertext_len        (u32 BE)
//!   48      *     ciphertext            (incl. 16-byte tag)
//!   ?       64    signature             (Ed25519)
//! ```
//!
//! The signature covers
//! `channel_id || epoch_id_be || sender_pubkey || counter_be || ciphertext`
//! — i.e. all addressable header fields plus the ciphertext. The
//! AEAD's AAD covers
//! `channel_id || epoch_id_be || sender_pubkey || counter_be`.
//! Both share the same prefix on purpose: tampering with any
//! header field invalidates both the signature **and** the AEAD
//! authenticator, so an attacker who can flip just one of the two
//! still loses.

use lakko_crypto::aead::{self, Algorithm, KEY_LEN, NONCE_LEN};
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};

use crate::epoch::{EpochId, EPOCH_ID_LEN};
use crate::error::{ChannelError, Result};
use crate::seed::ContentKeySeed;
use crate::CHANNEL_ID_LEN;

/// Domain-separator for the Ed25519 signature on a `ChannelPost`,
/// frozen at v1.
pub const POST_SIG_DOMAIN_V1: &[u8] = b"lakko/channel/v1/post";

/// Maximum plaintext length for a single post — 16 MiB.
///
/// ChaCha20-Poly1305 supports up to 2^32 − 1 blocks (~256 GiB);
/// the cap here is a sanity guard so the FFI surface can refuse
/// pathologically-large posts without ambiguity. Attachments
/// belong in object storage with content-addressed pointers — the
/// post text is for the message body itself.
pub const MAX_PLAINTEXT_LEN: usize = 16 * 1024 * 1024;

/// `u64` width of the per-message monotone counter on the wire.
const COUNTER_LEN: usize = 8;

/// Constant-time width of the ciphertext-length prefix on the wire.
const CIPHERTEXT_LEN_PREFIX: usize = 4;

/// Minimum encoded post length: header (44) + smallest possible
/// ciphertext (16-byte AEAD tag alone) + signature (64).
const MIN_ENCODED_LEN: usize = EPOCH_ID_LEN
    + PUBLIC_KEY_LEN
    + COUNTER_LEN
    + CIPHERTEXT_LEN_PREFIX
    + aead::TAG_LEN
    + SIGNATURE_LEN;

/// HKDF info labels — pinned at v1 so a schema bump is loud.
const HKDF_INFO_KEY: &[u8] = b"lakko/channel/v1/msg-key";
const HKDF_INFO_NONCE: &[u8] = b"lakko/channel/v1/msg-nonce";

/// A signed, AEAD-encrypted post on a channel.
///
/// Borrows nothing; ownership is the natural fit because posts are
/// long-lived (cached by subscribers) and small relative to the
/// crypto cost of producing them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelPost {
    /// Epoch the post was encrypted under.
    pub epoch_id: EpochId,
    /// Publisher's Ed25519 public key.
    pub sender_pubkey: [u8; PUBLIC_KEY_LEN],
    /// Monotone per-`(channel_id, epoch_id, sender_pubkey)` counter.
    pub message_counter: u64,
    /// AEAD ciphertext (includes 16-byte Poly1305 tag).
    pub ciphertext: Vec<u8>,
    /// Ed25519 signature over the canonical byte string described
    /// in the module-level docs.
    pub signature: [u8; SIGNATURE_LEN],
}

impl ChannelPost {
    /// Encode `self` as a binary blob in the layout pinned at the
    /// top of this module.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let ct_len = self.ciphertext.len();
        let mut out = Vec::with_capacity(
            EPOCH_ID_LEN
                + PUBLIC_KEY_LEN
                + COUNTER_LEN
                + CIPHERTEXT_LEN_PREFIX
                + ct_len
                + SIGNATURE_LEN,
        );
        out.extend_from_slice(&self.epoch_id.to_be_bytes());
        out.extend_from_slice(&self.sender_pubkey);
        out.extend_from_slice(&self.message_counter.to_be_bytes());
        // Safe cast: the composer rejects oversize plaintexts and
        // an AEAD seal cannot grow ciphertext past `u32::MAX`.
        let ct_len_u32 = u32::try_from(ct_len).unwrap_or(u32::MAX);
        out.extend_from_slice(&ct_len_u32.to_be_bytes());
        out.extend_from_slice(&self.ciphertext);
        out.extend_from_slice(&self.signature);
        out
    }
}

/// Compose a signed encrypted post.
///
/// `signing_key.verifying_key()` MUST match the identity the
/// publisher is publishing under — we use it for the
/// `sender_pubkey` field of the post header. `message_counter`
/// MUST be strictly monotone within `(channel_id, epoch_id,
/// sender_pubkey)`; the crate cannot enforce this because the
/// counter lives in client storage between sessions.
///
/// # Errors
///
/// - [`ChannelError::PlaintextTooLong`] if `plaintext.len() >
///   MAX_PLAINTEXT_LEN`.
/// - [`ChannelError::KeyDerivation`] on HKDF failure (impossible
///   with current params, surfaced for completeness).
/// - [`ChannelError::AeadSeal`] on AEAD seal failure (likewise).
pub fn compose_post(
    seed: &ContentKeySeed,
    channel_id: &[u8; CHANNEL_ID_LEN],
    epoch_id: EpochId,
    message_counter: u64,
    signing_key: &SigningKey,
    plaintext: &[u8],
) -> Result<ChannelPost> {
    if plaintext.len() > MAX_PLAINTEXT_LEN {
        return Err(ChannelError::PlaintextTooLong {
            max: MAX_PLAINTEXT_LEN,
            got: plaintext.len(),
        });
    }
    let sender_pubkey = signing_key.verifying_key().to_bytes();

    let (key, nonce) =
        derive_key_and_nonce(seed, channel_id, epoch_id, &sender_pubkey, message_counter)?;
    let aad = aead_aad(channel_id, epoch_id, &sender_pubkey, message_counter);
    let ciphertext = aead::seal(Algorithm::ChaCha20Poly1305, &key, &nonce, &aad, plaintext)
        .map_err(|_| ChannelError::AeadSeal)?;

    let sig_input = signature_input(
        channel_id,
        epoch_id,
        &sender_pubkey,
        message_counter,
        &ciphertext,
    );
    let signature = signing_key.sign(&sig_input).to_bytes();

    Ok(ChannelPost {
        epoch_id,
        sender_pubkey,
        message_counter,
        ciphertext,
        signature,
    })
}

/// Parse a `ChannelPost` from its on-wire bytes.
///
/// Does not validate the signature or attempt to decrypt — those
/// are the caller's responsibility via [`verify_and_decrypt`]. The
/// split lets a subscriber stash undecryptable posts (e.g. seed
/// for the matching epoch hasn't arrived yet) and replay them
/// later without losing fidelity.
///
/// # Errors
///
/// Returns [`ChannelError::Wire`] on any truncation or length
/// inconsistency.
pub fn decode_post(bytes: &[u8]) -> Result<ChannelPost> {
    if bytes.len() < MIN_ENCODED_LEN {
        return Err(ChannelError::Wire("post shorter than minimum header"));
    }

    let mut cursor = 0usize;

    let epoch_id = EpochId::from_be_bytes(&bytes[cursor..cursor + EPOCH_ID_LEN])?;
    cursor += EPOCH_ID_LEN;

    let mut sender_pubkey = [0u8; PUBLIC_KEY_LEN];
    sender_pubkey.copy_from_slice(&bytes[cursor..cursor + PUBLIC_KEY_LEN]);
    cursor += PUBLIC_KEY_LEN;

    let counter_bytes: [u8; COUNTER_LEN] = bytes[cursor..cursor + COUNTER_LEN]
        .try_into()
        .map_err(|_| ChannelError::Wire("counter slice"))?;
    let message_counter = u64::from_be_bytes(counter_bytes);
    cursor += COUNTER_LEN;

    let ct_len_bytes: [u8; CIPHERTEXT_LEN_PREFIX] = bytes[cursor..cursor + CIPHERTEXT_LEN_PREFIX]
        .try_into()
        .map_err(|_| ChannelError::Wire("ciphertext length prefix"))?;
    let ct_len = u32::from_be_bytes(ct_len_bytes) as usize;
    cursor += CIPHERTEXT_LEN_PREFIX;

    if ct_len < aead::TAG_LEN {
        return Err(ChannelError::Wire(
            "ciphertext shorter than 16-byte AEAD tag",
        ));
    }
    if bytes.len() < cursor + ct_len + SIGNATURE_LEN {
        return Err(ChannelError::Wire("post truncated before signature"));
    }

    let ciphertext = bytes[cursor..cursor + ct_len].to_vec();
    cursor += ct_len;

    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(&bytes[cursor..cursor + SIGNATURE_LEN]);
    cursor += SIGNATURE_LEN;

    if cursor != bytes.len() {
        return Err(ChannelError::Wire("trailing bytes after signature"));
    }

    Ok(ChannelPost {
        epoch_id,
        sender_pubkey,
        message_counter,
        ciphertext,
        signature,
    })
}

/// Verify a post's Ed25519 signature, then AEAD-open the ciphertext.
///
/// Returns the plaintext on full success. The signature is checked
/// **before** the AEAD open so a forged-by-an-insider post is
/// rejected without touching the cipher — both belt and braces.
///
/// # Errors
///
/// - [`ChannelError::BadSenderKey`] if the post's `sender_pubkey`
///   is not a valid Ed25519 point.
/// - [`ChannelError::BadSignature`] if the signature does not
///   verify.
/// - [`ChannelError::AeadOpen`] if the AEAD authenticator rejects
///   the ciphertext (wrong seed, AAD mismatch, tampered bytes).
pub fn verify_and_decrypt(
    seed: &ContentKeySeed,
    channel_id: &[u8; CHANNEL_ID_LEN],
    post: &ChannelPost,
) -> Result<Vec<u8>> {
    let verifying_key =
        VerifyingKey::from_bytes(&post.sender_pubkey).map_err(|_| ChannelError::BadSenderKey)?;

    let sig_input = signature_input(
        channel_id,
        post.epoch_id,
        &post.sender_pubkey,
        post.message_counter,
        &post.ciphertext,
    );
    let signature =
        Signature::from_bytes(&post.signature).map_err(|_| ChannelError::BadSignature)?;
    verifying_key
        .verify(&sig_input, &signature)
        .map_err(|_| ChannelError::BadSignature)?;

    let (key, nonce) = derive_key_and_nonce(
        seed,
        channel_id,
        post.epoch_id,
        &post.sender_pubkey,
        post.message_counter,
    )?;
    let aad = aead_aad(
        channel_id,
        post.epoch_id,
        &post.sender_pubkey,
        post.message_counter,
    );
    aead::open(
        Algorithm::ChaCha20Poly1305,
        &key,
        &nonce,
        &aad,
        &post.ciphertext,
    )
    .map_err(|_| ChannelError::AeadOpen)
}

// ----------------------------------------------------------------
// Internal helpers — kept private so the HKDF / AAD contract is
// one source of truth.
// ----------------------------------------------------------------

fn aead_aad(
    channel_id: &[u8; CHANNEL_ID_LEN],
    epoch_id: EpochId,
    sender_pubkey: &[u8; PUBLIC_KEY_LEN],
    message_counter: u64,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(CHANNEL_ID_LEN + EPOCH_ID_LEN + PUBLIC_KEY_LEN + COUNTER_LEN);
    aad.extend_from_slice(channel_id);
    aad.extend_from_slice(&epoch_id.to_be_bytes());
    aad.extend_from_slice(sender_pubkey);
    aad.extend_from_slice(&message_counter.to_be_bytes());
    aad
}

fn signature_input(
    channel_id: &[u8; CHANNEL_ID_LEN],
    epoch_id: EpochId,
    sender_pubkey: &[u8; PUBLIC_KEY_LEN],
    message_counter: u64,
    ciphertext: &[u8],
) -> Vec<u8> {
    let mut input = Vec::with_capacity(
        POST_SIG_DOMAIN_V1.len()
            + 1 // domain separator NUL terminator
            + CHANNEL_ID_LEN
            + EPOCH_ID_LEN
            + PUBLIC_KEY_LEN
            + COUNTER_LEN
            + ciphertext.len(),
    );
    input.extend_from_slice(POST_SIG_DOMAIN_V1);
    input.push(0u8);
    input.extend_from_slice(channel_id);
    input.extend_from_slice(&epoch_id.to_be_bytes());
    input.extend_from_slice(sender_pubkey);
    input.extend_from_slice(&message_counter.to_be_bytes());
    input.extend_from_slice(ciphertext);
    input
}

fn derive_key_and_nonce(
    seed: &ContentKeySeed,
    channel_id: &[u8; CHANNEL_ID_LEN],
    epoch_id: EpochId,
    sender_pubkey: &[u8; PUBLIC_KEY_LEN],
    message_counter: u64,
) -> Result<(SecretBytes<KEY_LEN>, [u8; NONCE_LEN])> {
    // HKDF salt for the key binds (channel, epoch, sender) — see
    // ADR-0010 § "message_key" derivation.
    let mut key_salt = Vec::with_capacity(CHANNEL_ID_LEN + EPOCH_ID_LEN + PUBLIC_KEY_LEN);
    key_salt.extend_from_slice(channel_id);
    key_salt.extend_from_slice(&epoch_id.to_be_bytes());
    key_salt.extend_from_slice(sender_pubkey);
    let key = hkdf_sha256::<KEY_LEN>(seed.as_inner_bytes(), &key_salt, HKDF_INFO_KEY)
        .map_err(|_| ChannelError::KeyDerivation)?;

    // HKDF salt for the nonce additionally binds the message
    // counter so two posts from the same sender in the same epoch
    // never collide on their nonce.
    let mut nonce_salt =
        Vec::with_capacity(CHANNEL_ID_LEN + EPOCH_ID_LEN + PUBLIC_KEY_LEN + COUNTER_LEN);
    nonce_salt.extend_from_slice(channel_id);
    nonce_salt.extend_from_slice(&epoch_id.to_be_bytes());
    nonce_salt.extend_from_slice(sender_pubkey);
    nonce_salt.extend_from_slice(&message_counter.to_be_bytes());
    let nonce_secret =
        hkdf_sha256::<NONCE_LEN>(seed.as_inner_bytes(), &nonce_salt, HKDF_INFO_NONCE)
            .map_err(|_| ChannelError::KeyDerivation)?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(nonce_secret.as_bytes());

    Ok((key, nonce))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_seed() -> ContentKeySeed {
        ContentKeySeed::from_bytes_for_test([0x42; CONTENT_KEY_SEED_LEN_LOCAL])
    }
    const CONTENT_KEY_SEED_LEN_LOCAL: usize = 32;
    const CHANNEL: [u8; CHANNEL_ID_LEN] = *b"_lakko_channel_x";

    #[test]
    fn roundtrip_through_compose_and_verify() {
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let post = compose_post(
            &seed,
            &CHANNEL,
            EpochId::new(7),
            42,
            &signing,
            b"hello channel",
        )
        .unwrap();

        let pt = verify_and_decrypt(&seed, &CHANNEL, &post).unwrap();
        assert_eq!(pt, b"hello channel");
    }

    #[test]
    fn wrong_seed_fails_aead() {
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let other_seed = ContentKeySeed::from_bytes_for_test([0xAA; CONTENT_KEY_SEED_LEN_LOCAL]);
        let post = compose_post(&seed, &CHANNEL, EpochId::new(1), 0, &signing, b"hi").unwrap();
        let err = verify_and_decrypt(&other_seed, &CHANNEL, &post).unwrap_err();
        assert!(matches!(err, ChannelError::AeadOpen));
    }

    #[test]
    fn wrong_channel_id_fails_signature() {
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let other_channel = *b"OTHER_CHANNELID_";
        let post = compose_post(&seed, &CHANNEL, EpochId::new(1), 0, &signing, b"hi").unwrap();
        let err = verify_and_decrypt(&seed, &other_channel, &post).unwrap_err();
        // The Ed25519 signature input covers channel_id, so a
        // verifier replaying the post under the wrong channel
        // hits the signature check first — the AEAD layer never
        // sees the post. This is the belt-and-braces property
        // promised in the module docs.
        assert!(matches!(err, ChannelError::BadSignature));
    }

    #[test]
    fn tampered_ciphertext_fails_signature() {
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let mut post = compose_post(&seed, &CHANNEL, EpochId::new(1), 0, &signing, b"hi").unwrap();
        // Flip a bit in the ciphertext. The signature payload
        // covers the ciphertext, so the Ed25519 verify catches
        // this before AEAD even runs.
        post.ciphertext[0] ^= 0x01;
        let err = verify_and_decrypt(&seed, &CHANNEL, &post).unwrap_err();
        assert!(matches!(err, ChannelError::BadSignature));
    }

    #[test]
    fn tampered_signature_fails_signature() {
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let mut post = compose_post(&seed, &CHANNEL, EpochId::new(1), 0, &signing, b"hi").unwrap();
        post.signature[0] ^= 0x01;
        let err = verify_and_decrypt(&seed, &CHANNEL, &post).unwrap_err();
        assert!(matches!(err, ChannelError::BadSignature));
    }

    #[test]
    fn wire_roundtrip() {
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let post =
            compose_post(&seed, &CHANNEL, EpochId::new(99), 7, &signing, b"wire-rt").unwrap();
        let bytes = post.to_bytes();
        let parsed = decode_post(&bytes).unwrap();
        assert_eq!(parsed, post);
        let pt = verify_and_decrypt(&seed, &CHANNEL, &parsed).unwrap();
        assert_eq!(pt, b"wire-rt");
    }

    #[test]
    fn decode_rejects_short_buffer() {
        let err = decode_post(&[0u8; 10]).unwrap_err();
        assert!(matches!(err, ChannelError::Wire(_)));
    }

    #[test]
    fn decode_rejects_trailing_garbage() {
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let post = compose_post(&seed, &CHANNEL, EpochId::new(1), 0, &signing, b"x").unwrap();
        let mut bytes = post.to_bytes();
        bytes.push(0xFF);
        let err = decode_post(&bytes).unwrap_err();
        assert!(matches!(err, ChannelError::Wire(_)));
    }

    #[test]
    fn decode_rejects_ciphertext_under_tag() {
        // Forge a post whose ciphertext_len < AEAD tag length.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0u8; EPOCH_ID_LEN]);
        bytes.extend_from_slice(&[0u8; PUBLIC_KEY_LEN]);
        bytes.extend_from_slice(&[0u8; COUNTER_LEN]);
        bytes.extend_from_slice(&5u32.to_be_bytes()); // < 16
        bytes.extend_from_slice(&[0u8; 5]);
        bytes.extend_from_slice(&[0u8; SIGNATURE_LEN]);
        let err = decode_post(&bytes).unwrap_err();
        assert!(matches!(err, ChannelError::Wire(_)));
    }

    #[test]
    fn distinct_senders_get_distinct_keys() {
        // Two publishers in the same PublisherGroup share the seed
        // but the HKDF salt binds in their pubkey, so their
        // per-message keys differ. The acid test: a post from
        // Alice cannot be decrypted as if it came from Bob even
        // if you swap the signature.
        let alice = SigningKey::generate();
        let bob = SigningKey::generate();
        let seed = fixture_seed();
        let post_alice =
            compose_post(&seed, &CHANNEL, EpochId::new(1), 0, &alice, b"from alice").unwrap();
        let mut forged = post_alice.clone();
        forged.sender_pubkey = bob.verifying_key().to_bytes();
        // Re-sign with bob so signature passes; AEAD must still
        // fail because the key was derived under alice's pubkey.
        let sig_input = signature_input(
            &CHANNEL,
            forged.epoch_id,
            &forged.sender_pubkey,
            forged.message_counter,
            &forged.ciphertext,
        );
        forged.signature = bob.sign(&sig_input).to_bytes();
        let err = verify_and_decrypt(&seed, &CHANNEL, &forged).unwrap_err();
        assert!(matches!(err, ChannelError::AeadOpen));
    }

    #[test]
    fn distinct_counters_in_same_epoch_get_distinct_nonces() {
        // Implicit invariant — if counter wasn't in the nonce
        // salt, two posts in the same epoch from the same sender
        // would collide on AEAD nonce, which is catastrophic for
        // ChaCha20-Poly1305 (key recovery). We assert that the
        // ciphertexts differ even when plaintexts are identical.
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let post_0 = compose_post(&seed, &CHANNEL, EpochId::new(1), 0, &signing, b"same").unwrap();
        let post_1 = compose_post(&seed, &CHANNEL, EpochId::new(1), 1, &signing, b"same").unwrap();
        assert_ne!(post_0.ciphertext, post_1.ciphertext);
    }

    #[test]
    fn epoch_advance_changes_keys() {
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let e1 = compose_post(&seed, &CHANNEL, EpochId::new(1), 0, &signing, b"same").unwrap();
        let e2 = compose_post(&seed, &CHANNEL, EpochId::new(2), 0, &signing, b"same").unwrap();
        assert_ne!(e1.ciphertext, e2.ciphertext);
        // And cross-epoch decryption fails: the e1 ciphertext
        // does not open with the key the e2 HKDF salt produces.
        let mut swapped = e1.clone();
        swapped.epoch_id = EpochId::new(2);
        // Re-sign so the signature passes; we want to land at
        // the AEAD-open rejection, not the signature one.
        let sig_input = signature_input(
            &CHANNEL,
            swapped.epoch_id,
            &swapped.sender_pubkey,
            swapped.message_counter,
            &swapped.ciphertext,
        );
        swapped.signature = signing.sign(&sig_input).to_bytes();
        let err = verify_and_decrypt(&seed, &CHANNEL, &swapped).unwrap_err();
        assert!(matches!(err, ChannelError::AeadOpen));
    }

    #[test]
    fn rejects_oversize_plaintext() {
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let big = vec![0u8; MAX_PLAINTEXT_LEN + 1];
        let err = compose_post(&seed, &CHANNEL, EpochId::new(1), 0, &signing, &big).unwrap_err();
        assert!(matches!(err, ChannelError::PlaintextTooLong { .. }));
    }

    #[test]
    fn invalid_sender_pubkey_rejected() {
        let signing = SigningKey::generate();
        let seed = fixture_seed();
        let mut post = compose_post(&seed, &CHANNEL, EpochId::new(1), 0, &signing, b"hi").unwrap();
        // Swap in 32 bytes that did not produce the signature.
        // Two valid rejection paths cover this: either dalek
        // refuses to parse the bytes as a verifying key
        // (`BadSenderKey`) or the bytes parse but the signature
        // does not verify (`BadSignature`). Both outcomes prevent
        // decryption, which is the security property we care
        // about. dalek 2.x is lenient on some malformed encodings
        // and surfaces them as a verification failure rather than
        // a parsing failure, so we accept either variant here.
        post.sender_pubkey = [0xFF; PUBLIC_KEY_LEN];
        let err = verify_and_decrypt(&seed, &CHANNEL, &post).unwrap_err();
        assert!(matches!(
            err,
            ChannelError::BadSenderKey | ChannelError::BadSignature
        ));
    }
}
