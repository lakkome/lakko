// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Key-transparency log skeleton for Lakko.
//!
//! Implements a CONIKS-flavored append-only Merkle log with:
//!
//! - [`merkle`] — RFC 6962-style binary Merkle tree with BLAKE3 and
//!   leaf/node domain separation
//! - [`log::LogEntry`] — `(username_hash, identity_pubkey, version)` binding
//! - [`log::SignedRoot`] — Ed25519-signed root the operator publishes
//! - [`log::InclusionProof`] — audit-path proof for a single entry
//! - [`log::InMemoryLog`] — in-memory append-only log for tests / Phase-0.5
//!   skeleton; production storage lands in Phase 2 when wired into
//!   `services/discovery`
//!
//! See `docs/adr/0001-mls-only.md` ("Identity") for how identities bind to
//! KT-log entries, and `docs/ROADMAP.md` Phase 2 for the rotation flow
//! (supersession proof + 30-day bridge for old invite links).

#![forbid(unsafe_code)]

pub mod account;
pub mod error;
pub mod leaf_auth;
pub mod log;
pub mod merkle;
pub mod supersession;
pub mod witness;

pub use account::{account_username_hash, ACCOUNT_KEY_DOMAIN};
pub use error::{Error, Result};
pub use leaf_auth::{sign_leaf_genesis, verify_leaf_genesis};
pub use log::{
    check_no_rollback, verify_consistency as verify_root_consistency, verify_inclusion,
    InMemoryLog, InclusionProof, LogEntry, SignedRoot,
};
pub use merkle::{consistency_proof, verify_consistency, TreeHash};
pub use supersession::{
    verify_supersession, verify_supersession_chain, PinnedIdentity, RotatedIdentity,
    RotationEvidence, SupersessionProof, MAX_VALIDITY_SECS, SUPERSESSION_PROOF_LEN,
};
pub use witness::{sign_witness_cosig, verify_witness_cosig, WITNESS_COSIG_LEN};
