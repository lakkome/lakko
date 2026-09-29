// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Channel member card ([ADR-0060] section 3, "Channels get a member list").
//!
//! A subscriber (publishers too) uploads a **member card** to
//! `services/channels`: its profile envelope ([ADR-0048] v3, opaque here),
//! sealed under the channel's current epoch seed so every holder of that seed —
//! every current subscriber and publisher — can open it and the server cannot.
//! The server stores one card per subscriber key it already holds.
//!
//! ## Keyed per subscriber
//!
//! The card key is derived per `(channel, epoch, subscriber)`, and the AEAD AAD
//! binds the same three plus the version, so the channels server cannot swap two
//! subscribers' cards (or move a card between channels or epochs): a card served
//! under the wrong subscriber key fails the authenticator and is refused, never
//! shown under someone else's name.
//!
//! ```text
//! card_key = HKDF-SHA-256(ikm  = epoch seed,
//!                         salt = channel_id(16) ‖ epoch_id(4 BE) ‖ subscriber_pk(32),
//!                         info = "lakko/channel/member-card/v1")
//! wire     = version(1)=0x02 ‖ epoch_id(4 BE) ‖ nonce(12) ‖ ciphertext(plaintext + 16-byte tag)
//! aad      = version ‖ epoch_id(4 BE) ‖ channel_id(16) ‖ subscriber_pk(32)
//! plaintext = identity_pk(32) ‖ binding_sig(64) ‖ profile
//! ```
//!
//! ## Bound to the account behind it
//!
//! (ADR-0060 section 3, "A card is bound to the account behind it".) The
//! subscriber key is a per-channel key that says nothing about the account,
//! so without a binding any subscriber could upload a card naming another
//! account and a reader would file the card's grant and name under that
//! account. The plaintext therefore opens with the account identity key and
//! its Ed25519 signature over
//!
//! ```text
//! "lakko/channel/member-card-binding/v1" ‖ channel_id(16) ‖ subscriber_pk(32) ‖ BLAKE3(profile)
//! ```
//!
//! [`open_member_card`] verifies it and returns the identity key beside the
//! profile; a card whose binding does not verify is refused
//! ([`ChannelError::MemberCardBinding`]). The signature names the channel and
//! the subscriber key, so a card cannot be replayed under another subscriber
//! key or in another channel even by someone holding its plaintext. It does
//! NOT name the epoch: a subscriber re-seals the same signed profile after a
//! rotation without re-signing. Version 1 (no binding) is refused: a flag day.
//!
//! The caller still checks that the profile it decodes belongs to the
//! returned identity key (the ADR-0048 profile carries no key of its own, so
//! the identity key returned here is the one the reader files the profile
//! under).
//!
//! **HKDF, not BLAKE3.** The key follows this crate's own convention (the
//! [`post`](crate::post) per-message key is HKDF-SHA-256 with the seed as IKM
//! and the binding fields as salt), so a reviewer reads one derivation style
//! across the crate. The info label is distinct from the post labels
//! (`lakko/channel/v1/msg-key`, `lakko/channel/v1/msg-nonce`), so a card key
//! never equals a post key.
//!
//! **Random nonce.** A card is re-uploaded whenever the profile changes, so the
//! same `(channel, epoch, subscriber)` key can seal several plaintexts; the
//! nonce is 96 fresh random bits per seal and rides in the clear. The number of
//! seals under one key is a handful of profile edits within one epoch, far below
//! the random-nonce birthday bound.
//!
//! **Epoch in the clear.** A reader holds several epoch seeds (ADR-0031
//! retention), so the epoch rides in the clear for [`member_card_epoch`] to pick
//! the seed. It is also in the salt and the AAD, so a relabelled epoch fails.
//!
//! **Size.** The profile is at most [`MAX_MEMBER_CARD_PROFILE_LEN`] = 1024
//! bytes. The value is chosen, not measured: it is above the ADR-0048 profile
//! envelope's current maximum with room for the v3 grant (a 153-byte username
//! certificate plus reach seeds), and small enough to bound a paged member-list
//! response.
//!
//! ## The upload signature
//!
//! `services/channels` stores a card only when the request is signed by the
//! subscriber key the path names (the Ed25519 key of [`crate::subscribe`]).
//! The signed bytes are defined here, once, for the clients and the server:
//!
//! ```text
//! "lakko/channel/member-card-sig/v1" ‖ channel_id(16) ‖ subscriber_pk(32) ‖ SHA-256(card)
//! ```
//!
//! The domain is distinct from the card key's HKDF info label
//! ([`MEMBER_CARD_KEY_INFO_V1`]) and from every other signature domain in the
//! crate (`lakko/channel/v1/post-sig`), so a card signature is never a post
//! signature. A delete signs the same bytes over the empty card; no card is
//! empty (a card is at least [`MEMBER_CARD_MIN_LEN`] bytes), so a delete
//! signature never authorises a write and the other way round.
//!
//! [ADR-0060]: ../../../docs/adr/0060-scoped-discovery.md
//! [ADR-0048]: ../../../docs/adr/0048-profile-names.md

