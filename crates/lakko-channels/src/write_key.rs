// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A subscriber's key for one write under a post, and the proof the write
//! carries ([ADR-0087] section 13a).
//!
//! ## A key per write
//!
//! A subscriber has no MLS leaf, so a reaction or a comment is a row at the
//! channel service. The maintainer refused a key reused across writes
//! (2026-11-01 for posts, 2026-11-02 within one post): the service must not be
//! able to tell that two rows came from one person, not even a reaction and a
//! comment under the same post. So every write has its own key:
//!
//! ```text
//! reaction: seed = BLAKE3-derive-key("lakko/channel/reaction-write-key/v1",
//!                                    account_seed(32) ‖ channel_id(16) ‖ post_cursor(8, BE) ‖ label(16))
//! comment:  seed = BLAKE3-derive-key("lakko/channel/comment-write-key/v1",
//!                                    account_seed(32) ‖ channel_id(16) ‖ post_cursor(8, BE) ‖ comment_nonce(16))
//! key = Ed25519 from seed
//! ```
//!
//! `account_seed` is the 32-byte Ed25519 seed of the account identity, so
//! every device of the account derives the same key.
//!
//! * A **reaction** key folds in the label, so adding and taking off one
//!   reaction use one key (the service keeps one row per `(post, key, label)`
//!   and only that key's holder deletes it), while two reactions of one
//!   person under one post have unrelated keys.
//! * A **comment** key folds in a random 16-byte `comment_nonce` the author
//!   picks ([`new_comment_nonce`]); the author's other devices learn it from
//!   the account's own-comments register (`lakko_sync`), which is what lets any
//!   of them delete the comment.
//!
//! The two domains differ, so a reaction key never equals a comment key even
//! if a label and a nonce share bytes.
//!
//! ## The proof
//!
//! Every write is signed by its key. The proof travels in one header, in the
//! owner proof's shape ([`crate::owner`]):
//!
//! ```text
//! X-Lakko-Channel-Write-Proof: v1.<unix_ms>.<nonce_b64url>.<sig_b64url>
//! ```
//!
//! It signs the write, not the HTTP request:
//!
//! ```text
//! "lakko/channel/write-proof/v1" ‖ 0x00 ‖ op(1) ‖ channel_id(16) ‖ post_cursor(8, BE)
//!   ‖ writer_key(32) ‖ subject ‖ unix_ms(8, BE) ‖ nonce(16)
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
//! domain is distinct from every other proof domain in the crate.
//!
//! ## What the service checks
//!
//! [`verify_write_proof`] is pure: the window ([`WRITE_PROOF_WINDOW_MS`], the
//! ADR-0053 window) first, then that the writer key is a valid Ed25519 key of
//! full order, then the signature. The service then refuses a nonce it has
//! seen within twice the window (state), so a captured request cannot be
//! replayed to put back a reaction its author took off, and only then spends
//! the anonymous token an add costs (`channel-react` or `channel-comment`,
//! ADR-0054). A delete spends none: only the key's holder can sign it.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use lakko_crypto::hash;
use lakko_crypto::random;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN};

use crate::owner::{
    OwnerProof, OwnerProofError, ACCOUNT_SEED_LEN, OWNER_PROOF_NONCE_LEN, OWNER_PROOF_WINDOW_MS,
};
use crate::reaction::ReactionLabel;
use crate::CHANNEL_ID_LEN;

/// BLAKE3 derive-key context of a reaction write key. Frozen.
pub const REACTION_WRITE_KEY_DOMAIN: &str = "lakko/channel/reaction-write-key/v1";
/// BLAKE3 derive-key context of a comment write key. Frozen.
pub const COMMENT_WRITE_KEY_DOMAIN: &str = "lakko/channel/comment-write-key/v1";
/// Domain separator that opens a write proof's canonical bytes. Frozen.
pub const WRITE_PROOF_DOMAIN: &[u8] = b"lakko/channel/write-proof/v1";
/// The request header a write proof travels in (lowercase).
pub const WRITE_PROOF_HEADER_NAME: &str = "x-lakko-channel-write-proof";
/// How far from the service's clock a proof's `unix_ms` may lie, either way.
/// The ADR-0053 window.
pub const WRITE_PROOF_WINDOW_MS: u64 = OWNER_PROOF_WINDOW_MS;
/// Length of a write key's public half.
pub const WRITE_KEY_LEN: usize = PUBLIC_KEY_LEN;
/// Length of a comment nonce.
pub const COMMENT_NONCE_LEN: usize = 16;

