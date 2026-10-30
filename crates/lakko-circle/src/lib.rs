// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Circle directories ([ADR-0086]): finding a person through someone you
//! already know, with no phone woken and no list of who knows whom on a server.
//!
//! A person keeps one small directory on the Delivery Service. Their contacts
//! hold a filter of it and read one row on a hit. Removing a contact changes a
//! read key: nothing stored moves, and nobody else does any work.
//!
//! | Module | What it is |
//! |---|---|
//! | [`keys`] | every derivation, and the wrap |
//! | [`tree`] | the one-way key tree; [`tree::TreeOwner`] the owner's side, [`tree::member_epoch_key`] a contact's |
//! | [`epoch`] | [`EpochKey`], and the chain that reaches older epochs |
//! | [`filter`] | the static retrieval filter from a term tag to its rows |
//! | [`row`] | sealed rows |
//! | [`head`] | the public head, and the grant a contact holds |
//! | [`writer`] | [`DirectoryWriter`], which turns changes into one atomic write |
//!
//! The crate does no I/O. Clocks and sockets are the caller's; randomness comes
//! from `lakko_crypto::random`. Every decoder is bounded and fail-closed, and
//! has a fuzz target (`fuzz/`).
//!
//! ## Sizes, measured by the tests of this crate
//!
//! | | 150 contacts | 10,000 contacts |
//! |---|---|---|
//! | Capacity bucket | 256 leaves | 16,384 leaves |
//! | A removal, payload | 662 bytes | 1,046 bytes |
//! | A removal, JSON body | 1,058 bytes | 1,606 bytes |
//! | A contact's path read, payload | 570 bytes | 954 bytes |
//! | Filter, 9 terms per person | 5,118 bytes | 327,678 bytes |
//! | Filter, bytes per term | 3.79 | 3.64 |
//! | False positives per filter | 1.3e-5 | 6.0e-4 |
//! | Edges stored | 15,300 bytes | 982,980 bytes |
//! | One row | 865 bytes | 865 bytes |
//!
//! [ADR-0086]: ../../../docs/adr/0086-circle-directories.md

#![forbid(unsafe_code)]

mod codec;

pub mod epoch;
pub mod error;
pub mod filter;
pub mod head;
pub mod keys;
pub mod row;
pub mod tree;
pub mod writer;

#[doc(hidden)]
pub mod fuzz_api;

pub use lakko_crypto::SecretBytes;

pub use epoch::EpochKey;
pub use error::CircleError;
pub use filter::{
    filter_header, open_filter, seal_filter, FilterEntry, FilterHeader, FilterStats, OpenedFilter,
    MAX_FILTER_BYTES, MAX_ROWS, MAX_ROWS_PER_TERM, TAG_LEN_BYTES,
};
pub use head::{DirectoryHead, MemberGrant, GRANT_LEN, HEAD_LEN};
pub use keys::{
    directory_id, directory_owner_matches, owner_public_key, owner_seed, owner_signing_seed,
    write_digest, DIRECTORY_ID_LEN, KEY_LEN, SALT_LEN, WRAP_LEN,
};
pub use row::{open_row, row_epoch, seal_row, ROW_CARD_MAX_LEN, ROW_LEN};
pub use tree::{
    member_epoch_key, path_nodes, TreeOwner, TreeUpdate, Wrap, MAX_CAPACITY_LOG, MAX_EPOCH,
    MAX_LEAVES, MAX_NODE, MIN_CAPACITY_LOG, TREE_HEIGHT,
};
pub use writer::{
    directory_path, DirectoryWrite, DirectoryWriter, MAX_ROWS_PER_WRITE, OWNER_PROOF_EPOCH,
};
