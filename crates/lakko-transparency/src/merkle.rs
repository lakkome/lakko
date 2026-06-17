// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! RFC 6962-style Merkle tree (binary, BLAKE3-keyed domain separation).
//!
//! Leaves are hashed with the **leaf prefix** byte `0x00`; internal nodes
//! are hashed with the **node prefix** byte `0x01`. This matches Certificate
//! Transparency (RFC 6962) and CONIKS, and prevents second-preimage attacks
//! where a node hash could be passed off as a leaf hash or vice versa.
//!
//! For odd-numbered tree levels, the final unpaired node is **lifted**
//! unchanged to the next level (RFC 6962 §2.1, "MerkleTreeHash"). This
//! avoids leaf duplication attacks.

use lakko_crypto::hash;

/// 32-byte Merkle tree node hash.
pub type TreeHash = [u8; 32];

/// Hash a leaf entry (raw bytes) into a tree-hash with the leaf-prefix byte.
#[must_use]
pub fn hash_leaf(data: &[u8]) -> TreeHash {
    let mut input = Vec::with_capacity(data.len() + 1);
    input.push(0x00);
    input.extend_from_slice(data);
    hash::blake3(&input)
}

/// Hash two child node-hashes into a parent node-hash with the node-prefix byte.
#[must_use]
pub fn hash_node(left: &TreeHash, right: &TreeHash) -> TreeHash {
    let mut input = [0u8; 1 + 32 + 32];
    input[0] = 0x01;
    input[1..33].copy_from_slice(left);
    input[33..65].copy_from_slice(right);
    hash::blake3(&input)
}

/// Compute the RFC 6962 Merkle tree root for a slice of leaf entries.
///
/// An empty input produces the all-zero hash (sentinel for empty tree).
#[must_use]
pub fn root(leaves: &[Vec<u8>]) -> TreeHash {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    let leaf_hashes: Vec<TreeHash> = leaves.iter().map(|l| hash_leaf(l)).collect();
    root_from_hashes(&leaf_hashes)
}

/// Same as [`root`] but takes pre-hashed leaves directly.
#[must_use]
pub fn root_from_hashes(hashes: &[TreeHash]) -> TreeHash {
    if hashes.is_empty() {
        return [0u8; 32];
    }
    if hashes.len() == 1 {
        return hashes[0];
    }
    // RFC 6962: split at largest power-of-two strictly less than n.
    let split = largest_pow2_strictly_less(hashes.len());
    let left = root_from_hashes(&hashes[..split]);
    let right = root_from_hashes(&hashes[split..]);
    hash_node(&left, &right)
}

/// Generate an inclusion-proof audit path for the leaf at `index`.
///
/// Returns the sequence of sibling node-hashes the verifier needs to walk
/// from the leaf up to the root. The resulting `Vec` contains
/// `ceil(log2(n))` hashes for a tree of `n` leaves.
///
/// # Panics
///
/// Panics in debug builds if `index >= leaves.len()`. Callers should range-
/// check before invoking.
#[must_use]
pub fn audit_path(leaves: &[Vec<u8>], index: usize) -> Vec<TreeHash> {
    debug_assert!(index < leaves.len(), "index out of range");
    let leaf_hashes: Vec<TreeHash> = leaves.iter().map(|l| hash_leaf(l)).collect();
    let mut path = Vec::new();
    audit_path_inner(&leaf_hashes, index, &mut path);
    path
}

fn audit_path_inner(hashes: &[TreeHash], index: usize, out: &mut Vec<TreeHash>) {
    if hashes.len() <= 1 {
        return;
    }
    let split = largest_pow2_strictly_less(hashes.len());
    if index < split {
        // We are in the left subtree; sibling is the right subtree's root.
        let right_root = root_from_hashes(&hashes[split..]);
        audit_path_inner(&hashes[..split], index, out);
        out.push(right_root);
    } else {
        // We are in the right subtree; sibling is the left subtree's root.
        let left_root = root_from_hashes(&hashes[..split]);
        audit_path_inner(&hashes[split..], index - split, out);
        out.push(left_root);
    }
}