/// A write proof: the owner proof's header shape under its own domain.
/// Parse it with [`OwnerProof::parse`] and encode it with
/// [`OwnerProof::encode`].
pub type WriteProof = OwnerProof;

/// Why a write proof was refused: the owner proof's reasons. The service
/// maps every one to the same response.
pub type WriteProofError = OwnerProofError;

const SEP: u8 = 0;

fn derive(domain: &str, account_seed: &[u8; ACCOUNT_SEED_LEN], parts: &[&[u8]]) -> [u8; 32] {
    let mut material = Vec::with_capacity(ACCOUNT_SEED_LEN + CHANNEL_ID_LEN + 8 + 16);
    material.extend_from_slice(account_seed);
    for p in parts {
        material.extend_from_slice(p);
    }
    let out = hash::blake3_derive_key_from_material(domain, &material);
    material.fill(0);
    out
}

/// Derive the write key seed of the reaction `label` on the post at
/// `post_cursor` in `channel_id`.
#[must_use]
pub fn reaction_write_seed(
    account_seed: &[u8; ACCOUNT_SEED_LEN],
    channel_id: &[u8; CHANNEL_ID_LEN],
    post_cursor: u64,
    label: &ReactionLabel,
) -> [u8; 32] {
    derive(
        REACTION_WRITE_KEY_DOMAIN,
        account_seed,
        &[channel_id, &post_cursor.to_be_bytes(), label],
    )
}

/// Derive the write key seed of the comment `comment_nonce` on the post at
/// `post_cursor` in `channel_id`.
#[must_use]
pub fn comment_write_seed(
    account_seed: &[u8; ACCOUNT_SEED_LEN],
    channel_id: &[u8; CHANNEL_ID_LEN],
    post_cursor: u64,
    comment_nonce: &[u8; COMMENT_NONCE_LEN],
) -> [u8; 32] {
    derive(
        COMMENT_WRITE_KEY_DOMAIN,
        account_seed,
        &[channel_id, &post_cursor.to_be_bytes(), comment_nonce],
    )
}

/// A fresh random comment nonce from the OS CSPRNG.
#[must_use]
pub fn new_comment_nonce() -> [u8; COMMENT_NONCE_LEN] {
    let mut n = [0u8; COMMENT_NONCE_LEN];
    random::fill(&mut n);
    n
}

/// One subscriber write under a post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriberWrite<'a> {
    /// Add a reaction row with this label.
    AddReaction {
        /// The reaction label ([`crate::reaction::ReactionKey::label`]).
        label: ReactionLabel,
    },
    /// Take off the reaction row with this label.
    DeleteReaction {
        /// The reaction label.
        label: ReactionLabel,
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

/// The parts of a write a proof binds, as the service reads them from the
/// request it routed (or, for a comment delete, from the stored row).
#[derive(Clone, Copy, Debug)]
pub struct WriteStatement<'a> {
    /// The channel.
    pub channel_id: &'a [u8; CHANNEL_ID_LEN],
    /// The post's server cursor.
    pub post_cursor: u64,
    /// The write key's public half, as the row names it.
    pub writer_key: &'a [u8; WRITE_KEY_LEN],
    /// What is written.
    pub write: SubscriberWrite<'a>,
}

/// The bytes a write proof signs.
#[must_use]
pub fn write_proof_canonical_bytes(
    statement: &WriteStatement<'_>,
    unix_ms: u64,
    nonce: &[u8; OWNER_PROOF_NONCE_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        WRITE_PROOF_DOMAIN.len()
            + 2
            + CHANNEL_ID_LEN
            + 8
            + WRITE_KEY_LEN
            + 32
            + 8
            + OWNER_PROOF_NONCE_LEN,
    );
    out.extend_from_slice(WRITE_PROOF_DOMAIN);
    out.push(SEP);
    out.push(statement.write.op());
    out.extend_from_slice(statement.channel_id);
    out.extend_from_slice(&statement.post_cursor.to_be_bytes());
    out.extend_from_slice(statement.writer_key);
    statement.write.put_subject(&mut out);
    out.extend_from_slice(&unix_ms.to_be_bytes());
    out.extend_from_slice(nonce);
    out
}

