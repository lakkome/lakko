// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A subscriber's key for one post, and the proof a subscriber write carries
//! ([ADR-0087] section 13a, "A key per post").
//!
//! ## The key
//!
//! ```text
//! post_key_seed = BLAKE3-derive-key("lakko/channel/post-key/v1",
//!                                   account_seed(32) ‖ channel_id(16) ‖ post_cursor(8, BE))
//! post_key      = Ed25519 from post_key_seed
//! ```
//!
//! `account_seed` is the 32-byte Ed25519 seed of the account identity, so
//! every device of the account derives the same key (and so any of them can
//! take a reaction off), and `post_cursor` is the server cursor of the post
//! being reacted to or commented on. Two posts' keys are unrelated without the
//! account seed, so the service cannot tell that two writes under two posts
//! came from one person. The maintainer refused a per-channel subscriber key
//! on every write (2026-11-01) for exactly that reason; nothing here is
//! reused from post to post.
//!
//! ## The proof
//!
//! Every subscriber write (a reaction added or taken off, a comment added or
//! deleted by its author) is signed by the post key. The proof travels in one
//! header, in the owner proof's shape ([`crate::owner`]):
//!
//! ```text
//! X-Lakko-Channel-Post-Key-Proof: v1.<unix_ms>.<nonce_b64url>.<sig_b64url>
//! ```
//!
//! It signs the write, not the HTTP request, so the bytes do not depend on
//! how the service routes it:
//!
//! ```text
//! "lakko/channel/post-key-proof/v1" ‖ 0x00 ‖ op(1) ‖ channel_id(16) ‖ post_cursor(8, BE)
//!   ‖ post_key(32) ‖ subject ‖ unix_ms(8, BE) ‖ nonce(16)
//! ```
//!
//! | op | write | subject |
//! |---|---|---|
//! | `0x01` | add a reaction | the 16-byte label |
//! | `0x02` | take a reaction off | the 16-byte label |
//! | `0x03` | add a comment | `BLAKE3(sealed)`, 32 bytes |
//! | `0x04` | delete one's own comment | the comment's server cursor, 8 bytes BE |
//!
//! The op fixes the subject's length, so the encoding is injective. The
//! domain is distinct from every other proof domain in the crate, so a post
//! key proof is never an owner, co-publisher or invite proof.
//!
//! ## What the service checks
//!
//! [`verify_post_key_proof`] is pure: the window ([`POST_KEY_PROOF_WINDOW_MS`],
//! the ADR-0053 window) first, then that the post key is a valid Ed25519 key
//! of full order, then the signature. The service also refuses a nonce it has
//! seen within the window (state), so a captured request cannot be replayed
//! to put back a reaction its author took off. An add also spends one
//! anonymous token (`channel-react` or `channel-comment`, ADR-0054); a delete
//! spends none, since only the holder of the post key can sign it.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use lakko_crypto::hash;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN};

use crate::owner::{
    OwnerProof, OwnerProofError, ACCOUNT_SEED_LEN, OWNER_PROOF_NONCE_LEN, OWNER_PROOF_WINDOW_MS,
};
use crate::reaction::REACTION_LABEL_LEN;
use crate::CHANNEL_ID_LEN;

/// BLAKE3 derive-key context of a post key seed. Frozen.
pub const POST_KEY_DOMAIN: &str = "lakko/channel/post-key/v1";
/// Domain separator that opens a post key proof's canonical bytes. Frozen.
pub const POST_KEY_PROOF_DOMAIN: &[u8] = b"lakko/channel/post-key-proof/v1";
/// The request header a post key proof travels in (lowercase).
pub const POST_KEY_PROOF_HEADER_NAME: &str = "x-lakko-channel-post-key-proof";
/// How far from the service's clock a proof's `unix_ms` may lie, either way.
/// The ADR-0053 window.
pub const POST_KEY_PROOF_WINDOW_MS: u64 = OWNER_PROOF_WINDOW_MS;
/// Length of a post key's public half.
pub const POST_KEY_LEN: usize = PUBLIC_KEY_LEN;