use lakko_crypto::aead::{self, Algorithm, KEY_LEN, NONCE_LEN};
use lakko_crypto::hash;
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::random;
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};

use crate::epoch::{EpochId, EPOCH_ID_LEN};
use crate::error::{ChannelError, Result};
use crate::seed::ContentKeySeed;
use crate::CHANNEL_ID_LEN;

/// Wire-format version of a member card (v2: the account binding). The opener
/// refuses any other value.
pub const MEMBER_CARD_VERSION_V2: u8 = 2;

/// HKDF info label for the per-subscriber card key, frozen at v1.
pub const MEMBER_CARD_KEY_INFO_V1: &[u8] = b"lakko/channel/member-card/v1";

/// Domain that opens the bytes the account identity key signs to bind a card
/// to its account, frozen at v1. Distinct from every other domain here.
pub const MEMBER_CARD_BINDING_DOMAIN_V1: &[u8] = b"lakko/channel/member-card-binding/v1";

/// Length of the account identity public key a card names.
pub const MEMBER_CARD_IDENTITY_PK_LEN: usize = PUBLIC_KEY_LEN;

/// Length of the binding prefix of a card plaintext: identity key and
/// signature.
pub const MEMBER_CARD_BINDING_LEN: usize = MEMBER_CARD_IDENTITY_PK_LEN + SIGNATURE_LEN;

/// Domain that opens the bytes a member-card upload or delete signature covers,
/// frozen at v1. Distinct from [`MEMBER_CARD_KEY_INFO_V1`].
pub const MEMBER_CARD_SIG_DOMAIN_V1: &[u8] = b"lakko/channel/member-card-sig/v1";

/// The request header carrying a member-card upload or delete signature: the
/// 64-byte signature in base64url without padding. Shared by the clients and
/// `services/channels`.
pub const MEMBER_CARD_SIG_HEADER: &str = "x-lakko-member-card-sig";

/// Length of a member-card request signature (Ed25519).
pub const MEMBER_CARD_SIG_LEN: usize = SIGNATURE_LEN;

/// Maximum length of the sealed profile, in bytes. Chosen, not measured (see
/// the module docs).
pub const MAX_MEMBER_CARD_PROFILE_LEN: usize = 1024;

/// Length of the subscriber key a card is bound to (the 32-byte key the
/// subscriber registered with `services/channels`).
pub const SUBSCRIBER_PK_LEN: usize = PUBLIC_KEY_LEN;

/// Header length: `version + epoch_id + nonce`.
const HEADER_LEN: usize = 1 + EPOCH_ID_LEN + NONCE_LEN;

/// Smallest valid card: header, the binding, an empty profile and the tag.
pub const MEMBER_CARD_MIN_LEN: usize = HEADER_LEN + MEMBER_CARD_BINDING_LEN + aead::TAG_LEN;

/// A member card opened and its binding verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedMemberCard {
    /// The account identity public key that signed the binding: the account
    /// the reader files the card's profile and grant under.
    pub identity_pk: [u8; MEMBER_CARD_IDENTITY_PK_LEN],
    /// The profile envelope, opaque here.
    pub profile: Vec<u8>,
}

/// Largest valid card: header plus a maximal profile plus its tag.
pub const MEMBER_CARD_MAX_LEN: usize = MEMBER_CARD_MIN_LEN + MAX_MEMBER_CARD_PROFILE_LEN;