/// Verify `proof` over `statement` with `now_ms` the service's clock.
///
/// # Errors
///
/// [`WriteProofError::OutsideWindow`] (checked first, so a stale proof costs
/// no signature check), or [`WriteProofError::BadSignature`] for a writer key
/// that is not a valid Ed25519 key of full order or a signature that does not
/// verify.
pub fn verify_write_proof(
    proof: &WriteProof,
    statement: &WriteStatement<'_>,
    now_ms: u64,
) -> Result<(), WriteProofError> {
    if now_ms.abs_diff(proof.unix_ms) > WRITE_PROOF_WINDOW_MS {
        return Err(OwnerProofError::OutsideWindow);
    }
    let key = VerifyingKey::from_bytes(statement.writer_key)
        .map_err(|_| OwnerProofError::BadSignature)?;
    // A small-order key verifies a forged signature over any message.
    if key.is_weak() {
        return Err(OwnerProofError::BadSignature);
    }
    let signature =
        Signature::from_bytes(&proof.signature).map_err(|_| OwnerProofError::BadSignature)?;
    key.verify(
        &write_proof_canonical_bytes(statement, proof.unix_ms, &proof.nonce),
        &signature,
    )
    .map_err(|_| OwnerProofError::BadSignature)
}

/// The signing half shared by the two key types.
struct WriteSigner {
    key: SigningKey,
    channel_id: [u8; CHANNEL_ID_LEN],
    post_cursor: u64,
}

impl WriteSigner {
    fn from_seed(mut seed: [u8; 32], channel_id: &[u8; CHANNEL_ID_LEN], post_cursor: u64) -> Self {
        let key = SigningKey::from_seed(&seed);
        seed.fill(0);
        Self {
            key,
            channel_id: *channel_id,
            post_cursor,
        }
    }

    fn public_key(&self) -> [u8; WRITE_KEY_LEN] {
        self.key.verifying_key().to_bytes()
    }

    fn sign(
        &self,
        write: SubscriberWrite<'_>,
        now_ms: u64,
        nonce: [u8; OWNER_PROOF_NONCE_LEN],
    ) -> WriteProof {
        let public = self.public_key();
        let statement = WriteStatement {
            channel_id: &self.channel_id,
            post_cursor: self.post_cursor,
            writer_key: &public,
            write,
        };
        let bytes = write_proof_canonical_bytes(&statement, now_ms, &nonce);
        OwnerProof {
            unix_ms: now_ms,
            nonce,
            signature: self.key.sign(&bytes).to_bytes(),
        }
    }
}

/// The account's key for one reaction under one post. It signs adding and
/// taking off that reaction, nothing else. Clock and nonce are the caller's.
pub struct ReactionWriteKey {
    signer: WriteSigner,
    label: ReactionLabel,
}

impl core::fmt::Debug for ReactionWriteKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ReactionWriteKey")
            .field("public_key", &self.signer.public_key())
            .field("post_cursor", &self.signer.post_cursor)
            .finish_non_exhaustive()
    }
}

impl ReactionWriteKey {
    /// The key of the reaction `label` on the post at `post_cursor` in
    /// `channel_id`, for the account whose identity seed is `account_seed`.
    #[must_use]
    pub fn new(
        account_seed: &[u8; ACCOUNT_SEED_LEN],
        channel_id: &[u8; CHANNEL_ID_LEN],
        post_cursor: u64,
        label: ReactionLabel,
    ) -> Self {
        let seed = reaction_write_seed(account_seed, channel_id, post_cursor, &label);
        Self {
            signer: WriteSigner::from_seed(seed, channel_id, post_cursor),
            label,
        }
    }

    /// The public half the reaction row names.
    #[must_use]
    pub fn public_key(&self) -> [u8; WRITE_KEY_LEN] {
        self.signer.public_key()
    }

