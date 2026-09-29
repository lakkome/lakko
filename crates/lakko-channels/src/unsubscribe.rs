// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Leaving a channel on the server ([ADR-0060] section 3, "A subscriber can
//! unsubscribe").
//!
//! `DELETE /v1/channels/{channel_id}/subscribers/{pubkey_hex}` removes the
//! subscriber row, its seed envelopes and its member card. It is signed by the
//! subscriber key the path names (the Ed25519 key of [`crate::subscribe`]); the
//! signed bytes are defined here, once, for the clients and the server:
//!
//! ```text
//! "lakko/channel/unsubscribe/v1" ‖ channel_id(16) ‖ subscriber_pk(32) ‖ issued_at_ms(8 BE)
//! ```
//!
//! ## Freshness
//!
//! A captured signature must not delete the subscriber forever: without a
//! timestamp, anyone who once saw the request (a proxy, a log) could replay it
//! after the person subscribes again, and the subscription would silently
//! vanish. The signed bytes carry the signer's clock; the server accepts a
//! request only within [`UNSUBSCRIBE_WINDOW_MS`] of its own clock, either side
//! (the DS proof window of ADR-0053, chosen and not measured). **Residual,
//! recorded:** within the window a replay deletes the row again. That undoes
//! nothing the subscriber did not ask for, unless they re-subscribe inside the
//! same two minutes; a server-side nonce set would close that, and is not
//! built.
//!
//! ## Header
//!
//! [`UNSUBSCRIBE_SIG_HEADER`] carries `<issued_at_ms decimal>.<signature
//! base64url without padding>`; the FFI signer composes it and the server
//! parses it. The domain is distinct from every other domain in the crate,
//! so an unsubscribe signature is never a member-card or post signature.
//!
//! [ADR-0060]: ../../../docs/adr/0060-scoped-discovery.md

use lakko_crypto::signing::{Signature, SigningKey, SIGNATURE_LEN};

use crate::error::{ChannelError, Result};
use crate::member_card::SUBSCRIBER_PK_LEN;
use crate::CHANNEL_ID_LEN;

/// Domain that opens the bytes an unsubscribe signature covers, frozen at v1.
pub const UNSUBSCRIBE_DOMAIN_V1: &[u8] = b"lakko/channel/unsubscribe/v1";

/// The request header carrying an unsubscribe signature: `<issued_at_ms>.<sig
/// base64url>` (module docs).
pub const UNSUBSCRIBE_SIG_HEADER: &str = "x-lakko-unsubscribe-sig";

/// How far the signer's clock may be from the server's, either side, in
/// milliseconds. Chosen, not measured (the ADR-0053 DS proof window).
pub const UNSUBSCRIBE_WINDOW_MS: u64 = 120_000;

/// Length of an unsubscribe signature (Ed25519).
pub const UNSUBSCRIBE_SIG_LEN: usize = SIGNATURE_LEN;

/// The bytes a subscriber signs to leave `channel_id`:
/// `domain ‖ channel_id ‖ subscriber_pk ‖ issued_at_ms (8 BE)`.
#[must_use]
pub fn unsubscribe_signed_bytes(
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    issued_at_ms: u64,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(UNSUBSCRIBE_DOMAIN_V1.len() + CHANNEL_ID_LEN + SUBSCRIBER_PK_LEN + 8);
    out.extend_from_slice(UNSUBSCRIBE_DOMAIN_V1);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(subscriber_pk);
    out.extend_from_slice(&issued_at_ms.to_be_bytes());
    out
}

/// Sign an unsubscribe from `channel_id` with the subscriber key, stamped
/// `issued_at_ms` (the signer's clock).
#[must_use]
pub fn sign_unsubscribe_request(
    subscriber_key: &SigningKey,
    channel_id: &[u8; CHANNEL_ID_LEN],
    issued_at_ms: u64,
) -> [u8; UNSUBSCRIBE_SIG_LEN] {
    let pk = subscriber_key.verifying_key().to_bytes();
    subscriber_key
        .sign(&unsubscribe_signed_bytes(channel_id, &pk, issued_at_ms))
        .to_bytes()
}

