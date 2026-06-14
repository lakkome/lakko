// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Append-only key-transparency log with signed roots.
//!
//! Each entry binds `(username_hash, identity_pubkey, version)` and is
//! appended to a Merkle tree. The log operator periodically signs the
//! current root; clients verify both:
//!
//! - **Inclusion:** my entry `e` is at index `i` of a tree whose root
//!   matches a signed root I trust.
//! - **Append-only:** the new signed root never has a smaller log size
//!   than the previously trusted one (rollback detection).
//!
//! Phase 0.5 ships an in-memory implementation; production storage
//! (append-only file / object store) lands when the discovery service
//! integrates this in Phase 2.

use lakko_crypto::signing;

use crate::error::{Error, Result};
use crate::merkle::{self, TreeHash};

/// One entry in the key-transparency log.
///
/// Bindings are immutable once appended; identity rotation appends a **new**
/// entry with an incremented version number rather than mutating an old one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    /// BLAKE3 hash of the canonical username form.
    pub username_hash: [u8; 32],
    /// Identity public key (Ed25519 or X.509-cert-bound public key).
    pub identity_pubkey: Vec<u8>,
    /// Monotonic version number for this username; ≥1, incremented per
    /// rotation.
    pub version: u64,
}

impl LogEntry {
    /// Canonical byte serialization used for Merkle leaf hashing.
    ///
    /// Format: `username_hash (32) || version (8 LE) || pubkey_len (4 LE) || pubkey`.
    /// Stable across versions; never re-purpose the layout — append-only.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + 8 + 4 + self.identity_pubkey.len());
        out.extend_from_slice(&self.username_hash);
        out.extend_from_slice(&self.version.to_le_bytes());
        let pk_len = u32::try_from(self.identity_pubkey.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&pk_len.to_le_bytes());
        out.extend_from_slice(&self.identity_pubkey);
        out
    }
}

/// Signed Merkle-tree root produced by the log operator.
///
/// Clients pin the operator's [`signing::VerifyingKey`] out-of-band and
/// verify every received root against it. Roots are append-only by
/// `log_size`; receiving a smaller `log_size` than the last-trusted one
/// is a rollback attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedRoot {
    /// Merkle tree root over the first `log_size` entries.
    pub tree_root: TreeHash,
    /// Number of entries the root commits to.
    pub log_size: u64,
    /// Operator-chosen monotonic timestamp (seconds since UNIX epoch).
    pub timestamp_unix: u64,
    /// Ed25519 signature over `(tree_root || log_size || timestamp_unix)`.
    pub signature: signing::Signature,
}

impl SignedRoot {
    /// Bytes signed by the operator (canonical pre-image).
    #[must_use]
    pub fn signing_payload(tree_root: &TreeHash, log_size: u64, timestamp_unix: u64) -> Vec<u8> {
        let mut buf = Vec::with_capacity(32 + 8 + 8);
        buf.extend_from_slice(tree_root);
        buf.extend_from_slice(&log_size.to_le_bytes());
        buf.extend_from_slice(&timestamp_unix.to_le_bytes());
        buf
    }

    /// Verify the root's signature against `operator`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SignedRootInvalid`] if the signature does not verify.
    pub fn verify(&self, operator: &signing::VerifyingKey) -> Result<()> {
        let payload = Self::signing_payload(&self.tree_root, self.log_size, self.timestamp_unix);
        operator
            .verify(&payload, &self.signature)
            .map_err(|_| Error::SignedRootInvalid)
    }
}

/// Inclusion proof for a single entry against a specific signed root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InclusionProof {
    /// Leaf index (0-based) of the proven entry.
    pub leaf_index: u64,
    /// Audit path: sibling node-hashes from the leaf up to the root.
    pub audit_path: Vec<TreeHash>,
}