    /// The label this key is for.
    #[must_use]
    pub const fn label(&self) -> ReactionLabel {
        self.label
    }

    /// Sign adding the reaction.
    #[must_use]
    pub fn sign_add(&self, now_ms: u64, nonce: [u8; OWNER_PROOF_NONCE_LEN]) -> WriteProof {
        self.signer.sign(
            SubscriberWrite::AddReaction { label: self.label },
            now_ms,
            nonce,
        )
    }

    /// Sign taking the reaction off.
    #[must_use]
    pub fn sign_delete(&self, now_ms: u64, nonce: [u8; OWNER_PROOF_NONCE_LEN]) -> WriteProof {
        self.signer.sign(
            SubscriberWrite::DeleteReaction { label: self.label },
            now_ms,
            nonce,
        )
    }
}

/// The account's key for one comment under one post. It signs adding that
/// comment and deleting it, nothing else. Clock and nonce are the caller's.
pub struct CommentWriteKey {
    signer: WriteSigner,
}

impl core::fmt::Debug for CommentWriteKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CommentWriteKey")
            .field("public_key", &self.signer.public_key())
            .field("post_cursor", &self.signer.post_cursor)
            .finish_non_exhaustive()
    }
}

impl CommentWriteKey {
    /// The key of the comment `comment_nonce` on the post at `post_cursor` in
    /// `channel_id`, for the account whose identity seed is `account_seed`.
    #[must_use]
    pub fn new(
        account_seed: &[u8; ACCOUNT_SEED_LEN],
        channel_id: &[u8; CHANNEL_ID_LEN],
        post_cursor: u64,
        comment_nonce: &[u8; COMMENT_NONCE_LEN],
    ) -> Self {
        let seed = comment_write_seed(account_seed, channel_id, post_cursor, comment_nonce);
        Self {
            signer: WriteSigner::from_seed(seed, channel_id, post_cursor),
        }
    }

    /// The public half the comment row names; also what the comment's seal is
    /// keyed and bound to ([`crate::comment::CommentPlace::writer_key`]).
    #[must_use]
    pub fn public_key(&self) -> [u8; WRITE_KEY_LEN] {
        self.signer.public_key()
    }

    /// Sign adding the comment whose sealed bytes are `sealed`.
    #[must_use]
    pub fn sign_add(
        &self,
        sealed: &[u8],
        now_ms: u64,
        nonce: [u8; OWNER_PROOF_NONCE_LEN],
    ) -> WriteProof {
        self.signer
            .sign(SubscriberWrite::AddComment { sealed }, now_ms, nonce)
    }

    /// Sign deleting the comment the service filed at `comment_cursor`.
    #[must_use]
    pub fn sign_delete(
        &self,
        comment_cursor: u64,
        now_ms: u64,
        nonce: [u8; OWNER_PROOF_NONCE_LEN],
    ) -> WriteProof {
        self.signer.sign(
            SubscriberWrite::DeleteComment { comment_cursor },
            now_ms,
            nonce,
        )
    }
}

