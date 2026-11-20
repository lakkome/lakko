// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Two-way conversation link (ADR-0074 section 3.7).
//!
//! After a key rotation an account's conversation leaves stay bound to its
//! oldest key while the directory and the phrase hold the newest one (Stage
//! A). A contact who pinned an older key follows the rotation by its
//! [supersession proof](crate::supersession). A contact first met after the
//! rotation pinned the newest key and holds no such anchor, so a proof, which
//! the OLD key alone signs, says nothing it can trust: a malicious directory
//! could attach a key of its choosing to a person.
//!
//! This link closes that. Both keys sign the same statement, so it cannot be
//! made without the key the contact pinned:
//!
//! ```text
//!   version(1) = 1
//!   conversation_key(32)
//!   current_key(32)
//!   issued_at_unix_secs(8, i64)
//!   signature_by_conversation_key(64)
//!   signature_by_current_key(64)
//! ```
//!
//! Both signatures cover `DOMAIN ‖ version ‖ conversation_key ‖ current_key ‖
//! issued_at`. There is no expiry: the link states what the account's
//! conversations are bound to, which stays true after the rotation that
//! minted it.
//!
//! [`verify_conversation_link`] is the entry point. It returns the conversation
//! key only when the link names the caller's trusted key as `current_key` and
//! both signatures verify. A client calls it after the identity gate has
//! passed, with the key the gate left pinned.

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};

use crate::error::{Error, Result};

/// Domain separator of the signed statement.
const DOMAIN: &[u8] = b"lakko/transparency/conversation-link/v1\0";

/// Wire version.
const VERSION: u8 = 1;

/// Byte length of a link.
pub const CONVERSATION_LINK_LEN: usize =
    1 + PUBLIC_KEY_LEN + PUBLIC_KEY_LEN + 8 + SIGNATURE_LEN + SIGNATURE_LEN;

/// The fields of a link, before signing or after a successful [`open`].
///
/// [`open`]: ConversationLink::open
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConversationLink {
    /// The key the account's conversation leaves are bound to.
    pub conversation_key: [u8; PUBLIC_KEY_LEN],
    /// The key the directory publishes for the account.
    pub current_key: [u8; PUBLIC_KEY_LEN],
    /// When the link was minted, unix seconds. Informational: nothing expires.
    pub issued_at_unix_secs: i64,
}

impl ConversationLink {
    fn signed_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(DOMAIN.len() + 1 + PUBLIC_KEY_LEN * 2 + 8);
        out.extend_from_slice(DOMAIN);
        out.push(VERSION);
        out.extend_from_slice(&self.conversation_key);
        out.extend_from_slice(&self.current_key);
        out.extend_from_slice(&self.issued_at_unix_secs.to_be_bytes());
        out
    }

    /// Sign the link with both keys, deriving the two public keys from them.
    ///
    /// # Errors
    ///
    /// [`Error::ConversationLinkSameKey`] when the two keys are one: a link
    /// from a key to itself says nothing and is never published.
    pub fn sign(
        conversation_signing_key: &SigningKey,
        current_signing_key: &SigningKey,
        issued_at_unix_secs: i64,
    ) -> Result<[u8; CONVERSATION_LINK_LEN]> {
        let link = Self {
            conversation_key: conversation_signing_key.verifying_key().to_bytes(),
            current_key: current_signing_key.verifying_key().to_bytes(),
            issued_at_unix_secs,
        };
        if link.conversation_key == link.current_key {
            return Err(Error::ConversationLinkSameKey);
        }
        let signed = link.signed_bytes();
        let by_conversation = conversation_signing_key.sign(&signed);
        let by_current = current_signing_key.sign(&signed);

        let mut out = [0u8; CONVERSATION_LINK_LEN];
        out[0] = VERSION;
        let mut at = 1;
        for part in [
            link.conversation_key.as_slice(),
            link.current_key.as_slice(),
            issued_at_unix_secs.to_be_bytes().as_slice(),
            by_conversation.to_bytes().as_slice(),
            by_current.to_bytes().as_slice(),
        ] {
            out[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        }
        Ok(out)
    }

    /// Parse a link and verify both signatures against the keys it names.
    ///
    /// # Errors
    ///
    /// [`Error::ConversationLinkMalformed`] for a wrong length or version, or
    /// a link from a key to itself; [`Error::ConversationLinkInvalid`] for a
    /// key that is not one or a signature that does not verify.
    pub fn open(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != CONVERSATION_LINK_LEN || bytes[0] != VERSION {
            return Err(Error::ConversationLinkMalformed);
        }
        let (conversation_key, rest) = bytes[1..].split_at(PUBLIC_KEY_LEN);
        let (current_key, rest) = rest.split_at(PUBLIC_KEY_LEN);
        let (issued_at, signatures) = rest.split_at(8);
        let (by_conversation, by_current) = signatures.split_at(SIGNATURE_LEN);

        let link = Self {
            conversation_key: conversation_key
                .try_into()
                .map_err(|_| Error::ConversationLinkMalformed)?,
            current_key: current_key
                .try_into()
                .map_err(|_| Error::ConversationLinkMalformed)?,
            issued_at_unix_secs: i64::from_be_bytes(
                issued_at
                    .try_into()
                    .map_err(|_| Error::ConversationLinkMalformed)?,
            ),
        };
        if link.conversation_key == link.current_key {
            return Err(Error::ConversationLinkMalformed);
        }
        let signed = link.signed_bytes();
        for (key, signature) in [
            (&link.conversation_key, by_conversation),
            (&link.current_key, by_current),
        ] {
            let key = VerifyingKey::from_bytes(key).map_err(|_| Error::ConversationLinkInvalid)?;
            let signature =
                Signature::from_bytes(signature).map_err(|_| Error::ConversationLinkInvalid)?;
            key.verify(&signed, &signature)
                .map_err(|_| Error::ConversationLinkInvalid)?;
        }
        Ok(link)
    }
}

