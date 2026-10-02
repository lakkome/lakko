// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! The conversation vault ([ADR-0064]) and the blob delete key ([ADR-0065]).
//!
//! A vault is an object on the storage service, one per conversation plus two
//! per account (the index and the devices vault), that no request ties to an
//! account. It holds what a device needs to show a conversation it was away
//! from - the people, the pins, the messages - and what it needs to enter
//! the group again by itself. Everything in it is sealed under a key derived
//! from the account identity seed, so the server stores ciphertext of a few
//! padded sizes and a public key per vault, and nothing else.
//!
//! This crate is the part the three clients and the storage service must not
//! write more than once:
//!
//! - [`keys`] - the derivations: the vault root, a vault's id, key and write
//!   seed, and a blob's delete seed.
//! - [`seal`] - a vault object: padding to a size bucket, ChaCha20-Poly1305,
//!   and associated data that binds the vault, the kind, the chunk number
//!   and the version.
//! - [`proof`] - the request signature of the storage routes, signed with a
//!   vault's write key or a blob's delete key.
//! - [`record`] - the saved records and their bounded, canonical codec.
//! - [`merge`] - how two devices' copies of a conversation's messages become
//!   one.
//!
//! It is pure: no clock, no random source, no network and no store. The
//! caller passes the time to a proof and the nonce to a seal, so the crate is
//! the same on a server, in a browser and in an app extension. It holds no
//! state.
//!
//! No MLS type appears here. Re-entry material is carried as opaque bytes
//! (`group_info`), produced and consumed by `lakko-mls` on the other side of
//! the FFI.
//!
//! [ADR-0064]: ../../../docs/adr/0064-conversation-backup-and-device-re-entry.md
//! [ADR-0065]: ../../../docs/adr/0065-attachment-lifetime-and-access.md

pub mod keys;
pub mod merge;
pub mod proof;
pub mod record;
pub mod seal;

#[doc(hidden)]
pub mod fuzz_api;

pub use lakko_crypto::SecretBytes;

pub use keys::{ConversationRef, RefKind, VaultRef, VaultRefError, VaultRoot};
pub use merge::merge_messages;
pub use proof::{ProofError, StorageProof};
pub use record::{
    ChunkEntry, ConversationHead, ConversationKind, DevicesHead, Direction, HistoryChunk,
    IndexEntry, Membership, PersonalPin, RecordError, ReentryEpoch, RegistrarSeed, Role, SharedPin,
    VaultAttachment, VaultCall, VaultForward, VaultForwardOrigin, VaultIndex, VaultMessage,
    VaultPerson, VaultReaction, VaultReply, VaultThumb,
};
pub use seal::{open_object, seal_object, ObjectKind, SealError};

/// Lowercase hex, for the unit tests' pinned values.
#[cfg(test)]
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}