/// A post key proof: the owner proof's header shape under its own domain.
/// Parse it with [`OwnerProof::parse`] and encode it with
/// [`OwnerProof::encode`].
pub type PostKeyProof = OwnerProof;

/// Why a post key proof was refused: the owner proof's reasons. The service
/// maps every one to the same response.
pub type PostKeyProofError = OwnerProofError;

const SEP: u8 = 0;

/// Derive the post key seed of `post_cursor` in `channel_id` from the
/// account's identity seed.
#[must_use]
pub fn channel_post_key_seed(
    account_seed: &[u8; ACCOUNT_SEED_LEN],
    channel_id: &[u8; CHANNEL_ID_LEN],
    post_cursor: u64,
) -> [u8; 32] {
    let mut material = [0u8; ACCOUNT_SEED_LEN + CHANNEL_ID_LEN + 8];
    material[..ACCOUNT_SEED_LEN].copy_from_slice(account_seed);
    material[ACCOUNT_SEED_LEN..ACCOUNT_SEED_LEN + CHANNEL_ID_LEN].copy_from_slice(channel_id);
    material[ACCOUNT_SEED_LEN + CHANNEL_ID_LEN..].copy_from_slice(&post_cursor.to_be_bytes());
    let out = hash::blake3_derive_key_from_material(POST_KEY_DOMAIN, &material);
    material.fill(0);
    out
}

/// One subscriber write under a post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriberWrite<'a> {
    /// Add a reaction row with this label.
    AddReaction {
        /// The reaction label ([`crate::reaction::ReactionKey::label`]).
        label: [u8; REACTION_LABEL_LEN],
    },
    /// Take off the reaction row with this label.
    DeleteReaction {
        /// The reaction label.
        label: [u8; REACTION_LABEL_LEN],
    },
    /// Add a comment row holding `sealed` ([`crate::comment::seal_comment`]).
    AddComment {
        /// The sealed comment, exactly as stored.
        sealed: &'a [u8],
    },
    /// Delete one's own comment row.
    DeleteComment {
        /// The comment's server cursor.
        comment_cursor: u64,
    },
}

impl SubscriberWrite<'_> {
    /// The op byte. Frozen.
    #[must_use]
    pub const fn op(&self) -> u8 {
        match self {
            Self::AddReaction { .. } => 0x01,
            Self::DeleteReaction { .. } => 0x02,
            Self::AddComment { .. } => 0x03,
            Self::DeleteComment { .. } => 0x04,
        }
    }

    fn put_subject(&self, out: &mut Vec<u8>) {
        match self {
            Self::AddReaction { label } | Self::DeleteReaction { label } => {
                out.extend_from_slice(label);
            },
            Self::AddComment { sealed } => out.extend_from_slice(&hash::blake3(sealed)),
            Self::DeleteComment { comment_cursor } => {
                out.extend_from_slice(&comment_cursor.to_be_bytes());
            },
        }
    }
}

/// The parts of a write a post key proof binds, as the service reads them
/// from the request it routed.
#[derive(Clone, Copy, Debug)]
pub struct PostKeyStatement<'a> {
    /// The channel.
    pub channel_id: &'a [u8; CHANNEL_ID_LEN],
    /// The post's server cursor.
    pub post_cursor: u64,
    /// The post key's public half, as the row names it.
    pub post_key: &'a [u8; POST_KEY_LEN],
    /// What is written.
    pub write: SubscriberWrite<'a>,
}

/// The bytes a post key proof signs.
#[must_use]
pub fn post_key_proof_canonical_bytes(
    statement: &PostKeyStatement<'_>,
    unix_ms: u64,
    nonce: &[u8; OWNER_PROOF_NONCE_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        POST_KEY_PROOF_DOMAIN.len()
            + 2
            + CHANNEL_ID_LEN
            + 8
            + POST_KEY_LEN
            + 32
            + 8
            + OWNER_PROOF_NONCE_LEN,
    );
    out.extend_from_slice(POST_KEY_PROOF_DOMAIN);
    out.push(SEP);
    out.push(statement.write.op());
    out.extend_from_slice(statement.channel_id);
    out.extend_from_slice(&statement.post_cursor.to_be_bytes());
    out.extend_from_slice(statement.post_key);
    statement.write.put_subject(&mut out);
    out.extend_from_slice(&unix_ms.to_be_bytes());
    out.extend_from_slice(nonce);
    out
}