/// Seal `profile` into a member card for `subscriber_pk` in `channel_id` under
/// the seed of `epoch`, bound to the account whose 32-byte Ed25519 identity
/// seed is `account_identity_seed` (module docs).
///
/// # Errors
///
/// - [`ChannelError::MemberCardTooLong`] if `profile` exceeds
///   [`MAX_MEMBER_CARD_PROFILE_LEN`].
/// - [`ChannelError::KeyDerivation`] on HKDF failure (impossible with these
///   lengths, surfaced for completeness).
/// - [`ChannelError::AeadSeal`] on AEAD failure (likewise).
pub fn seal_member_card(
    epoch_seed: &ContentKeySeed,
    epoch: EpochId,
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    account_identity_seed: &[u8; 32],
    profile: &[u8],
) -> Result<Vec<u8>> {
    if profile.len() > MAX_MEMBER_CARD_PROFILE_LEN {
        return Err(ChannelError::MemberCardTooLong {
            max: MAX_MEMBER_CARD_PROFILE_LEN,
            got: profile.len(),
        });
    }
    let identity = SigningKey::from_seed(account_identity_seed);
    let signature = identity.sign(&member_card_binding_bytes(
        channel_id,
        subscriber_pk,
        profile,
    ));
    let mut plaintext = Vec::with_capacity(MEMBER_CARD_BINDING_LEN + profile.len());
    plaintext.extend_from_slice(&identity.verifying_key().to_bytes());
    plaintext.extend_from_slice(&signature.to_bytes());
    plaintext.extend_from_slice(profile);

    let key = card_key(epoch_seed, epoch, channel_id, subscriber_pk)?;
    let mut nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let aad = card_aad(epoch, channel_id, subscriber_pk);
    let ciphertext = aead::seal(Algorithm::ChaCha20Poly1305, &key, &nonce, &aad, &plaintext)
        .map_err(|_| ChannelError::AeadSeal)?;

    let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    out.push(MEMBER_CARD_VERSION_V2);
    out.extend_from_slice(&epoch.to_be_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Read the clear epoch of a member card, so a reader can pick the matching
/// seed before [`open_member_card`]. The epoch is not authenticated until the
/// card opens.
///
/// # Errors
///
/// [`ChannelError::Wire`] if the card is shorter than [`MEMBER_CARD_MIN_LEN`],
/// longer than [`MEMBER_CARD_MAX_LEN`], or carries an unknown version.
pub fn member_card_epoch(bytes: &[u8]) -> Result<EpochId> {
    check_frame(bytes)?;
    EpochId::from_be_bytes(&bytes[1..=EPOCH_ID_LEN])
}

/// Open a member card sealed for `subscriber_pk` in `channel_id`, under the
/// seed of the epoch the card names, and verify its account binding.
/// Fail-closed: a card sealed for another subscriber, channel or epoch,
/// tampered in any byte, or whose binding does not verify, does not open.
///
/// # Errors
///
/// - [`ChannelError::Wire`] on a malformed frame (see [`member_card_epoch`]).
/// - [`ChannelError::KeyDerivation`] on HKDF failure (impossible).
/// - [`ChannelError::AeadOpen`] if the authenticator refuses the card (wrong
///   seed, wrong subscriber / channel / epoch binding, or tampered bytes).
/// - [`ChannelError::MemberCardBinding`] if the identity key is not a valid
///   Ed25519 key or its signature does not cover this channel, subscriber
///   key and profile.
pub fn open_member_card(
    epoch_seed: &ContentKeySeed,
    bytes: &[u8],
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
) -> Result<OpenedMemberCard> {
    let epoch = member_card_epoch(bytes)?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&bytes[1 + EPOCH_ID_LEN..HEADER_LEN]);
    let key = card_key(epoch_seed, epoch, channel_id, subscriber_pk)?;
    let aad = card_aad(epoch, channel_id, subscriber_pk);
    let plaintext = aead::open(
        Algorithm::ChaCha20Poly1305,
        &key,
        &nonce,
        &aad,
        &bytes[HEADER_LEN..],
    )
    .map_err(|_| ChannelError::AeadOpen)?;
    verify_binding(&plaintext, channel_id, subscriber_pk)
}

/// Split a card plaintext into its binding and profile and verify the
/// binding. Also the fuzz entry point: the AEAD keeps a fuzzer from reaching
/// this parser through [`open_member_card`].
pub(crate) fn verify_binding(
    plaintext: &[u8],
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
) -> Result<OpenedMemberCard> {
    // The frame bound guarantees an opened plaintext holds the binding
    // prefix; this check keeps the parser total on its own.
    if plaintext.len() < MEMBER_CARD_BINDING_LEN {
        return Err(ChannelError::MemberCardBinding);
    }
    let (identity_bytes, rest) = plaintext.split_at(MEMBER_CARD_IDENTITY_PK_LEN);
    let (sig_bytes, profile) = rest.split_at(SIGNATURE_LEN);
    if profile.len() > MAX_MEMBER_CARD_PROFILE_LEN {
        return Err(ChannelError::MemberCardBinding);
    }
    let identity_pk: [u8; MEMBER_CARD_IDENTITY_PK_LEN] = identity_bytes
        .try_into()
        .map_err(|_| ChannelError::MemberCardBinding)?;
    let verifying =
        VerifyingKey::from_bytes(&identity_pk).map_err(|_| ChannelError::MemberCardBinding)?;
    // A small-order key verifies a forged signature over any message.
    if verifying.is_weak() {
        return Err(ChannelError::MemberCardBinding);
    }
    let signature =
        Signature::from_bytes(sig_bytes).map_err(|_| ChannelError::MemberCardBinding)?;
    verifying
        .verify(
            &member_card_binding_bytes(channel_id, subscriber_pk, profile),
            &signature,
        )
        .map_err(|_| ChannelError::MemberCardBinding)?;
    Ok(OpenedMemberCard {
        identity_pk,
        profile: profile.to_vec(),
    })
}

/// The bytes the account identity key signs to bind a card to its account:
/// `domain ‖ channel_id ‖ subscriber_pk ‖ BLAKE3(profile)`.
#[must_use]
pub fn member_card_binding_bytes(
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    profile: &[u8],
) -> Vec<u8> {
    let digest = hash::blake3(profile);
    let mut out = Vec::with_capacity(
        MEMBER_CARD_BINDING_DOMAIN_V1.len() + CHANNEL_ID_LEN + SUBSCRIBER_PK_LEN + digest.len(),
    );
    out.extend_from_slice(MEMBER_CARD_BINDING_DOMAIN_V1);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(subscriber_pk);
    out.extend_from_slice(&digest);
    out
}

/// The bytes a subscriber signs to upload `card` (or, with an empty `card`, to
/// delete its card): `domain ‖ channel_id ‖ subscriber_pk ‖ SHA-256(card)`.
#[must_use]
pub fn member_card_signed_bytes(
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    card: &[u8],
) -> Vec<u8> {
    let digest = hash::sha256(card);
    let mut out = Vec::with_capacity(
        MEMBER_CARD_SIG_DOMAIN_V1.len() + CHANNEL_ID_LEN + SUBSCRIBER_PK_LEN + digest.len(),
    );
    out.extend_from_slice(MEMBER_CARD_SIG_DOMAIN_V1);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(subscriber_pk);
    out.extend_from_slice(&digest);
    out
}

/// Sign a member-card upload (or, with an empty `card`, a delete) with the
/// subscriber key. The subscriber key the bytes name is this key's public
/// half.
#[must_use]
pub fn sign_member_card_request(
    subscriber_key: &SigningKey,
    channel_id: &[u8; CHANNEL_ID_LEN],
    card: &[u8],
) -> [u8; MEMBER_CARD_SIG_LEN] {
    let pk = subscriber_key.verifying_key().to_bytes();
    subscriber_key
        .sign(&member_card_signed_bytes(channel_id, &pk, card))
        .to_bytes()
}

/// Verify a member-card request signature by `subscriber_pk`, which must be a
/// valid subscriber key ([`crate::subscribe::subscriber_public_key`]).
///
/// # Errors
///
/// [`ChannelError::BadSubscriberKey`] for an invalid or small-order key,
/// [`ChannelError::BadSignature`] for a signature of the wrong length or one
/// that does not verify.
pub fn verify_member_card_request(
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    card: &[u8],
    signature: &[u8],
) -> Result<()> {
    let key = crate::subscribe::subscriber_public_key(subscriber_pk)?;
    let sig = Signature::from_bytes(signature).map_err(|_| ChannelError::BadSignature)?;
    key.verify(
        &member_card_signed_bytes(channel_id, subscriber_pk, card),
        &sig,
    )
    .map_err(|_| ChannelError::BadSignature)
}

// ----------------------------------------------------------------
// Internal helpers — one source of truth for the frame, key and AAD.
// ----------------------------------------------------------------

fn check_frame(bytes: &[u8]) -> Result<()> {
    if bytes.len() < MEMBER_CARD_MIN_LEN {
        return Err(ChannelError::Wire("member card shorter than minimum"));
    }
    if bytes.len() > MEMBER_CARD_MAX_LEN {
        return Err(ChannelError::Wire("member card longer than maximum"));
    }
    if bytes[0] != MEMBER_CARD_VERSION_V2 {
        return Err(ChannelError::Wire("unknown member card version"));
    }
    Ok(())
}

fn card_key(
    epoch_seed: &ContentKeySeed,
    epoch: EpochId,
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
) -> Result<SecretBytes<KEY_LEN>> {
    let mut salt = Vec::with_capacity(CHANNEL_ID_LEN + EPOCH_ID_LEN + SUBSCRIBER_PK_LEN);
    salt.extend_from_slice(channel_id);
    salt.extend_from_slice(&epoch.to_be_bytes());
    salt.extend_from_slice(subscriber_pk);
    hkdf_sha256::<KEY_LEN>(epoch_seed.as_inner_bytes(), &salt, MEMBER_CARD_KEY_INFO_V1)
        .map_err(|_| ChannelError::KeyDerivation)
}

fn card_aad(
    epoch: EpochId,
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(1 + EPOCH_ID_LEN + CHANNEL_ID_LEN + SUBSCRIBER_PK_LEN);
    aad.push(MEMBER_CARD_VERSION_V2);
    aad.extend_from_slice(&epoch.to_be_bytes());
    aad.extend_from_slice(channel_id);
    aad.extend_from_slice(subscriber_pk);
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];
    const ALICE: [u8; SUBSCRIBER_PK_LEN] = [0xA1; SUBSCRIBER_PK_LEN];
    const BOB: [u8; SUBSCRIBER_PK_LEN] = [0xB0; SUBSCRIBER_PK_LEN];

    fn seed() -> ContentKeySeed {
        ContentKeySeed::from_bytes_for_test([0x5E; 32])
    }

    /// Alice's account identity seed; her subscriber key in `CHANNEL` is
    /// `ALICE`.
    const ALICE_ACCOUNT: [u8; 32] = [0x1A; 32];
    const BOB_ACCOUNT: [u8; 32] = [0x1B; 32];

    fn account_pk(seed: &[u8; 32]) -> [u8; 32] {
        SigningKey::from_seed(seed).verifying_key().to_bytes()
    }

    fn card(profile: &[u8]) -> Vec<u8> {
        seal_member_card(
            &seed(),
            EpochId::new(7),
            &CHANNEL,
            &ALICE,
            &ALICE_ACCOUNT,
            profile,
        )
        .unwrap()
    }

    /// Seal an arbitrary plaintext under the real card key and AAD: what a
    /// subscriber holding the epoch seed can put on the server.
    fn seal_raw(subscriber: &[u8; SUBSCRIBER_PK_LEN], plaintext: &[u8]) -> Vec<u8> {
        let epoch = EpochId::new(7);
        let key = card_key(&seed(), epoch, &CHANNEL, subscriber).unwrap();
        let nonce = [9u8; NONCE_LEN];
        let ct = aead::seal(
            Algorithm::ChaCha20Poly1305,
            &key,
            &nonce,
            &card_aad(epoch, &CHANNEL, subscriber),
            plaintext,
        )
        .unwrap();
        let mut out = vec![MEMBER_CARD_VERSION_V2];
        out.extend_from_slice(&epoch.to_be_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    fn binding(
        signer: &[u8; 32],
        named: &[u8; 32],
        subscriber: &[u8; SUBSCRIBER_PK_LEN],
        profile: &[u8],
    ) -> Vec<u8> {
        let sig = SigningKey::from_seed(signer)
            .sign(&member_card_binding_bytes(&CHANNEL, subscriber, profile))
            .to_bytes();
        let mut out = named.to_vec();
        out.extend_from_slice(&sig);
        out.extend_from_slice(profile);
        out
    }

    #[test]
    fn the_card_names_the_account_that_signed_it() {
        let opened = open_member_card(&seed(), &card(b"p"), &CHANNEL, &ALICE).unwrap();
        assert_eq!(opened.identity_pk, account_pk(&ALICE_ACCOUNT));
        assert_eq!(opened.profile, b"p");
    }

    #[test]
    fn a_card_naming_another_account_is_refused() {
        // Subscriber A (holding the epoch seed) seals a card that names B's
        // account key, signed with its own key: refused.
        let forged = seal_raw(
            &ALICE,
            &binding(&ALICE_ACCOUNT, &account_pk(&BOB_ACCOUNT), &ALICE, b"bob?"),
        );
        assert!(matches!(
            open_member_card(&seed(), &forged, &CHANNEL, &ALICE),
            Err(ChannelError::MemberCardBinding)
        ));
        // The honest binding verifies through the same path.
        let honest = seal_raw(
            &ALICE,
            &binding(&ALICE_ACCOUNT, &account_pk(&ALICE_ACCOUNT), &ALICE, b"me"),
        );
        assert_eq!(
            open_member_card(&seed(), &honest, &CHANNEL, &ALICE)
                .unwrap()
                .identity_pk,
            account_pk(&ALICE_ACCOUNT)
        );
    }

    #[test]
    fn a_genuine_binding_moved_to_another_subscriber_or_profile_is_refused() {
        // B lifts A's genuine plaintext (it can open A's card) and re-seals
        // it under its own subscriber key: the binding names A's subscriber
        // key, so it fails.
        let lifted = binding(&ALICE_ACCOUNT, &account_pk(&ALICE_ACCOUNT), &ALICE, b"a");
        assert!(matches!(
            open_member_card(&seed(), &seal_raw(&BOB, &lifted), &CHANNEL, &BOB),
            Err(ChannelError::MemberCardBinding)
        ));
        // Same binding, profile altered inside a valid seal.
        let mut altered = lifted;
        let last = altered.len() - 1;
        altered[last] ^= 1;
        assert!(matches!(
            open_member_card(&seed(), &seal_raw(&ALICE, &altered), &CHANNEL, &ALICE),
            Err(ChannelError::MemberCardBinding)
        ));
        // A small-order identity key (it verifies forged signatures).
        let mut weak = vec![0u8; MEMBER_CARD_BINDING_LEN];
        weak[0] = 1;
        assert!(matches!(
            open_member_card(&seed(), &seal_raw(&ALICE, &weak), &CHANNEL, &ALICE),
            Err(ChannelError::MemberCardBinding)
        ));
    }

    #[test]
    fn a_plaintext_shorter_than_the_binding_never_opens() {
        // The frame minimum already forces the binding prefix: a sealed
        // plaintext shorter than it is below MEMBER_CARD_MIN_LEN.
        let short = seal_raw(&ALICE, &[0u8; MEMBER_CARD_BINDING_LEN - 1]);
        assert!(matches!(
            open_member_card(&seed(), &short, &CHANNEL, &ALICE),
            Err(ChannelError::Wire(_))
        ));
    }

    const GOLDEN_BINDING_BYTES_SHA256: &str =
        "76bc8b32b2cbb49448a2b54ca7b8cb5d8b6a2d6b508e6f37f96604a89724d592";

    #[test]
    fn the_binding_bytes_are_pinned() {
        let bytes = member_card_binding_bytes(&[0xAA; CHANNEL_ID_LEN], &[0xBB; 32], b"profile");
        assert!(bytes.starts_with(b"lakko/channel/member-card-binding/v1"));
        assert_eq!(
            bytes.len(),
            MEMBER_CARD_BINDING_DOMAIN_V1.len() + 16 + 32 + 32
        );
        for other in [
            MEMBER_CARD_SIG_DOMAIN_V1,
            MEMBER_CARD_KEY_INFO_V1,
            crate::unsubscribe::UNSUBSCRIBE_DOMAIN_V1,
        ] {
            assert!(!other.starts_with(MEMBER_CARD_BINDING_DOMAIN_V1));
            assert!(!MEMBER_CARD_BINDING_DOMAIN_V1.starts_with(other));
        }
        assert_eq!(
            hex::encode(hash::sha256(&bytes)),
            GOLDEN_BINDING_BYTES_SHA256
        );
    }

    #[test]
    fn round_trip() {
        let profile = b"profile envelope bytes";
        let wire = card(profile);
        assert_eq!(
            wire.len(),
            HEADER_LEN + MEMBER_CARD_BINDING_LEN + profile.len() + aead::TAG_LEN
        );
        assert_eq!(wire[0], MEMBER_CARD_VERSION_V2);
        assert_eq!(member_card_epoch(&wire).unwrap(), EpochId::new(7));
        assert_eq!(
            open_member_card(&seed(), &wire, &CHANNEL, &ALICE)
                .unwrap()
                .profile,
            profile
        );
    }

    #[test]
    fn empty_and_maximal_profiles_round_trip() {
        for len in [0, MAX_MEMBER_CARD_PROFILE_LEN] {
            let profile = vec![0x42; len];
            let wire = card(&profile);
            assert!(wire.len() >= MEMBER_CARD_MIN_LEN && wire.len() <= MEMBER_CARD_MAX_LEN);
            assert_eq!(
                open_member_card(&seed(), &wire, &CHANNEL, &ALICE)
                    .unwrap()
                    .profile,
                profile
            );
        }
    }

    #[test]
    fn two_seals_of_one_profile_differ() {
        // Fresh nonce per seal: re-uploading the same profile is not linkable
        // byte-for-byte, and a key is never reused with one nonce.
        assert_ne!(card(b"same"), card(b"same"));
    }

    #[test]
    fn a_card_does_not_open_for_another_subscriber() {
        let wire = card(b"alice");
        assert!(matches!(
            open_member_card(&seed(), &wire, &CHANNEL, &BOB),
            Err(ChannelError::AeadOpen)
        ));
    }

    #[test]
    fn a_card_does_not_open_in_another_channel() {
        let wire = card(b"alice");
        let other = [0x0D; CHANNEL_ID_LEN];
        assert!(matches!(
            open_member_card(&seed(), &wire, &other, &ALICE),
            Err(ChannelError::AeadOpen)
        ));
    }

    #[test]
    fn a_card_does_not_open_under_another_seed() {
        let wire = card(b"alice");
        let other = ContentKeySeed::from_bytes_for_test([0x5F; 32]);
        assert!(matches!(
            open_member_card(&other, &wire, &CHANNEL, &ALICE),
            Err(ChannelError::AeadOpen)
        ));
    }

    #[test]
    fn a_relabelled_epoch_does_not_open() {
        let mut wire = card(b"alice");
        wire[1..=EPOCH_ID_LEN].copy_from_slice(&EpochId::new(8).to_be_bytes());
        assert_eq!(member_card_epoch(&wire).unwrap(), EpochId::new(8));
        assert!(matches!(
            open_member_card(&seed(), &wire, &CHANNEL, &ALICE),
            Err(ChannelError::AeadOpen)
        ));
    }

    #[test]
    fn tampering_any_byte_is_refused() {
        let wire = card(b"alice's profile");
        for idx in 0..wire.len() {
            let mut bad = wire.clone();
            bad[idx] ^= 0x01;
            assert!(
                open_member_card(&seed(), &bad, &CHANNEL, &ALICE).is_err(),
                "tampering byte {idx} must be refused"
            );
        }
    }

    #[test]
    fn truncated_and_oversize_cards_are_refused() {
        let wire = card(b"alice");
        for len in 0..MEMBER_CARD_MIN_LEN {
            assert!(matches!(
                member_card_epoch(&wire[..len.min(wire.len())]),
                Err(ChannelError::Wire(_))
            ));
        }
        // A truncated tail that still meets the minimum fails the authenticator.
        assert!(matches!(
            open_member_card(&seed(), &wire[..wire.len() - 1], &CHANNEL, &ALICE),
            Err(ChannelError::AeadOpen)
        ));
        let mut long = card(&[0u8; MAX_MEMBER_CARD_PROFILE_LEN]);
        long.push(0);
        assert!(matches!(
            open_member_card(&seed(), &long, &CHANNEL, &ALICE),
            Err(ChannelError::Wire(_))
        ));
    }

    #[test]
    fn oversize_profile_is_refused_at_seal() {
        let profile = vec![0u8; MAX_MEMBER_CARD_PROFILE_LEN + 1];
        assert!(matches!(
            seal_member_card(
                &seed(),
                EpochId::new(7),
                &CHANNEL,
                &ALICE,
                &ALICE_ACCOUNT,
                &profile
            ),
            Err(ChannelError::MemberCardTooLong { max, got })
                if max == MAX_MEMBER_CARD_PROFILE_LEN && got == MAX_MEMBER_CARD_PROFILE_LEN + 1
        ));
    }

    #[test]
    fn a_request_signature_binds_channel_key_and_card() {
        let sub = SigningKey::from_seed(&[0x51; 32]);
        let pk = sub.verifying_key().to_bytes();
        let wire = card(b"alice");
        let sig = sign_member_card_request(&sub, &CHANNEL, &wire);
        verify_member_card_request(&CHANNEL, &pk, &wire, &sig).unwrap();
        // Another card, another channel, another key, a delete: all refused.
        assert!(matches!(
            verify_member_card_request(&CHANNEL, &pk, &card(b"alice"), &sig),
            Err(ChannelError::BadSignature)
        ));
        assert!(verify_member_card_request(&[0x0D; CHANNEL_ID_LEN], &pk, &wire, &sig).is_err());
        let other = SigningKey::from_seed(&[0x52; 32])
            .verifying_key()
            .to_bytes();
        assert!(verify_member_card_request(&CHANNEL, &other, &wire, &sig).is_err());
        assert!(verify_member_card_request(&CHANNEL, &pk, &[], &sig).is_err());
        // A delete signature verifies over the empty card only.
        let del = sign_member_card_request(&sub, &CHANNEL, &[]);
        verify_member_card_request(&CHANNEL, &pk, &[], &del).unwrap();
        assert!(verify_member_card_request(&CHANNEL, &pk, &wire, &del).is_err());
        assert!(matches!(
            verify_member_card_request(&CHANNEL, &pk, &wire, &sig[..63]),
            Err(ChannelError::BadSignature)
        ));
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(matches!(
            verify_member_card_request(&CHANNEL, &identity, &wire, &sig),
            Err(ChannelError::BadSubscriberKey)
        ));
    }

    const GOLDEN_SIGNED_BYTES_SHA256: &str =
        "1465d24894cdfaeec3f962da20cc17fa0519038a7320e40249f29cd9d7fe49aa";

    #[test]
    fn the_signed_bytes_are_pinned() {
        let bytes = member_card_signed_bytes(&[0xAA; CHANNEL_ID_LEN], &[0xBB; 32], b"card");
        assert!(bytes.starts_with(b"lakko/channel/member-card-sig/v1"));
        assert_eq!(bytes.len(), MEMBER_CARD_SIG_DOMAIN_V1.len() + 16 + 32 + 32);
        assert_ne!(MEMBER_CARD_SIG_DOMAIN_V1, MEMBER_CARD_KEY_INFO_V1);
        assert_eq!(
            hex::encode(hash::sha256(&bytes)),
            GOLDEN_SIGNED_BYTES_SHA256
        );
    }

    #[test]
    fn unknown_version_is_refused() {
        let mut wire = card(b"alice");
        wire[0] = 1;
        assert!(matches!(
            member_card_epoch(&wire),
            Err(ChannelError::Wire(_))
        ));
        assert!(matches!(
            open_member_card(&seed(), &wire, &CHANNEL, &ALICE),
            Err(ChannelError::Wire(_))
        ));
    }
}
