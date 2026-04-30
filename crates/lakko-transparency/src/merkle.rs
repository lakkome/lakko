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
}