/// In-memory append-only log with a single operator signing key.
///
/// `O(N)` proof generation — fine for tests and Phase-0.5-scale skeletons,
/// re-implement with a persistent storage layer (append-only segment file
/// or RocksDB) when wiring into `services/discovery` in Phase 2.
pub struct InMemoryLog {
    entries: Vec<LogEntry>,
    /// Pre-computed canonical bytes per entry, used as Merkle leaves.
    leaf_bytes: Vec<Vec<u8>>,
    operator: signing::SigningKey,
    operator_pub: signing::VerifyingKey,
}

impl InMemoryLog {
    /// Create a fresh log signed by `operator`.
    #[must_use]
    pub fn new(operator: signing::SigningKey) -> Self {
        let operator_pub = operator.verifying_key();
        Self {
            entries: Vec::new(),
            leaf_bytes: Vec::new(),
            operator,
            operator_pub,
        }
    }

    /// Operator's verifying key — clients pin this out-of-band.
    #[must_use]
    pub fn operator_key(&self) -> signing::VerifyingKey {
        self.operator_pub
    }

    /// Number of entries currently in the log.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.entries.len() as u64
    }

    /// Append `entry` to the end of the log. Returns the leaf index.
    pub fn append(&mut self, entry: LogEntry) -> u64 {
        let bytes = entry.canonical_bytes();
        self.entries.push(entry);
        self.leaf_bytes.push(bytes);
        (self.entries.len() - 1) as u64
    }

    /// Read a previously appended entry.
    ///
    /// # Errors
    ///
    /// Returns [`Error::IndexOutOfBounds`] if `index >= size()`.
    pub fn entry(&self, index: u64) -> Result<&LogEntry> {
        let i = usize::try_from(index).map_err(|_| Error::IndexOutOfBounds {
            index,
            size: self.size(),
        })?;
        self.entries.get(i).ok_or(Error::IndexOutOfBounds {
            index,
            size: self.size(),
        })
    }

    /// Current Merkle tree root.
    #[must_use]
    pub fn current_root(&self) -> TreeHash {
        merkle::root(&self.leaf_bytes)
    }

    /// Produce a signed root for the current log state.
    #[must_use]
    pub fn sign_current_root(&self, timestamp_unix: u64) -> SignedRoot {
        let tree_root = self.current_root();
        let log_size = self.size();
        let payload = SignedRoot::signing_payload(&tree_root, log_size, timestamp_unix);
        let signature = self.operator.sign(&payload);
        SignedRoot {
            tree_root,
            log_size,
            timestamp_unix,
            signature,
        }
    }

    /// Build an inclusion proof for the entry at `index` against the
    /// current log state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::IndexOutOfBounds`] if `index >= size()`.
    pub fn prove_inclusion(&self, index: u64) -> Result<InclusionProof> {
        let i = usize::try_from(index).map_err(|_| Error::IndexOutOfBounds {
            index,
            size: self.size(),
        })?;
        if i >= self.entries.len() {
            return Err(Error::IndexOutOfBounds {
                index,
                size: self.size(),
            });
        }
        let audit_path = merkle::audit_path(&self.leaf_bytes, i);
        Ok(InclusionProof {
            leaf_index: index,
            audit_path,
        })
    }

    /// Build an RFC 6962 §2.1.2 consistency proof from `old_size` to the
    /// current log size — proving the current tree append-only-extends the tree
    /// of the first `old_size` leaves (ADR-0020). The operator hands this to the
    /// witness, which verifies it before co-signing the new root. An `old_size`
    /// of 0 or `>=` the current size yields an empty proof (the matching
    /// [`verify_consistency`](crate::verify_consistency) handles those).
    #[must_use]
    pub fn consistency_proof(&self, old_size: u64) -> Vec<TreeHash> {
        let old = usize::try_from(old_size).unwrap_or(usize::MAX);
        merkle::consistency_proof(old, &self.leaf_bytes)
    }
}