/// Verify an unsubscribe by `subscriber_pk`: the stamp within
/// [`UNSUBSCRIBE_WINDOW_MS`] of `now_ms`, then the signature.
///
/// # Errors
///
/// - [`ChannelError::StaleRequest`] for a stamp outside the window (checked
///   first, so a stale request costs no signature check).
/// - [`ChannelError::BadSubscriberKey`] for an invalid or small-order key.
/// - [`ChannelError::BadSignature`] for a signature of the wrong length or
///   one that does not verify.
pub fn verify_unsubscribe_request(
    channel_id: &[u8; CHANNEL_ID_LEN],
    subscriber_pk: &[u8; SUBSCRIBER_PK_LEN],
    issued_at_ms: u64,
    signature: &[u8],
    now_ms: u64,
) -> Result<()> {
    if now_ms.abs_diff(issued_at_ms) > UNSUBSCRIBE_WINDOW_MS {
        return Err(ChannelError::StaleRequest);
    }
    let key = crate::subscribe::subscriber_public_key(subscriber_pk)?;
    let sig = Signature::from_bytes(signature).map_err(|_| ChannelError::BadSignature)?;
    key.verify(
        &unsubscribe_signed_bytes(channel_id, subscriber_pk, issued_at_ms),
        &sig,
    )
    .map_err(|_| ChannelError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::hash;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];
    const NOW: u64 = 1_800_000_000_000;

    #[test]
    fn a_fresh_signature_verifies_and_binds_channel_key_and_stamp() {
        let sub = SigningKey::from_seed(&[0x51; 32]);
        let pk = sub.verifying_key().to_bytes();
        let sig = sign_unsubscribe_request(&sub, &CHANNEL, NOW);
        verify_unsubscribe_request(&CHANNEL, &pk, NOW, &sig, NOW).unwrap();
        // Either side of the window's edge.
        verify_unsubscribe_request(&CHANNEL, &pk, NOW, &sig, NOW + UNSUBSCRIBE_WINDOW_MS).unwrap();
        verify_unsubscribe_request(&CHANNEL, &pk, NOW, &sig, NOW - UNSUBSCRIBE_WINDOW_MS).unwrap();
        // Another channel, another key, another stamp: refused.
        assert!(matches!(
            verify_unsubscribe_request(&[0x0D; CHANNEL_ID_LEN], &pk, NOW, &sig, NOW),
            Err(ChannelError::BadSignature)
        ));
        let other = SigningKey::from_seed(&[0x52; 32])
            .verifying_key()
            .to_bytes();
        assert!(matches!(
            verify_unsubscribe_request(&CHANNEL, &other, NOW, &sig, NOW),
            Err(ChannelError::BadSignature)
        ));
        assert!(matches!(
            verify_unsubscribe_request(&CHANNEL, &pk, NOW + 1, &sig, NOW),
            Err(ChannelError::BadSignature)
        ));
        assert!(matches!(
            verify_unsubscribe_request(&CHANNEL, &pk, NOW, &sig[..63], NOW),
            Err(ChannelError::BadSignature)
        ));
        let mut small = [0u8; 32];
        small[0] = 1;
        assert!(matches!(
            verify_unsubscribe_request(&CHANNEL, &small, NOW, &sig, NOW),
            Err(ChannelError::BadSubscriberKey)
        ));
    }

    #[test]
    fn a_replay_outside_the_window_is_refused() {
        let sub = SigningKey::from_seed(&[0x51; 32]);
        let pk = sub.verifying_key().to_bytes();
        let sig = sign_unsubscribe_request(&sub, &CHANNEL, NOW);
        for now in [
            NOW + UNSUBSCRIBE_WINDOW_MS + 1,
            NOW - UNSUBSCRIBE_WINDOW_MS - 1,
            u64::MAX,
            0,
        ] {
            assert!(matches!(
                verify_unsubscribe_request(&CHANNEL, &pk, NOW, &sig, now),
                Err(ChannelError::StaleRequest)
            ));
        }
    }

    #[test]
    fn the_signed_bytes_are_pinned_and_domain_separated() {
        let bytes = unsubscribe_signed_bytes(&[0xAA; CHANNEL_ID_LEN], &[0xBB; 32], 42);
        assert!(bytes.starts_with(UNSUBSCRIBE_DOMAIN_V1));
        assert_eq!(bytes.len(), UNSUBSCRIBE_DOMAIN_V1.len() + 16 + 32 + 8);
        assert_eq!(&bytes[bytes.len() - 8..], &42u64.to_be_bytes());
        for other in [
            crate::member_card::MEMBER_CARD_SIG_DOMAIN_V1,
            crate::member_card::MEMBER_CARD_BINDING_DOMAIN_V1,
            crate::member_card::MEMBER_CARD_KEY_INFO_V1,
            crate::post::POST_SIG_DOMAIN_V1,
        ] {
            assert!(!other.starts_with(UNSUBSCRIBE_DOMAIN_V1));
            assert!(!UNSUBSCRIBE_DOMAIN_V1.starts_with(other));
        }
        assert_eq!(hex::encode(hash::sha256(&bytes)), GOLDEN_SHA256);
    }

    const GOLDEN_SHA256: &str = "f370c47629609dbba146637cb701adcd853a82380db4d3562da9c4bbdb7e9b10";
}
