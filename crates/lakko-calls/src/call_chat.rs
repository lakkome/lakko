// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A text message in a group call's own chat (2026-11-07,
//! `docs/ux/calls.md` section 4.1.4).
//!
//! # Why
//!
//! People in a group call write to each other while it lasts. The messages
//! reach only the people in the call when they are sent, are kept nowhere
//! and vanish when the call ends, so they ride the sealed SFU relay of
//! [`crate::relay`] (ADR-0078 section 2) rather than a conversation journal.
//! Each message is signed with the sender's MLS leaf key, exactly like the
//! name for the call ([`crate::call_name`]), so a receiver attributes it to
//! the leaf that signed it and never to whatever the SFU stamped as the
//! relay sender.
//!
//! # Wire
//!
//! ```text
//! "lakko/call/chat/v1\0" (19) || version(1) = 0x01 ||
//! call_id(16) || peer_id_len(1) || peer_id(1..=64, visible ASCII) ||
//! seq(8, BE u64, >= 1) || text_len(2, BE) || text(1..=800 bytes UTF-8) ||
//! signer_key(32) || signature(64)
//! ```
//!
//! The signature is Ed25519 by `signer_key` (the sender's MLS leaf key) over
//! `"lakko/call/chat/sig/v1\0" || call_id || peer_id_len || peer_id || seq
//! || text_len || text`.
//!
//! # The text
//!
//! [`check_call_chat_text`]: valid UTF-8, 1 to [`MAX_CALL_CHAT_TEXT_BYTES`]
//! bytes, not white space only, no control character except a line feed and
//! a tab. Nothing is trimmed: the sender composes what the person typed.
//!
//! # Order and duplicates
//!
//! A sender numbers its messages `1, 2, 3, ...` for the life of its peer id.
//! A receiver shows a message once per `(peer id, seq)`: a frame re-sent
//! after a reconnect is not a new message. There is no other order than
//! arrival.

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use lakko_mls::{Group, Identity};

use crate::relay::{RelayError, CALL_ID_LEN, MAX_PEER_ID_LEN};

/// Domain prefix of a call-chat message. Frozen.
pub const CALL_CHAT_DOMAIN: &[u8] = b"lakko/call/chat/v1\x00";

/// Version byte of a call-chat message.
pub const CALL_CHAT_VERSION: u8 = 0x01;

/// Domain of the signed bytes; distinct from every other relay payload's.
const CALL_CHAT_SIG_DOMAIN: &[u8] = b"lakko/call/chat/sig/v1\x00";

/// Longest text, in bytes. Chosen so the longest message fits
/// [`crate::relay::MAX_RELAY_PLAINTEXT`] with a 64-byte peer id.
pub const MAX_CALL_CHAT_TEXT_BYTES: usize = 800;

/// Why a text is not admissible in a call-chat message. Stable codes for
/// the clients (`code()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallChatTextError {
    /// No bytes at all.
    Empty,
    /// Only white space.
    Blank,
    /// More than [`MAX_CALL_CHAT_TEXT_BYTES`] bytes.
    TooLong,
    /// A control character other than a line feed or a tab.
    ControlCharacter,
}

impl CallChatTextError {
    /// Stable camelCase code, the form the relay error codes use.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Blank => "blank",
            Self::TooLong => "tooLong",
            Self::ControlCharacter => "controlCharacter",
        }
    }
}

/// The one rule for a call-chat text, used by the composer (to enable the
/// send button) and by every receiver.
///
/// # Errors
///
/// The [`CallChatTextError`] that applies, checked in the order empty,
/// too long, control character, blank.
pub fn check_call_chat_text(text: &str) -> Result<(), CallChatTextError> {
    if text.is_empty() {
        return Err(CallChatTextError::Empty);
    }
    if text.len() > MAX_CALL_CHAT_TEXT_BYTES {
        return Err(CallChatTextError::TooLong);
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(CallChatTextError::ControlCharacter);
    }
    if text.chars().all(char::is_whitespace) {
        return Err(CallChatTextError::Blank);
    }
    Ok(())
}