/// Verify an inclusion proof for `entry` against a `signed_root` previously
/// validated against the operator's key.
///
/// This is the **client-side** entry point: callers should first call
/// [`SignedRoot::verify`] to ensure the root itself is authentic.
///
/// # Errors
///
/// Returns [`Error::InclusionProofMismatch`] if the proof does not bind
/// `entry` at `proof.leaf_index` to `signed_root.tree_root`.
pub fn verify_inclusion(
    entry: &LogEntry,
    proof: &InclusionProof,
    signed_root: &SignedRoot,
) -> Result<()> {
    let leaf_bytes = entry.canonical_bytes();
    if merkle::verify_inclusion(
        &leaf_bytes,
        proof.leaf_index,
        signed_root.log_size,
        &proof.audit_path,
        &signed_root.tree_root,
    ) {
        Ok(())
    } else {
        Err(Error::InclusionProofMismatch)
    }
}

/// Detect a rollback between a previously trusted root and a newly
/// received one.
///
/// # Errors
///
/// Returns [`Error::SignedRootRollback`] if `new.log_size < seen.log_size`.
pub fn check_no_rollback(seen: &SignedRoot, new: &SignedRoot) -> Result<()> {
    if new.log_size < seen.log_size {
        Err(Error::SignedRootRollback {
            new: new.log_size,
            seen: seen.log_size,
        })
    } else {
        Ok(())
    }
}

