// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Error type for `lakko-transparency`.

use thiserror::Error;

/// Errors produced by the key-transparency log and its proof verifiers.
#[derive(Debug, Error)]
pub enum Error {
    /// Tried to query an entry beyond the log's current size.
    #[error("leaf index {index} out of bounds (log size {size})")]
    IndexOutOfBounds {
        /// Requested leaf index.
        index: u64,
        /// Current log size in leaves.
        size: u64,
    },

    /// The leaf at this index is a seal-announcement leaf, not an identity
    /// entry (ADR-0101 section 10).
    #[error("leaf index {index} is not an identity entry")]
    NotAnIdentityLeaf {
        /// Requested leaf index.
        index: u64,
    },

    /// An inclusion proof failed verification.
    #[error("inclusion proof did not verify against the given root")]
    InclusionProofMismatch,

    /// A seal-announcement leaf is not before the reset leaf it must precede
    /// (ADR-0101 section 10).
    #[error("seal announcement leaf is not before the reset leaf")]
    AnnouncementNotBeforeReset,

    /// A consistency proof failed verification.
    #[error("consistency proof did not verify between the two roots")]
    ConsistencyProofMismatch,

    /// Signed root signature failed verification.
    #[error("signed root signature is invalid")]
    SignedRootInvalid,

    /// Signed root size is older than what the client has previously seen
    /// (rollback attempt).
    #[error("signed root rollback: new size {new} < seen size {seen}")]
    SignedRootRollback {
        /// Size of the newly received signed root.
        new: u64,
        /// Size of the previously trusted root.
        seen: u64,
    },

    /// Crypto-layer failure surfaced from `lakko-crypto`.
    #[error(transparent)]
    Crypto(#[from] lakko_crypto::Error),

    /// A conversation link is not 201 bytes of the known version, or links a
    /// key to itself (ADR-0074 section 3.7).
    #[error("conversation link is malformed")]
    ConversationLinkMalformed,

    /// A conversation link names a key that is not one, or one of its two
    /// signatures does not verify (ADR-0074 section 3.7).
    #[error("conversation link signature is invalid")]
    ConversationLinkInvalid,

    /// A conversation link names another key as the account's current key.
    #[error("conversation link is for another key")]
    ConversationLinkKeyMismatch,

    /// A conversation link was asked for from a key to itself.
    #[error("a key cannot link to itself")]
    ConversationLinkSameKey,

    /// A supersession proof was not the expected wire length (G-4).
    #[error("supersession proof has wrong length: {got} bytes")]
    SupersessionBadLength {
        /// The actual length received.
        got: usize,
    },

    /// A supersession proof carried an unsupported version byte (G-4).
    #[error("unsupported supersession proof version: {0}")]
    SupersessionBadVersion(u8),

    /// A supersession proof's Ed25519 signature did not verify against the
    /// old key it names (G-4).
    #[error("supersession proof signature is invalid")]
    SupersessionBadSignature,

    /// A supersession proof's old key did not match the key the caller
    /// already trusts (G-4).
    #[error("supersession proof old key does not match the trusted key")]
    SupersessionOldKeyMismatch,

    /// A supersession proof has expired relative to `now` (G-4).
    #[error("supersession proof has expired")]
    SupersessionExpired,

    /// A supersession proof's validity window is invalid — either
    /// `expires_at < issued_at`, or it exceeds the 30-day maximum (G-4,
    /// invariant #6: a leaked old key cannot indefinitely re-authorise).
    #[error("supersession proof validity window is invalid")]
    SupersessionInvalidValidityWindow,

    /// A rotation log entry is for a different account than the one being
    /// verified (its `username_hash` does not match) (G-4).
    #[error("supersession log entry is for a different account")]
    SupersessionEntryUsernameMismatch,

    /// A rotation log entry's version is not strictly newer than the
    /// version the peer already trusts (a downgrade / replay) (G-4).
    #[error("supersession log entry version {entry} is not newer than pinned {pinned}")]
    SupersessionEntryVersionNotNewer {
        /// The version on the presented entry.
        entry: u64,
        /// The version the peer currently pins.
        pinned: u64,
    },

    /// A supersession proof authorises a different key than the one in the
    /// log entry it accompanies (G-4).
    #[error("supersession proof authorises a different key than the log entry")]
    SupersessionEntryKeyMismatch,

    /// A genesis leaf's embedded identity pubkey is not a valid Ed25519 key
    /// (G-30 #2, ADR-0019).
    #[error("leaf genesis: identity pubkey is not a valid Ed25519 key")]
    LeafGenesisBadKey,

    /// A genesis leaf self-signature was not the expected 64-byte length
    /// (G-30 #2, ADR-0019).
    #[error("leaf genesis: signature has wrong length")]
    LeafGenesisBadLength,

    /// A genesis leaf self-signature did not verify against the key embedded in
    /// the leaf (G-30 #2, ADR-0019).
    #[error("leaf genesis self-signature is invalid")]
    LeafGenesisInvalid,

    /// A witness co-signature was not the expected wire length (ADR-0020).
    #[error("witness co-signature has wrong length: {got} bytes")]
    WitnessCosigBadLength {
        /// The actual length received.
        got: usize,
    },

    /// A witness co-signature carried an unsupported version byte (ADR-0020).
    #[error("unsupported witness co-signature version: {0}")]
    WitnessCosigBadVersion(u8),

    /// A witness co-signature's Ed25519 signature did not verify against the
    /// pinned witness key (ADR-0020).
    #[error("witness co-signature signature is invalid")]
    WitnessCosigInvalid,

    /// A witness co-signature's embedded `(tree_root, log_size)` did not match
    /// the root the caller is verifying — a co-sig spliced from a different
    /// root (ADR-0020).
    #[error("witness co-signature is bound to a different root")]
    WitnessCosigBindingMismatch,

    /// A witness-quorum verification was requested with a zero threshold — a
    /// misconfiguration that would let an empty quorum trivially pass. Rejected
    /// fail-closed before any co-signature is examined (ADR-0039, G-30
    /// finding-1, invariant #4).
    #[error("witness quorum threshold must be non-zero")]
    WitnessQuorumBadThreshold,

    /// Fewer distinct pinned witness keys produced a valid co-signature over the
    /// expected root than the quorum threshold requires (ADR-0039, G-30
    /// finding-1).
    #[error(
        "witness quorum not met: {achieved} distinct valid co-signatures, {required} required"
    )]
    WitnessQuorumNotMet {
        /// The number of distinct pinned witness keys that produced at least one
        /// valid co-signature over the expected root.
        achieved: usize,
        /// The quorum threshold the caller required.
        required: usize,
    },
}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, Error>;