/// The longest prefix of `text` that fits [`MAX_CALL_CHAT_TEXT_BYTES`],
/// cut on a character boundary (for the composer, which takes no more text
/// once a message reaches the limit).
#[must_use]
pub fn clamp_call_chat_text(text: &str) -> &str {
    if text.len() <= MAX_CALL_CHAT_TEXT_BYTES {
        return text;
    }
    let mut end = MAX_CALL_CHAT_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Length of a call-chat message carrying a `peer_id_len`-byte peer id and
/// a `text_len`-byte text.
#[must_use]
pub const fn call_chat_len(peer_id_len: usize, text_len: usize) -> usize {
    CALL_CHAT_DOMAIN.len()
        + 1
        + CALL_ID_LEN
        + 1
        + peer_id_len
        + 8
        + 2
        + text_len
        + PUBLIC_KEY_LEN
        + SIGNATURE_LEN
}

/// A decoded (not yet verified) call-chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallChatMessage {
    /// The call it belongs to.
    pub call_id: [u8; CALL_ID_LEN],
    /// The sender's SFU peer id.
    pub peer_id: String,
    /// The sender's sequence number for this message (>= 1).
    pub seq: u64,
    /// The text.
    pub text: String,
    /// The leaf key that signed it.
    pub signer_key: [u8; PUBLIC_KEY_LEN],
    /// The signature.
    pub signature: [u8; SIGNATURE_LEN],
}

/// A call-chat message that verified against the call's roster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCallChat {
    /// The sender's peer id.
    pub peer_id: String,
    /// The sequence number; with `peer_id` the message's identity.
    pub seq: u64,
    /// The text.
    pub text: String,
    /// The account key the signer's leaf resolves to (ADR-0050): the person.
    pub account_key: Vec<u8>,
}

fn check_peer_id(peer_id: &[u8]) -> Result<(), RelayError> {
    if peer_id.is_empty() || peer_id.len() > MAX_PEER_ID_LEN {
        return Err(RelayError::Malformed("peer id length"));
    }
    if !peer_id.iter().all(|b| (0x21..=0x7e).contains(b)) {
        return Err(RelayError::Malformed("peer id characters"));
    }
    Ok(())
}

fn signed_bytes(call_id: &[u8; CALL_ID_LEN], peer_id: &[u8], seq: u64, text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        CALL_CHAT_SIG_DOMAIN.len() + CALL_ID_LEN + 1 + peer_id.len() + 8 + 2 + text.len(),
    );
    out.extend_from_slice(CALL_CHAT_SIG_DOMAIN);
    out.extend_from_slice(call_id);
    // Both lengths are bounded by every caller (64 and 800).
    out.push(u8::try_from(peer_id.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(peer_id);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&u16::try_from(text.len()).unwrap_or(u16::MAX).to_be_bytes());
    out.extend_from_slice(text);
    out
}

/// Compose a call-chat message: `text` numbered `seq` for call `call_id`
/// under SFU `peer_id`, signed with `identity`'s leaf signature key.
///
/// # Errors
///
/// [`RelayError::Malformed`] for a bad peer id, a `seq` of 0, or a text
/// [`check_call_chat_text`] refuses; [`RelayError::IdentityKeyMismatch`]
/// when the identity's signing seed does not produce its published
/// signature key.
pub fn compose_call_chat(
    identity: &Identity,
    call_id: &[u8; CALL_ID_LEN],
    peer_id: &str,
    seq: u64,
    text: &str,
) -> Result<Vec<u8>, RelayError> {
    let peer = peer_id.as_bytes();
    check_peer_id(peer)?;
    if seq == 0 {
        return Err(RelayError::Malformed("seq"));
    }
    check_call_chat_text(text).map_err(|_| RelayError::Malformed("text"))?;
    let text_bytes = text.as_bytes();
    let signing = SigningKey::from_seed(&identity.ed25519_seed_bytes());
    let signer_key = signing.verifying_key().to_bytes();
    if identity.signature_public_key() != signer_key.as_slice() {
        return Err(RelayError::IdentityKeyMismatch);
    }
    let signature = signing
        .sign(&signed_bytes(call_id, peer, seq, text_bytes))
        .to_bytes();
    let text_len = u16::try_from(text_bytes.len()).map_err(|_| RelayError::Malformed("text"))?;
    let mut out = Vec::with_capacity(call_chat_len(peer.len(), text_bytes.len()));
    out.extend_from_slice(CALL_CHAT_DOMAIN);
    out.push(CALL_CHAT_VERSION);
    out.extend_from_slice(call_id);
    out.push(u8::try_from(peer.len()).map_err(|_| RelayError::Malformed("peer id length"))?);
    out.extend_from_slice(peer);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&text_len.to_be_bytes());
    out.extend_from_slice(text_bytes);
    out.extend_from_slice(&signer_key);
    out.extend_from_slice(&signature);
    Ok(out)
}