/// Verify that `leaf` is the entry at `index` in a tree of size `tree_size`
/// whose root is `expected_root`, given the audit `path`.
///
/// The audit path is in **inner-first** order (matches [`audit_path`]):
/// `path[0]` is the sibling at the leaf-most subtree, `path[last]` is the
/// sibling at the top level.
#[must_use]
pub fn verify_inclusion(
    leaf: &[u8],
    index: u64,
    tree_size: u64,
    path: &[TreeHash],
    expected_root: &TreeHash,
) -> bool {
    if index >= tree_size || tree_size == 0 {
        return false;
    }

    // First, walk down from root to leaf and record at each level whether
    // we descend into the left subtree (`true`) or the right subtree (`false`).
    let mut descent: Vec<bool> = Vec::new();
    let mut idx = index;
    let mut size = tree_size;
    while size > 1 {
        let split = largest_pow2_strictly_less_u64(size);
        if idx < split {
            descent.push(true);
            size = split;
        } else {
            descent.push(false);
            idx -= split;
            size -= split;
        }
    }

    if descent.len() != path.len() {
        return false; // path length must match tree depth at this position
    }

    // Walk back up from the leaf, applying inner siblings first.
    // descent.last() is the leaf-most level → matches path[0].
    let mut current = hash_leaf(leaf);
    for (is_left, sibling) in descent.iter().rev().zip(path.iter()) {
        current = if *is_left {
            hash_node(&current, sibling)
        } else {
            hash_node(sibling, &current)
        };
    }

    &current == expected_root
}

/// Generate an RFC 6962 §2.1.2 **consistency proof** that the tree of the
/// first `old_size` leaves is an append-only prefix of the tree of all
/// `leaves` (size `new_size = leaves.len()`).
///
/// The proof lets a verifier holding only `old_root` + `new_root` confirm the
/// new tree extends the old without re-hashing the whole log. Returns an empty
/// proof for the degenerate `old_size == 0` (empty is a prefix of anything) and
/// `old_size >= new_size` (no extension) cases; the matching
/// [`verify_consistency`] handles those.
#[must_use]
pub fn consistency_proof(old_size: usize, leaves: &[Vec<u8>]) -> Vec<TreeHash> {
    let n = leaves.len();
    if old_size == 0 || old_size >= n {
        return Vec::new();
    }
    let hashes: Vec<TreeHash> = leaves.iter().map(|l| hash_leaf(l)).collect();
    subproof(old_size, &hashes, true)
}

/// RFC 6962 §2.1.2 `SUBPROOF(m, D, b)`. `b` records whether this subtree is
/// entirely the old tree's right edge (so the old root is reconstructed from
/// the path, no node emitted) or a left subtree the verifier must be handed.
fn subproof(m: usize, hashes: &[TreeHash], b: bool) -> Vec<TreeHash> {
    let n = hashes.len();
    if m == n {
        return if b {
            Vec::new()
        } else {
            vec![root_from_hashes(hashes)]
        };
    }
    let k = largest_pow2_strictly_less(n);
    if m <= k {
        let mut proof = subproof(m, &hashes[..k], b);
        proof.push(root_from_hashes(&hashes[k..]));
        proof
    } else {
        let mut proof = subproof(m - k, &hashes[k..], false);
        proof.push(root_from_hashes(&hashes[..k]));
        proof
    }
}

/// Verify an RFC 6962 §2.1.2 consistency proof: that `old_root` (the root over
/// the first `old_size` leaves) is an append-only prefix of `new_root` (the
/// root over `new_size` leaves). Reconstructs BOTH roots from the proof and
/// requires both to match — so a fork (a different `new_root` at the same size,
/// or a history that does not extend `old_root`) is rejected.
#[must_use]
pub fn verify_consistency(
    old_size: u64,
    old_root: &TreeHash,
    new_size: u64,
    new_root: &TreeHash,
    proof: &[TreeHash],
) -> bool {
    if old_size > new_size {
        return false;
    }
    if old_size == new_size {
        return proof.is_empty() && old_root == new_root;
    }
    if old_size == 0 {
        // The empty tree is a prefix of any tree; the claim carries no nodes
        // (we cannot bind it to a specific `old_root` here).
        return proof.is_empty();
    }

    // Iterative reconstruction. `node` walks the old tree's rightmost leaf up;
    // `last` the new tree's rightmost leaf up.
    let mut node = old_size - 1;
    let mut last = new_size - 1;
    while node & 1 == 1 {
        node >>= 1;
        last >>= 1;
    }

    let mut it = proof.iter();
    // Seed: when the old subtree is itself a complete subtree (node == 0 after
    // the climb) its root IS `old_root`; otherwise the first proof node is the
    // shared right-edge hash.
    let (mut h_old, mut h_new) = if node > 0 {
        match it.next() {
            Some(seed) => (*seed, *seed),
            None => return false,
        }
    } else {
        (*old_root, *old_root)
    };

    while node > 0 {
        if node & 1 == 1 {
            // Right child: a left sibling joins BOTH reconstructions.
            let Some(p) = it.next() else {
                return false;
            };
            h_old = hash_node(p, &h_old);
            h_new = hash_node(p, &h_new);
        } else if node < last {
            // Left child that has a right sibling only in the bigger tree.
            let Some(p) = it.next() else {
                return false;
            };
            h_new = hash_node(&h_new, p);
        }
        // else: left child at the tree edge — lifted, no sibling.
        node >>= 1;
        last >>= 1;
    }

    // Remaining proof nodes extend the new reconstruction up to `new_root`.
    while last > 0 {
        let Some(p) = it.next() else {
            return false;
        };
        h_new = hash_node(&h_new, p);
        last >>= 1;
    }

    it.next().is_none() && &h_old == old_root && &h_new == new_root
}