/// Verify that `new` append-only-extends `old` via an RFC 6962 §2.1.2
/// consistency proof (ADR-0020). This is the witness's gate: it co-signs a new
/// root only when this passes against its last-co-signed root, so the operator
/// cannot get a co-signature on a forked history.
///
/// This binds only the `(tree_root, log_size)` of each root — the caller is
/// responsible for having verified each root's operator signature first.
///
/// # Errors
///
/// Returns [`Error::ConsistencyProofMismatch`] if the proof does not prove
/// `new.tree_root` is the append-only successor of `old.tree_root`.
pub fn verify_consistency(old: &SignedRoot, new: &SignedRoot, proof: &[TreeHash]) -> Result<()> {
    if merkle::verify_consistency(
        old.log_size,
        &old.tree_root,
        new.log_size,
        &new.tree_root,
        proof,
    ) {
        Ok(())
    } else {
        Err(Error::ConsistencyProofMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, version: u64) -> LogEntry {
        LogEntry {
            username_hash: lakko_crypto::hash::blake3(name.as_bytes()),
            identity_pubkey: vec![0xCC; 32],
            version,
        }
    }

    #[test]
    fn empty_log_size_zero_root_zero() {
        let log = InMemoryLog::new(signing::SigningKey::generate());
        assert_eq!(log.size(), 0);
        assert_eq!(log.current_root(), [0u8; 32]);
    }

    #[test]
    fn append_returns_monotonic_index() {
        let mut log = InMemoryLog::new(signing::SigningKey::generate());
        assert_eq!(log.append(entry("alice", 1)), 0);
        assert_eq!(log.append(entry("bob", 1)), 1);
        assert_eq!(log.append(entry("alice", 2)), 2);
        assert_eq!(log.size(), 3);
    }

    #[test]
    fn signed_root_verifies_against_operator_key() {
        let mut log = InMemoryLog::new(signing::SigningKey::generate());
        log.append(entry("alice", 1));
        let signed = log.sign_current_root(1_700_000_000);
        signed.verify(&log.operator_key()).expect("good signature");
    }

    #[test]
    fn signed_root_with_wrong_key_fails() {
        let mut log = InMemoryLog::new(signing::SigningKey::generate());
        log.append(entry("alice", 1));
        let signed = log.sign_current_root(1_700_000_000);
        let other = signing::SigningKey::generate().verifying_key();
        let result = signed.verify(&other);
        assert!(matches!(result, Err(Error::SignedRootInvalid)));
    }

    #[test]
    fn inclusion_proof_roundtrip() {
        let mut log = InMemoryLog::new(signing::SigningKey::generate());
        for i in 0..10 {
            log.append(entry(&format!("user-{i}"), 1));
        }
        let signed = log.sign_current_root(1_700_000_000);
        for i in 0..10 {
            let proof = log.prove_inclusion(i).expect("proof");
            let stored_entry = log.entry(i).expect("entry");
            verify_inclusion(stored_entry, &proof, &signed).expect("proof verifies");
        }
    }

    #[test]
    fn inclusion_proof_rejects_wrong_entry() {
        let mut log = InMemoryLog::new(signing::SigningKey::generate());
        log.append(entry("alice", 1));
        log.append(entry("bob", 1));
        let signed = log.sign_current_root(1_700_000_000);
        let proof = log.prove_inclusion(0).unwrap();
        // Try verifying with bob's entry against alice's proof — should fail.
        let bob = log.entry(1).unwrap();
        let result = verify_inclusion(bob, &proof, &signed);
        assert!(matches!(result, Err(Error::InclusionProofMismatch)));
    }

    #[test]
    fn out_of_bounds_query_errors() {
        let log = InMemoryLog::new(signing::SigningKey::generate());
        assert!(matches!(
            log.entry(0),
            Err(Error::IndexOutOfBounds { index: 0, size: 0 })
        ));
        assert!(matches!(
            log.prove_inclusion(7),
            Err(Error::IndexOutOfBounds { index: 7, size: 0 })
        ));
    }

    #[test]
    fn rollback_detection_works() {
        let mut log = InMemoryLog::new(signing::SigningKey::generate());
        log.append(entry("alice", 1));
        let r1 = log.sign_current_root(1_700_000_000);
        log.append(entry("bob", 1));
        let r2 = log.sign_current_root(1_700_000_001);

        // Forward direction: r1 → r2 is fine.
        check_no_rollback(&r1, &r2).expect("forward ok");

        // Rollback: r2 → r1 is a rollback.
        let result = check_no_rollback(&r2, &r1);
        assert!(matches!(
            result,
            Err(Error::SignedRootRollback { new: 1, seen: 2 })
        ));
    }

    #[test]
    fn identity_rotation_appends_higher_version() {
        let mut log = InMemoryLog::new(signing::SigningKey::generate());
        let alice_v1 = entry("alice", 1);
        let alice_v2 = entry("alice", 2);
        let i1 = log.append(alice_v1.clone());
        let i2 = log.append(alice_v2.clone());

        let signed = log.sign_current_root(1_700_000_000);
        let p1 = log.prove_inclusion(i1).unwrap();
        let p2 = log.prove_inclusion(i2).unwrap();

        verify_inclusion(&alice_v1, &p1, &signed).expect("v1 included");
        verify_inclusion(&alice_v2, &p2, &signed).expect("v2 included");
        // Both entries have the same username_hash but different versions.
        assert_eq!(alice_v1.username_hash, alice_v2.username_hash);
        assert_ne!(alice_v1.version, alice_v2.version);
    }

    #[test]
    fn consistency_proof_extends_old_root() {
        // The operator's consistency proof from an old size to the current size
        // verifies the current root append-only-extends the old root (ADR-0020).
        let mut log = InMemoryLog::new(signing::SigningKey::generate());
        for i in 0..4 {
            log.append(entry(&format!("u{i}"), 1));
        }
        let old_root = log.current_root();
        let old_size = log.size();
        for i in 4..10 {
            log.append(entry(&format!("u{i}"), 1));
        }
        let new_root = log.current_root();
        let new_size = log.size();

        let proof = log.consistency_proof(old_size);
        assert!(crate::merkle::verify_consistency(
            old_size, &old_root, new_size, &new_root, &proof
        ));
        // And the SignedRoot-level wrapper agrees.
        let old = SignedRoot {
            tree_root: old_root,
            log_size: old_size,
            timestamp_unix: 1,
            signature: lakko_crypto::signing::Signature::from_bytes(&[0u8; 64]).unwrap(),
        };
        let new = SignedRoot {
            tree_root: new_root,
            log_size: new_size,
            timestamp_unix: 2,
            signature: lakko_crypto::signing::Signature::from_bytes(&[0u8; 64]).unwrap(),
        };
        crate::verify_root_consistency(&old, &new, &proof).expect("root consistency");
    }
}