/// Verify a link against the key the caller trusts for the account and return
/// the conversation key it names.
///
/// # Errors
///
/// What [`ConversationLink::open`] refuses, and
/// [`Error::ConversationLinkKeyMismatch`] when the link names another key as
/// current: a link for someone else proves nothing about this account.
pub fn verify_conversation_link(
    bytes: &[u8],
    trusted_current_key: &[u8; PUBLIC_KEY_LEN],
) -> Result<[u8; PUBLIC_KEY_LEN]> {
    let link = ConversationLink::open(bytes)?;
    if &link.current_key != trusted_current_key {
        return Err(Error::ConversationLinkKeyMismatch);
    }
    Ok(link.conversation_key)
}

/// The account's OWN conversation key, read from the link its directory
/// bundle serves (ADR-0074, "Joining after a rotation", G-56).
///
/// `link` is the bundle's `conversation_link` field (`None` when absent) and
/// `current_key` THIS device's own current account key, never one fetched
/// from the directory. Answers `None` for an absent or empty link, one that
/// does not verify against `current_key` (minted before a reset, or forged),
/// and one naming the current key itself; otherwise the 32-byte conversation
/// key the account's leaves are bound to. A server can strip the link (the
/// answer is then `None`) but cannot add one: it needs the current key's
/// signature.
#[must_use]
pub fn own_conversation_key(
    link: Option<&[u8]>,
    current_key: &[u8; PUBLIC_KEY_LEN],
) -> Option<[u8; PUBLIC_KEY_LEN]> {
    let link = link.filter(|l| !l.is_empty())?;
    let key = verify_conversation_link(link, current_key).ok()?;
    (key != *current_key).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn key(byte: u8) -> SigningKey {
        SigningKey::from_seed(&[byte; 32])
    }

    fn minted() -> [u8; CONVERSATION_LINK_LEN] {
        ConversationLink::sign(&key(1), &key(2), 1_793_000_000).expect("two keys")
    }

    #[test]
    fn the_own_conversation_key_rule() {
        let current = key(2).verifying_key().to_bytes();
        let link = minted();
        assert_eq!(own_conversation_key(None, &current), None, "absent");
        assert_eq!(own_conversation_key(Some(&[]), &current), None, "empty");
        assert_eq!(
            own_conversation_key(Some(&link), &current),
            Some(key(1).verifying_key().to_bytes()),
            "verifies"
        );
        let other = key(3).verifying_key().to_bytes();
        assert_eq!(
            own_conversation_key(Some(&link), &other),
            None,
            "another current key"
        );
        let mut damaged = link;
        damaged[CONVERSATION_LINK_LEN - 1] ^= 1;
        assert_eq!(
            own_conversation_key(Some(&damaged), &current),
            None,
            "does not verify"
        );
        assert_eq!(
            own_conversation_key(Some(&link[..10]), &current),
            None,
            "truncated"
        );
    }

    #[test]
    fn a_minted_link_names_the_conversation_key_to_whoever_trusts_the_current_one() {
        let current = key(2).verifying_key().to_bytes();
        assert_eq!(
            verify_conversation_link(&minted(), &current).expect("verifies"),
            key(1).verifying_key().to_bytes()
        );
    }

    #[test]
    fn the_layout_is_the_documented_length() {
        assert_eq!(CONVERSATION_LINK_LEN, 201);
        assert_eq!(minted().len(), 201);
    }

    #[test]
    fn a_link_for_another_person_is_refused() {
        let someone_else = key(3).verifying_key().to_bytes();
        assert!(matches!(
            verify_conversation_link(&minted(), &someone_else),
            Err(Error::ConversationLinkKeyMismatch)
        ));
    }

    /// The attack this format exists to stop: a key the contact never pinned
    /// claims a person, signing alone. A proof carries one signature, so here
    /// the current key's slot is filled by the attacker too.
    #[test]
    fn a_link_the_pinned_key_did_not_sign_is_refused() {
        let attacker = key(9);
        let victim = key(2).verifying_key().to_bytes();
        let link = ConversationLink {
            conversation_key: attacker.verifying_key().to_bytes(),
            current_key: victim,
            issued_at_unix_secs: 1_793_000_000,
        };
        let signed = link.signed_bytes();
        let by_attacker = attacker.sign(&signed).to_bytes();

        let mut forged = [0u8; CONVERSATION_LINK_LEN];
        forged[0] = VERSION;
        forged[1..33].copy_from_slice(&link.conversation_key);
        forged[33..65].copy_from_slice(&link.current_key);
        forged[65..73].copy_from_slice(&link.issued_at_unix_secs.to_be_bytes());
        forged[73..137].copy_from_slice(&by_attacker);
        forged[137..201].copy_from_slice(&by_attacker);

        assert!(matches!(
            verify_conversation_link(&forged, &victim),
            Err(Error::ConversationLinkInvalid)
        ));
    }

    #[test]
    fn the_conversation_key_must_have_signed_too() {
        let current = key(2);
        let link = ConversationLink {
            conversation_key: key(1).verifying_key().to_bytes(),
            current_key: current.verifying_key().to_bytes(),
            issued_at_unix_secs: 1_793_000_000,
        };
        let by_current = current.sign(&link.signed_bytes()).to_bytes();
        let mut forged = minted();
        forged[73..137].copy_from_slice(&by_current);
        forged[137..201].copy_from_slice(&by_current);
        assert!(matches!(
            ConversationLink::open(&forged),
            Err(Error::ConversationLinkInvalid)
        ));
    }

    #[test]
    fn a_key_cannot_link_to_itself() {
        assert!(matches!(
            ConversationLink::sign(&key(1), &key(1), 1),
            Err(Error::ConversationLinkSameKey)
        ));
    }

    #[test]
    fn another_length_or_version_is_malformed() {
        let link = minted();
        assert!(matches!(
            ConversationLink::open(&link[..200]),
            Err(Error::ConversationLinkMalformed)
        ));
        let mut longer = link.to_vec();
        longer.push(0);
        assert!(matches!(
            ConversationLink::open(&longer),
            Err(Error::ConversationLinkMalformed)
        ));
        let mut wrong_version = link;
        wrong_version[0] = 2;
        assert!(matches!(
            ConversationLink::open(&wrong_version),
            Err(Error::ConversationLinkMalformed)
        ));
        assert!(matches!(
            ConversationLink::open(&[]),
            Err(Error::ConversationLinkMalformed)
        ));
    }

    proptest! {
        #[test]
        fn every_flipped_byte_is_refused(index in 0..CONVERSATION_LINK_LEN, bit in 0u8..8) {
            let mut link = minted();
            link[index] ^= 1 << bit;
            prop_assert!(ConversationLink::open(&link).is_err());
        }

        #[test]
        fn arbitrary_bytes_never_open(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
            prop_assert!(ConversationLink::open(&bytes).is_err());
        }
    }
}