/// Largest power of two strictly less than `n` (n >= 2).
fn largest_pow2_strictly_less(n: usize) -> usize {
    debug_assert!(n >= 2);
    let mut k = 1usize;
    while k < n {
        k <<= 1;
    }
    k >> 1
}

fn largest_pow2_strictly_less_u64(n: u64) -> u64 {
    debug_assert!(n >= 2);
    let mut k = 1u64;
    while k < n {
        k <<= 1;
    }
    k >> 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(n: usize) -> Vec<Vec<u8>> {
        (0..n).map(|i| format!("leaf-{i}").into_bytes()).collect()
    }

    #[test]
    fn empty_tree_is_all_zero() {
        assert_eq!(root(&[]), [0u8; 32]);
    }

    #[test]
    fn single_leaf_tree_equals_leaf_hash() {
        let l = vec![b"hello".to_vec()];
        assert_eq!(root(&l), hash_leaf(b"hello"));
    }

    #[test]
    fn two_leaves_combine_into_parent() {
        let l = vec![b"a".to_vec(), b"b".to_vec()];
        let expected = hash_node(&hash_leaf(b"a"), &hash_leaf(b"b"));
        assert_eq!(root(&l), expected);
    }

    #[test]
    fn power_of_two_tree() {
        // 4 leaves → balanced binary tree
        let l = leaves(4);
        let h0 = hash_leaf(&l[0]);
        let h1 = hash_leaf(&l[1]);
        let h2 = hash_leaf(&l[2]);
        let h3 = hash_leaf(&l[3]);
        let h01 = hash_node(&h0, &h1);
        let h23 = hash_node(&h2, &h3);
        let r = hash_node(&h01, &h23);
        assert_eq!(root(&l), r);
    }

    #[test]
    fn odd_tree_lifts_singleton() {
        // 3 leaves → split at 2; left subtree is balanced (2), right is single.
        let l = leaves(3);
        let h0 = hash_leaf(&l[0]);
        let h1 = hash_leaf(&l[1]);
        let h2 = hash_leaf(&l[2]);
        let h01 = hash_node(&h0, &h1);
        let r = hash_node(&h01, &h2);
        assert_eq!(root(&l), r);
    }

    #[test]
    fn inclusion_proof_roundtrip_small() {
        for n in 1..=16 {
            let l = leaves(n);
            let r = root(&l);
            for i in 0..n {
                let path = audit_path(&l, i);
                assert!(
                    verify_inclusion(&l[i], i as u64, n as u64, &path, &r),
                    "inclusion failed for n={n}, i={i}",
                );
            }
        }
    }

    #[test]
    fn inclusion_proof_rejects_wrong_leaf() {
        let l = leaves(8);
        let r = root(&l);
        let path = audit_path(&l, 3);
        assert!(!verify_inclusion(b"not-the-leaf", 3, 8, &path, &r));
    }

    #[test]
    fn inclusion_proof_rejects_wrong_index() {
        let l = leaves(8);
        let r = root(&l);
        let path = audit_path(&l, 3);
        assert!(!verify_inclusion(&l[3], 4, 8, &path, &r));
    }

    #[test]
    fn inclusion_proof_rejects_truncated_path() {
        let l = leaves(8);
        let r = root(&l);
        let mut path = audit_path(&l, 3);
        path.pop();
        assert!(!verify_inclusion(&l[3], 3, 8, &path, &r));
    }

    #[test]
    fn inclusion_proof_rejects_extra_path() {
        let l = leaves(8);
        let r = root(&l);
        let mut path = audit_path(&l, 3);
        path.push([0xAA; 32]);
        assert!(!verify_inclusion(&l[3], 3, 8, &path, &r));
    }

    proptest::proptest! {
        #[test]
        fn proptest_inclusion_arbitrary_tree(
            data in proptest::collection::vec(
                proptest::collection::vec(proptest::num::u8::ANY, 0..40),
                1..32usize,
            ),
        ) {
            let r = root(&data);
            for i in 0..data.len() {
                let path = audit_path(&data, i);
                proptest::prop_assert!(verify_inclusion(
                    &data[i],
                    i as u64,
                    data.len() as u64,
                    &path,
                    &r,
                ));
            }
        }
    }

    // -- RFC 6962 §2.1.2 consistency proofs (ADR-0020) --

    fn other_leaves(n: usize) -> Vec<Vec<u8>> {
        (0..n).map(|i| format!("other-{i}").into_bytes()).collect()
    }

    #[test]
    fn consistency_proof_roundtrip_exhaustive() {
        for n in 2..=40usize {
            let all = leaves(n);
            let new_root = root(&all);
            for m in 1..n {
                let old_root = root(&all[..m]);
                let proof = consistency_proof(m, &all);
                assert!(
                    verify_consistency(m as u64, &old_root, n as u64, &new_root, &proof),
                    "consistency round-trip failed for m={m}, n={n}"
                );
            }
        }
    }

    #[test]
    fn consistency_same_size_requires_equal_roots() {
        let a = leaves(5);
        let ra = root(&a);
        // Same size + same root + empty proof → consistent.
        assert!(verify_consistency(5, &ra, 5, &ra, &[]));
        // Same size + a DIFFERENT root → a fork at the same size, rejected.
        let rb = root(&other_leaves(5));
        assert!(!verify_consistency(5, &ra, 5, &rb, &[]));
    }

    #[test]
    fn consistency_rejects_wrong_new_root() {
        let all = leaves(9);
        let old_root = root(&all[..3]);
        let proof = consistency_proof(3, &all);
        assert!(!verify_consistency(3, &old_root, 9, &[0xAB; 32], &proof));
    }

    #[test]
    fn consistency_rejects_wrong_old_root() {
        let all = leaves(9);
        let new_root = root(&all);
        let proof = consistency_proof(3, &all);
        assert!(!verify_consistency(3, &[0xCD; 32], 9, &new_root, &proof));
    }

    #[test]
    fn consistency_rejects_fork_history() {
        // The old tree is the first 4 of A; tree B shares no history with A.
        let a = leaves(8);
        let old_root = root(&a[..4]);
        let b = other_leaves(8);
        let new_root_b = root(&b);
        // Neither A's nor B's proof can bridge old_root(A[..4]) → root(B).
        assert!(!verify_consistency(
            4,
            &old_root,
            8,
            &new_root_b,
            &consistency_proof(4, &a)
        ));
        assert!(!verify_consistency(
            4,
            &old_root,
            8,
            &new_root_b,
            &consistency_proof(4, &b)
        ));
    }

    #[test]
    fn consistency_rejects_tampered_proof() {
        let all = leaves(12);
        let old_root = root(&all[..5]);
        let new_root = root(&all);
        let mut proof = consistency_proof(5, &all);
        assert!(!proof.is_empty());
        proof[0][0] ^= 0x01;
        assert!(!verify_consistency(5, &old_root, 12, &new_root, &proof));
    }

    #[test]
    fn consistency_rejects_truncated_and_extra_proof() {
        let all = leaves(11);
        let old_root = root(&all[..4]);
        let new_root = root(&all);
        let proof = consistency_proof(4, &all);

        let mut trunc = proof.clone();
        trunc.pop();
        assert!(!verify_consistency(4, &old_root, 11, &new_root, &trunc));

        let mut extra = proof;
        extra.push([0x00; 32]);
        assert!(!verify_consistency(4, &old_root, 11, &new_root, &extra));
    }

    proptest::proptest! {
        #[test]
        fn proptest_consistency_arbitrary(
            data in proptest::collection::vec(
                proptest::collection::vec(proptest::num::u8::ANY, 0..24),
                2..40usize,
            ),
            m_frac in 1..1_000u32,
        ) {
            let n = data.len();
            let m = 1 + (m_frac as usize % (n - 1)); // 1..=n-1
            let new_root = root(&data);
            let old_root = root(&data[..m]);
            let proof = consistency_proof(m, &data);
            proptest::prop_assert!(
                verify_consistency(m as u64, &old_root, n as u64, &new_root, &proof)
            );
        }
    }
}