/// Decode a call-chat message without verifying it. Bounded and exact:
/// every length is checked before a slice is taken, and the text passes the
/// same rule [`compose_call_chat`] enforces.
///
/// # Errors
///
/// [`RelayError::WrongDomain`], [`RelayError::UnknownVersion`],
/// [`RelayError::BadLength`] or [`RelayError::Malformed`].
pub fn decode_call_chat(bytes: &[u8]) -> Result<CallChatMessage, RelayError> {
    let rest = bytes
        .strip_prefix(CALL_CHAT_DOMAIN)
        .ok_or(RelayError::WrongDomain)?;
    let (&version, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    if version != CALL_CHAT_VERSION {
        return Err(RelayError::UnknownVersion);
    }
    if rest.len() < CALL_ID_LEN + 1 {
        return Err(RelayError::BadLength);
    }
    let (call_id_bytes, rest) = rest.split_at(CALL_ID_LEN);
    let (&peer_len, rest) = rest.split_first().ok_or(RelayError::BadLength)?;
    let peer_len = usize::from(peer_len);
    if rest.len() < peer_len + 8 + 2 {
        return Err(RelayError::BadLength);
    }
    let (peer, rest) = rest.split_at(peer_len);
    check_peer_id(peer)?;
    let (seq_bytes, rest) = rest.split_at(8);
    let (len_bytes, rest) = rest.split_at(2);
    let text_len = usize::from(u16::from_be_bytes([len_bytes[0], len_bytes[1]]));
    if text_len > MAX_CALL_CHAT_TEXT_BYTES {
        return Err(RelayError::Malformed("text"));
    }
    if rest.len() != text_len + PUBLIC_KEY_LEN + SIGNATURE_LEN {
        return Err(RelayError::BadLength);
    }
    let (text_bytes, rest) = rest.split_at(text_len);
    let (key_bytes, sig_bytes) = rest.split_at(PUBLIC_KEY_LEN);

    let mut seq_be = [0u8; 8];
    seq_be.copy_from_slice(seq_bytes);
    let seq = u64::from_be_bytes(seq_be);
    if seq == 0 {
        return Err(RelayError::Malformed("seq"));
    }
    let text =
        String::from_utf8(text_bytes.to_vec()).map_err(|_| RelayError::Malformed("utf-8"))?;
    check_call_chat_text(&text).map_err(|_| RelayError::Malformed("text"))?;
    let mut call_id = [0u8; CALL_ID_LEN];
    call_id.copy_from_slice(call_id_bytes);
    let mut signer_key = [0u8; PUBLIC_KEY_LEN];
    signer_key.copy_from_slice(key_bytes);
    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(sig_bytes);
    // `check_peer_id` admitted visible ASCII only, so this is UTF-8.
    let peer_id = String::from_utf8(peer.to_vec()).map_err(|_| RelayError::Malformed("utf-8"))?;
    Ok(CallChatMessage {
        call_id,
        peer_id,
        seq,
        text,
        signer_key,
        signature,
    })
}

/// Verify a call-chat message: it names `call_id`, it names `peer_id`, it
/// was signed by `peer_signer_key`, that key is a leaf of `group`, and the
/// signature verifies.
///
/// `peer_id` and `peer_signer_key` are what the caller has already bound to
/// the sender: the relay's stamped sender and the key that sender's
/// verified peer announcement named. Requiring both is what keeps an SFU
/// that colludes with one member from putting that member's words under
/// another member's name.
///
/// # Errors
///
/// Any decode error, then [`RelayError::WrongCall`],
/// [`RelayError::WrongPeer`], [`RelayError::NotAMember`],
/// [`RelayError::NoAccount`] or [`RelayError::BadSignature`].
pub fn verify_call_chat(
    group: &Group,
    call_id: &[u8; CALL_ID_LEN],
    peer_id: &str,
    peer_signer_key: &[u8],
    bytes: &[u8],
) -> Result<VerifiedCallChat, RelayError> {
    let decoded = decode_call_chat(bytes)?;
    if &decoded.call_id != call_id {
        return Err(RelayError::WrongCall);
    }
    if decoded.peer_id != peer_id {
        return Err(RelayError::WrongPeer);
    }
    if decoded.signer_key.as_slice() != peer_signer_key {
        return Err(RelayError::WrongPeer);
    }
    let member = group
        .members()
        .into_iter()
        .find(|m| m.signature_public_key.as_slice() == decoded.signer_key.as_slice())
        .ok_or(RelayError::NotAMember)?;
    let verifying =
        VerifyingKey::from_bytes(&decoded.signer_key).map_err(|_| RelayError::BadSignature)?;
    let signature =
        Signature::from_bytes(&decoded.signature).map_err(|_| RelayError::BadSignature)?;
    verifying
        .verify(
            &signed_bytes(
                &decoded.call_id,
                decoded.peer_id.as_bytes(),
                decoded.seq,
                decoded.text.as_bytes(),
            ),
            &signature,
        )
        .map_err(|_| RelayError::BadSignature)?;
    if member.account_key.is_empty() {
        return Err(RelayError::NoAccount);
    }
    Ok(VerifiedCallChat {
        peer_id: decoded.peer_id,
        seq: decoded.seq,
        text: decoded.text,
        account_key: member.account_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_name::compose_call_name;
    use crate::relay::{classify_relay_plaintext, RelayPayload, MAX_RELAY_PLAINTEXT};
    use lakko_mls::GroupKind;
    use proptest::prelude::*;

    const CALL: [u8; CALL_ID_LEN] = [0x11; CALL_ID_LEN];

    fn identity(seed: u8) -> Identity {
        Identity::from_seed(&[seed; 32]).expect("identity")
    }

    fn two_member_group() -> (Identity, Identity, Group) {
        let alice = identity(1);
        let bob = identity(2);
        let mut group = Group::create(&alice, GroupKind::Group).expect("group");
        let kp = bob.create_key_package().unwrap();
        group.add_member(&alice, kp).expect("add bob");
        (alice, bob, group)
    }

    #[test]
    fn a_message_round_trips_and_classifies() {
        let bob = identity(2);
        let wire = compose_call_chat(&bob, &CALL, "bobpeer", 3, "see you\nat 5").unwrap();
        assert_eq!(wire.len(), call_chat_len(7, 12));
        let d = decode_call_chat(&wire).unwrap();
        assert_eq!(d.call_id, CALL);
        assert_eq!(d.peer_id, "bobpeer");
        assert_eq!(d.seq, 3);
        assert_eq!(d.text, "see you\nat 5");
        assert_eq!(d.signer_key.as_slice(), bob.signature_public_key());
        assert_eq!(
            classify_relay_plaintext(&wire),
            RelayPayload::CallChat(wire.clone())
        );
    }

    #[test]
    fn the_text_rule() {
        assert_eq!(check_call_chat_text(""), Err(CallChatTextError::Empty));
        assert_eq!(
            check_call_chat_text(" \n\t "),
            Err(CallChatTextError::Blank)
        );
        assert_eq!(
            check_call_chat_text("a\u{0007}b"),
            Err(CallChatTextError::ControlCharacter)
        );
        assert_eq!(
            check_call_chat_text("a\rb"),
            Err(CallChatTextError::ControlCharacter)
        );
        assert_eq!(
            check_call_chat_text(&"x".repeat(MAX_CALL_CHAT_TEXT_BYTES + 1)),
            Err(CallChatTextError::TooLong)
        );
        assert!(check_call_chat_text(&"x".repeat(MAX_CALL_CHAT_TEXT_BYTES)).is_ok());
        assert!(check_call_chat_text("  hi  ").is_ok());
        assert!(check_call_chat_text("\u{1F469}\u{200D}\u{1F4BB} ok").is_ok());
        assert!(check_call_chat_text("tab\there").is_ok());
    }

    #[test]
    fn clamp_cuts_on_a_character_boundary() {
        let cyr = "я".repeat(500); // 1000 bytes
        let c = clamp_call_chat_text(&cyr);
        assert_eq!(c.len(), MAX_CALL_CHAT_TEXT_BYTES);
        assert!(check_call_chat_text(c).is_ok());
        let odd = format!("x{}", "я".repeat(500));
        let c = clamp_call_chat_text(&odd);
        assert_eq!(c.len(), MAX_CALL_CHAT_TEXT_BYTES - 1);
        assert_eq!(clamp_call_chat_text("short"), "short");
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        let bob = identity(2);
        for bad in ["", "   ", "a\u{0000}b", &"x".repeat(801)] {
            assert_eq!(
                compose_call_chat(&bob, &CALL, "p", 1, bad),
                Err(RelayError::Malformed("text")),
                "{bad:?}"
            );
        }
        assert_eq!(
            compose_call_chat(&bob, &CALL, "p", 0, "hi"),
            Err(RelayError::Malformed("seq"))
        );
        assert!(compose_call_chat(&bob, &CALL, "", 1, "hi").is_err());
        assert!(compose_call_chat(&bob, &CALL, "a b", 1, "hi").is_err());
    }

    #[test]
    fn a_text_that_breaks_the_rule_is_refused_at_decode() {
        let bob = identity(2);
        let good = compose_call_chat(&bob, &CALL, "p", 1, "hi").unwrap();
        let text_at = CALL_CHAT_DOMAIN.len() + 1 + CALL_ID_LEN + 1 + 1 + 8 + 2;
        let mut bad = good.clone();
        bad[text_at] = 0x07;
        assert_eq!(decode_call_chat(&bad), Err(RelayError::Malformed("text")));
        // An over-long declared length is refused before any slice.
        let mut long = good;
        long[text_at - 2..text_at].copy_from_slice(&801u16.to_be_bytes());
        assert_eq!(decode_call_chat(&long), Err(RelayError::Malformed("text")));
    }

    #[test]
    fn verify_binds_the_message_to_the_announced_leaf() {
        let (alice, bob, group) = two_member_group();
        let wire = compose_call_chat(&bob, &CALL, "bobpeer", 1, "hello").unwrap();
        let ok =
            verify_call_chat(&group, &CALL, "bobpeer", bob.signature_public_key(), &wire).unwrap();
        assert_eq!(ok.text, "hello");
        assert_eq!(ok.seq, 1);
        assert_eq!(ok.account_key.as_slice(), bob.signature_public_key());

        assert_eq!(
            verify_call_chat(
                &group,
                &[0x22; CALL_ID_LEN],
                "bobpeer",
                bob.signature_public_key(),
                &wire
            ),
            Err(RelayError::WrongCall)
        );
        assert_eq!(
            verify_call_chat(
                &group,
                &CALL,
                "alicepeer",
                bob.signature_public_key(),
                &wire
            ),
            Err(RelayError::WrongPeer)
        );
        // Bob writes as Alice's peer id; the SFU stamps it as Alice's.
        let claim = compose_call_chat(&bob, &CALL, "alicepeer", 9, "I am Alice").unwrap();
        assert_eq!(
            verify_call_chat(
                &group,
                &CALL,
                "alicepeer",
                alice.signature_public_key(),
                &claim
            ),
            Err(RelayError::WrongPeer)
        );
        let carol = identity(3);
        let stranger = compose_call_chat(&carol, &CALL, "carol", 1, "hi").unwrap();
        assert_eq!(
            verify_call_chat(
                &group,
                &CALL,
                "carol",
                carol.signature_public_key(),
                &stranger
            ),
            Err(RelayError::NotAMember)
        );
    }

    #[test]
    fn signatures_do_not_cross_with_the_call_name() {
        // A name and a message with the same fields: a name's signature
        // spliced into a message never verifies.
        let (_alice, bob, group) = two_member_group();
        let name = compose_call_name(&bob, &CALL, "bobpeer", 1, "Bob").unwrap();
        let name_sig = &name[name.len() - SIGNATURE_LEN..];
        let mut wire = compose_call_chat(&bob, &CALL, "bobpeer", 1, "Bob").unwrap();
        let at = wire.len() - SIGNATURE_LEN;
        wire[at..].copy_from_slice(name_sig);
        assert_eq!(
            verify_call_chat(&group, &CALL, "bobpeer", bob.signature_public_key(), &wire),
            Err(RelayError::BadSignature)
        );
    }

    #[test]
    fn the_largest_message_fits_a_relay_payload() {
        assert!(call_chat_len(MAX_PEER_ID_LEN, MAX_CALL_CHAT_TEXT_BYTES) <= MAX_RELAY_PLAINTEXT);
    }

    fn valid_text() -> impl Strategy<Value = String> {
        prop_oneof![
            "[A-Za-z0-9]([A-Za-z0-9 \n\t.,!?'-]{0,200})",
            "[\u{0400}-\u{04FF}\u{4E00}-\u{4E80}]{1,200}",
        ]
        .prop_map(|s| clamp_call_chat_text(&s).to_owned())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn compose_then_decode_is_the_identity(
            peer in "[!-~]{1,64}",
            seq in 1u64..=u64::MAX,
            text in valid_text(),
            call_id in proptest::array::uniform16(any::<u8>()),
        ) {
            let bob = identity(2);
            let wire = compose_call_chat(&bob, &call_id, &peer, seq, &text).unwrap();
            prop_assert_eq!(wire.len(), call_chat_len(peer.len(), text.len()));
            prop_assert!(wire.len() <= MAX_RELAY_PLAINTEXT);
            let d = decode_call_chat(&wire).unwrap();
            prop_assert_eq!(d.call_id, call_id);
            prop_assert_eq!(d.peer_id, peer);
            prop_assert_eq!(d.seq, seq);
            prop_assert_eq!(d.text, text);
        }

        #[test]
        fn decode_never_panics_and_refuses_every_damaged_wire(
            bytes in proptest::collection::vec(any::<u8>(), 0..1100),
            cut in 0usize..300,
        ) {
            let _ = decode_call_chat(&bytes);
            let mut prefixed = CALL_CHAT_DOMAIN.to_vec();
            prefixed.extend_from_slice(&bytes);
            let _ = decode_call_chat(&prefixed);
            let bob = identity(2);
            let wire = compose_call_chat(&bob, &CALL, "peer", 1, "message").unwrap();
            let n = cut.min(wire.len() - 1);
            prop_assert!(decode_call_chat(&wire[..n]).is_err());
        }

        #[test]
        fn any_flipped_byte_fails_verification(at in 0usize..1000, bit in 0u8..8) {
            let (_alice, bob, group) = two_member_group();
            let wire = compose_call_chat(&bob, &CALL, "bobpeer", 5, "hello there").unwrap();
            let i = at % wire.len();
            let mut bad = wire;
            bad[i] ^= 1 << bit;
            prop_assert!(verify_call_chat(
                &group, &CALL, "bobpeer", bob.signature_public_key(), &bad
            ).is_err());
        }

        #[test]
        fn clamp_always_yields_an_admissible_prefix(s in "\\PC{0,600}") {
            let c = clamp_call_chat_text(&s);
            prop_assert!(s.starts_with(c));
            prop_assert!(c.len() <= MAX_CALL_CHAT_TEXT_BYTES);
        }
    }
}