/// Verify `proof` over `statement` with `now_ms` the service's clock.
///
/// # Errors
///
/// [`PostKeyProofError::OutsideWindow`] (checked first, so a stale proof
/// costs no signature check), or [`PostKeyProofError::BadSignature`] for a
/// post key that is not a valid Ed25519 key of full order or a signature that
/// does not verify.
pub fn verify_post_key_proof(
    proof: &PostKeyProof,
    statement: &PostKeyStatement<'_>,
    now_ms: u64,
) -> Result<(), PostKeyProofError> {
    if now_ms.abs_diff(proof.unix_ms) > POST_KEY_PROOF_WINDOW_MS {
        return Err(OwnerProofError::OutsideWindow);
    }
    let key =
        VerifyingKey::from_bytes(statement.post_key).map_err(|_| OwnerProofError::BadSignature)?;
    // A small-order key verifies a forged signature over any message.
    if key.is_weak() {
        return Err(OwnerProofError::BadSignature);
    }
    let signature =
        Signature::from_bytes(&proof.signature).map_err(|_| OwnerProofError::BadSignature)?;
    key.verify(
        &post_key_proof_canonical_bytes(statement, proof.unix_ms, &proof.nonce),
        &signature,
    )
    .map_err(|_| OwnerProofError::BadSignature)
}

/// The account's key for one post: the signer of every write under it.
/// Clock and nonce are the caller's.
pub struct ChannelPostKey {
    key: SigningKey,
    channel_id: [u8; CHANNEL_ID_LEN],
    post_cursor: u64,
}

impl core::fmt::Debug for ChannelPostKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChannelPostKey")
            .field("public_key", &self.key.verifying_key().to_bytes())
            .field("post_cursor", &self.post_cursor)
            .finish_non_exhaustive()
    }
}

impl ChannelPostKey {
    /// The key of the post at `post_cursor` in `channel_id`, for the account
    /// whose identity seed is `account_seed`.
    #[must_use]
    pub fn from_account_seed(
        account_seed: &[u8; ACCOUNT_SEED_LEN],
        channel_id: &[u8; CHANNEL_ID_LEN],
        post_cursor: u64,
    ) -> Self {
        let mut seed = channel_post_key_seed(account_seed, channel_id, post_cursor);
        let key = SigningKey::from_seed(&seed);
        seed.fill(0);
        Self {
            key,
            channel_id: *channel_id,
            post_cursor,
        }
    }

    /// The public half a row names.
    #[must_use]
    pub fn public_key(&self) -> [u8; POST_KEY_LEN] {
        self.key.verifying_key().to_bytes()
    }

    /// The post this key is for.
    #[must_use]
    pub const fn post_cursor(&self) -> u64 {
        self.post_cursor
    }

    /// Sign `write` under this post at `now_ms` with `nonce`.
    #[must_use]
    pub fn sign_at(
        &self,
        write: SubscriberWrite<'_>,
        now_ms: u64,
        nonce: [u8; OWNER_PROOF_NONCE_LEN],
    ) -> PostKeyProof {
        let public = self.public_key();
        let statement = PostKeyStatement {
            channel_id: &self.channel_id,
            post_cursor: self.post_cursor,
            post_key: &public,
            write,
        };
        let bytes = post_key_proof_canonical_bytes(&statement, now_ms, &nonce);
        OwnerProof {
            unix_ms: now_ms,
            nonce,
            signature: self.key.sign(&bytes).to_bytes(),
        }
    }
}