/// Fuzz oracle behind `fuzz_api::write_proof_parse`: parse `value` as a write
/// proof header and verify it for a fixed write. `None` when it does not
/// parse; `Some(true)` when it re-encodes to the same text.
pub(crate) fn fuzz_parse_proof(value: &str) -> Option<bool> {
    let proof = OwnerProof::parse(value).ok()?;
    let statement = WriteStatement {
        channel_id: &[0x0C; CHANNEL_ID_LEN],
        post_cursor: 7,
        writer_key: &[0x5B; WRITE_KEY_LEN],
        write: SubscriberWrite::DeleteComment { comment_cursor: 9 },
    };
    let _ = verify_write_proof(&proof, &statement, 1_700_000_000_000);
    Some(proof.encode() == value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];
    const ACCOUNT: [u8; 32] = [0x11; 32];
    const NOW: u64 = 1_800_000_000_000;
    const LABEL: ReactionLabel = [1; 16];

    fn statement_for<'a>(
        key: &'a [u8; WRITE_KEY_LEN],
        cursor: u64,
        write: SubscriberWrite<'a>,
    ) -> WriteStatement<'a> {
        WriteStatement {
            channel_id: &CHANNEL,
            post_cursor: cursor,
            writer_key: key,
            write,
        }
    }

    #[test]
    fn the_seeds_are_pinned_and_depend_on_every_input() {
        let r = reaction_write_seed(&ACCOUNT, &CHANNEL, 7, &LABEL);
        let mut material = ACCOUNT.to_vec();
        material.extend_from_slice(&CHANNEL);
        material.extend_from_slice(&7u64.to_be_bytes());
        material.extend_from_slice(&LABEL);
        assert_eq!(
            r,
            hash::blake3_derive_key_from_material("lakko/channel/reaction-write-key/v1", &material)
        );
        assert_eq!(
            comment_write_seed(&ACCOUNT, &CHANNEL, 7, &LABEL),
            hash::blake3_derive_key_from_material("lakko/channel/comment-write-key/v1", &material)
        );
        // Same bytes, two domains: a reaction key is never a comment key.
        assert_ne!(r, comment_write_seed(&ACCOUNT, &CHANNEL, 7, &LABEL));
        assert_ne!(r, reaction_write_seed(&ACCOUNT, &CHANNEL, 8, &LABEL));
        assert_ne!(r, reaction_write_seed(&ACCOUNT, &CHANNEL, 7, &[2; 16]));
        assert_ne!(r, reaction_write_seed(&[0x12; 32], &CHANNEL, 7, &LABEL));
        assert_ne!(
            r,
            reaction_write_seed(&ACCOUNT, &[0x0D; CHANNEL_ID_LEN], 7, &LABEL)
        );
        assert_ne!(r, crate::owner::channel_owner_seed(&ACCOUNT, &CHANNEL));
    }

    /// One account's writes under one post have unrelated keys: two
    /// reactions, two comments, a reaction and a comment. Two devices of the
    /// account derive the same key for one write.
    #[test]
    fn every_write_has_its_own_key_and_every_device_the_same_one() {
        let a = ReactionWriteKey::new(&ACCOUNT, &CHANNEL, 7, LABEL);
        assert_eq!(
            a.public_key(),
            ReactionWriteKey::new(&ACCOUNT, &CHANNEL, 7, LABEL).public_key()
        );
        let b = ReactionWriteKey::new(&ACCOUNT, &CHANNEL, 7, [2; 16]);
        let c1 = CommentWriteKey::new(&ACCOUNT, &CHANNEL, 7, &[3; 16]);
        let c2 = CommentWriteKey::new(&ACCOUNT, &CHANNEL, 7, &[4; 16]);
        let keys = [
            a.public_key(),
            b.public_key(),
            c1.public_key(),
            c2.public_key(),
        ];
        for (i, x) in keys.iter().enumerate() {
            for y in &keys[i + 1..] {
                assert_ne!(x, y);
            }
        }
        assert!(format!("{a:?}").starts_with("ReactionWriteKey"));
        assert!(format!("{c1:?}").starts_with("CommentWriteKey"));
        assert_ne!(new_comment_nonce(), new_comment_nonce());
    }

    #[test]
    fn every_write_round_trips_through_the_header_and_verifies() {
        let r = ReactionWriteKey::new(&ACCOUNT, &CHANNEL, 7, LABEL);
        let c = CommentWriteKey::new(&ACCOUNT, &CHANNEL, 7, &[3; 16]);
        let rk = r.public_key();
        let ck = c.public_key();
        let sealed = [0xAB; 40];
        let cases = [
            (
                r.sign_add(NOW, [9; 16]),
                statement_for(&rk, 7, SubscriberWrite::AddReaction { label: LABEL }),
            ),
            (
                r.sign_delete(NOW, [9; 16]),
                statement_for(&rk, 7, SubscriberWrite::DeleteReaction { label: LABEL }),
            ),
            (
                c.sign_add(&sealed, NOW, [9; 16]),
                statement_for(&ck, 7, SubscriberWrite::AddComment { sealed: &sealed }),
            ),
            (
                c.sign_delete(3, NOW, [9; 16]),
                statement_for(&ck, 7, SubscriberWrite::DeleteComment { comment_cursor: 3 }),
            ),
        ];
        for (proof, statement) in cases {
            let header = proof.encode();
            let parsed = OwnerProof::parse(&header).unwrap();
            assert_eq!(parsed, proof);
            assert_eq!(fuzz_parse_proof(&header), Some(true));
            assert_eq!(verify_write_proof(&parsed, &statement, NOW + 1_000), Ok(()));
        }
    }

    /// A proof binds the op, the post, the channel, the key and the subject.
    #[test]
    fn a_proof_covers_exactly_one_write() {
        let r = ReactionWriteKey::new(&ACCOUNT, &CHANNEL, 7, LABEL);
        let rk = r.public_key();
        let add = SubscriberWrite::AddReaction { label: LABEL };
        let proof = r.sign_add(NOW, [9; 16]);
        let refused = |s: WriteStatement<'_>| {
            assert_eq!(
                verify_write_proof(&proof, &s, NOW),
                Err(OwnerProofError::BadSignature)
            );
        };
        refused(statement_for(
            &rk,
            7,
            SubscriberWrite::DeleteReaction { label: LABEL },
        ));
        refused(statement_for(
            &rk,
            7,
            SubscriberWrite::AddReaction { label: [2; 16] },
        ));
        refused(statement_for(&rk, 8, add));
        let other = ReactionWriteKey::new(&[0x12; 32], &CHANNEL, 7, LABEL).public_key();
        refused(statement_for(&other, 7, add));
        refused(WriteStatement {
            channel_id: &[0x0D; CHANNEL_ID_LEN],
            post_cursor: 7,
            writer_key: &rk,
            write: add,
        });

        let c = CommentWriteKey::new(&ACCOUNT, &CHANNEL, 7, &[3; 16]);
        let ck = c.public_key();
        let del = c.sign_delete(3, NOW, [9; 16]);
        assert_eq!(
            verify_write_proof(
                &del,
                &statement_for(&ck, 7, SubscriberWrite::DeleteComment { comment_cursor: 4 }),
                NOW
            ),
            Err(OwnerProofError::BadSignature)
        );
        let sealed = [0xAB; 40];
        let comment = c.sign_add(&sealed, NOW, [9; 16]);
        let mut other_sealed = sealed;
        other_sealed[0] ^= 1;
        assert_eq!(
            verify_write_proof(
                &comment,
                &statement_for(
                    &ck,
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
        let r = ReactionWriteKey::new(&ACCOUNT, &CHANNEL, 7, LABEL);
        let rk = r.public_key();
        let write = SubscriberWrite::AddReaction { label: LABEL };
        let proof = r.sign_add(NOW, [9; 16]);
        let s = statement_for(&rk, 7, write);
        assert_eq!(
            verify_write_proof(&proof, &s, NOW + WRITE_PROOF_WINDOW_MS),
            Ok(())
        );
        assert_eq!(
            verify_write_proof(&proof, &s, NOW - WRITE_PROOF_WINDOW_MS),
            Ok(())
        );
        assert_eq!(
            verify_write_proof(&proof, &s, NOW + WRITE_PROOF_WINDOW_MS + 1),
            Err(OwnerProofError::OutsideWindow)
        );
        let mut weak = [0u8; 32];
        weak[0] = 1;
        assert_eq!(
            verify_write_proof(&proof, &statement_for(&weak, 7, write), NOW),
            Err(OwnerProofError::BadSignature)
        );
    }

    #[test]
    fn the_canonical_bytes_are_pinned() {
        let public = [0x5B; WRITE_KEY_LEN];
        let s = statement_for(&public, 7, SubscriberWrite::AddReaction { label: LABEL });
        let bytes = write_proof_canonical_bytes(&s, 5, &[9; 16]);
        let mut expected = b"lakko/channel/write-proof/v1\x00\x01".to_vec();
        expected.extend_from_slice(&CHANNEL);
        expected.extend_from_slice(&7u64.to_be_bytes());
        expected.extend_from_slice(&public);
        expected.extend_from_slice(&LABEL);
        expected.extend_from_slice(&5u64.to_be_bytes());
        expected.extend_from_slice(&[9; 16]);
        assert_eq!(bytes, expected);
        assert_ne!(WRITE_PROOF_DOMAIN, crate::owner::OWNER_PROOF_DOMAIN);
    }
}