/// Fuzz oracle behind `fuzz_api::post_key_proof_parse`: parse `value` as a
/// post key proof header and verify it for a fixed write. `None` when it does
/// not parse; `Some(true)` when it re-encodes to the same text.
pub(crate) fn fuzz_parse_proof(value: &str) -> Option<bool> {
    let proof = OwnerProof::parse(value).ok()?;
    let statement = PostKeyStatement {
        channel_id: &[0x0C; CHANNEL_ID_LEN],
        post_cursor: 7,
        post_key: &[0x5B; POST_KEY_LEN],
        write: SubscriberWrite::DeleteComment { comment_cursor: 9 },
    };
    let _ = verify_post_key_proof(&proof, &statement, 1_700_000_000_000);
    Some(proof.encode() == value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];
    const ACCOUNT: [u8; 32] = [0x11; 32];
    const NOW: u64 = 1_800_000_000_000;

    fn statement_for<'a>(
        key: &'a [u8; POST_KEY_LEN],
        cursor: u64,
        write: SubscriberWrite<'a>,
    ) -> PostKeyStatement<'a> {
        PostKeyStatement {
            channel_id: &CHANNEL,
            post_cursor: cursor,
            post_key: key,
            write,
        }
    }

    #[test]
    fn the_seed_is_pinned_and_depends_on_every_input() {
        let seed = channel_post_key_seed(&ACCOUNT, &CHANNEL, 7);
        let mut material = ACCOUNT.to_vec();
        material.extend_from_slice(&CHANNEL);
        material.extend_from_slice(&7u64.to_be_bytes());
        assert_eq!(
            seed,
            hash::blake3_derive_key_from_material("lakko/channel/post-key/v1", &material)
        );
        assert_ne!(seed, channel_post_key_seed(&ACCOUNT, &CHANNEL, 8));
        assert_ne!(seed, channel_post_key_seed(&[0x12; 32], &CHANNEL, 7));
        assert_ne!(
            seed,
            channel_post_key_seed(&ACCOUNT, &[0x0D; CHANNEL_ID_LEN], 7)
        );
        // Not the owner key of the same channel.
        assert_ne!(seed, crate::owner::channel_owner_seed(&ACCOUNT, &CHANNEL));
    }

    #[test]
    fn two_devices_of_one_account_derive_one_key_and_two_posts_two_keys() {
        let a = ChannelPostKey::from_account_seed(&ACCOUNT, &CHANNEL, 7);
        let b = ChannelPostKey::from_account_seed(&ACCOUNT, &CHANNEL, 7);
        assert_eq!(a.public_key(), b.public_key());
        let other = ChannelPostKey::from_account_seed(&ACCOUNT, &CHANNEL, 8);
        assert_ne!(a.public_key(), other.public_key());
        assert!(format!("{a:?}").starts_with("ChannelPostKey"));
    }

    #[test]
    fn every_write_round_trips_through_the_header_and_verifies() {
        let key = ChannelPostKey::from_account_seed(&ACCOUNT, &CHANNEL, 7);
        let public = key.public_key();
        let sealed = [0xAB; 40];
        for write in [
            SubscriberWrite::AddReaction { label: [1; 16] },
            SubscriberWrite::DeleteReaction { label: [1; 16] },
            SubscriberWrite::AddComment { sealed: &sealed },
            SubscriberWrite::DeleteComment { comment_cursor: 3 },
        ] {
            let proof = key.sign_at(write, NOW, [9; 16]);
            let header = proof.encode();
            let parsed = OwnerProof::parse(&header).unwrap();
            assert_eq!(parsed, proof);
            assert_eq!(fuzz_parse_proof(&header), Some(true));
            assert_eq!(
                verify_post_key_proof(&parsed, &statement_for(&public, 7, write), NOW + 1_000),
                Ok(())
            );
        }
    }

    /// A proof binds the op, the post, the channel, the key and the subject:
    /// an add is not a delete, a reaction on one post is not one on another,
    /// and a delete of one comment is not a delete of another.
    #[test]
    fn a_proof_covers_exactly_one_write() {
        let key = ChannelPostKey::from_account_seed(&ACCOUNT, &CHANNEL, 7);
        let public = key.public_key();
        let add = SubscriberWrite::AddReaction { label: [1; 16] };
        let proof = key.sign_at(add, NOW, [9; 16]);
        let refused = |s: PostKeyStatement<'_>| {
            assert_eq!(
                verify_post_key_proof(&proof, &s, NOW),
                Err(OwnerProofError::BadSignature)
            );
        };
        refused(statement_for(
            &public,
            7,
            SubscriberWrite::DeleteReaction { label: [1; 16] },
        ));
        refused(statement_for(
            &public,
            7,
            SubscriberWrite::AddReaction { label: [2; 16] },
        ));
        refused(statement_for(&public, 8, add));
        let other = ChannelPostKey::from_account_seed(&[0x12; 32], &CHANNEL, 7).public_key();
        refused(statement_for(&other, 7, add));
        refused(PostKeyStatement {
            channel_id: &[0x0D; CHANNEL_ID_LEN],
            post_cursor: 7,
            post_key: &public,
            write: add,
        });

        let del = key.sign_at(
            SubscriberWrite::DeleteComment { comment_cursor: 3 },
            NOW,
            [9; 16],
        );
        assert_eq!(
            verify_post_key_proof(
                &del,
                &statement_for(
                    &public,
                    7,
                    SubscriberWrite::DeleteComment { comment_cursor: 4 }
                ),
                NOW
            ),
            Err(OwnerProofError::BadSignature)
        );
        let sealed = [0xAB; 40];
        let comment = key.sign_at(
            SubscriberWrite::AddComment { sealed: &sealed },
            NOW,
            [9; 16],
        );
        let mut other_sealed = sealed;
        other_sealed[0] ^= 1;
        assert_eq!(
            verify_post_key_proof(
                &comment,
                &statement_for(
                    &public,
                    7,
                    SubscriberWrite::AddComment {
                        sealed: &other_sealed
                    }
                ),
                NOW
            ),
            Err(OwnerProofError::BadSignature)
        );
    }

    #[test]
    fn the_window_is_checked_first_and_a_weak_key_is_refused() {
        let key = ChannelPostKey::from_account_seed(&ACCOUNT, &CHANNEL, 7);
        let public = key.public_key();
        let write = SubscriberWrite::AddReaction { label: [1; 16] };
        let proof = key.sign_at(write, NOW, [9; 16]);
        let s = statement_for(&public, 7, write);
        assert_eq!(
            verify_post_key_proof(&proof, &s, NOW + POST_KEY_PROOF_WINDOW_MS),
            Ok(())
        );
        assert_eq!(
            verify_post_key_proof(&proof, &s, NOW - POST_KEY_PROOF_WINDOW_MS),
            Ok(())
        );
        assert_eq!(
            verify_post_key_proof(&proof, &s, NOW + POST_KEY_PROOF_WINDOW_MS + 1),
            Err(OwnerProofError::OutsideWindow)
        );
        // The identity point decodes and is of small order.
        let mut weak = [0u8; 32];
        weak[0] = 1;
        assert_eq!(
            verify_post_key_proof(&proof, &statement_for(&weak, 7, write), NOW),
            Err(OwnerProofError::BadSignature)
        );
    }

    /// The canonical bytes are pinned: a change to the layout fails here
    /// before it fails between a client and the service.
    #[test]
    fn the_canonical_bytes_are_pinned() {
        let public = [0x5B; POST_KEY_LEN];
        let s = statement_for(&public, 7, SubscriberWrite::AddReaction { label: [1; 16] });
        let bytes = post_key_proof_canonical_bytes(&s, 5, &[9; 16]);
        let mut expected = b"lakko/channel/post-key-proof/v1\x00\x01".to_vec();
        expected.extend_from_slice(&CHANNEL);
        expected.extend_from_slice(&7u64.to_be_bytes());
        expected.extend_from_slice(&public);
        expected.extend_from_slice(&[1; 16]);
        expected.extend_from_slice(&5u64.to_be_bytes());
        expected.extend_from_slice(&[9; 16]);
        assert_eq!(bytes, expected);
        // Owner proofs and post key proofs never share bytes.
        assert_ne!(POST_KEY_PROOF_DOMAIN, crate::owner::OWNER_PROOF_DOMAIN);
    }
}
