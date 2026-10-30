// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The one-way key tree over a person's contacts (ADR-0086 section 3, after
//! Canetti et al. 1999).
//!
//! ## The tree
//!
//! A binary tree of fixed height [`TREE_HEIGHT`], nodes numbered as a heap:
//! the root is 1, the children of `v` are `2v` and `2v + 1`, the leaves are
//! `2^16 ..= 2^17 - 1`. Only the leftmost `1 << capacity_log` leaves exist.
//! Every node has a 32-byte key. Each contact holds one leaf key; the epoch
//! key comes from the root key.
//!
//! ## Edges: one way up, or one wrap
//!
//! The header holds one optional **edge** per node `c`, a wrap of the key of
//! `c`'s parent under `c`'s key. A member walks from its leaf to the root:
//!
//! ```text
//! edge present   parent key = open(edge, under the current key)
//! edge absent    parent key = up(current key)        one way
//! ```
//!
//! So each internal node has one **source** child whose key it comes from by
//! `up`, with no edge, and the other child holds a wrap of it. That gives
//! exactly one wrap per internal node, and a path of at most `capacity_log`
//! edges, which is all a member reads: 570 bytes at 150 contacts.
//!
//! ## What decides the keys
//!
//! Everything is a pure function of the owner seed, the tree's salt and one
//! sparse map, `stamps`: a leaf that lost a member records the epoch it did.
//!
//! ```text
//! key(leaf)  = leaf_key(owner_seed, salt, leaf, stamp(leaf))     stamp 0 by default
//! source(v)  = the leaf with the highest stamp below v, leftmost on a tie
//! key(v)     = up^height(v)( key(source(v)) )
//! ```
//!
//! So [`TreeOwner`] holds no keys at rest, and the state a device keeps
//! ([`TreeOwner::encode_state`]) holds none either.
//!
//! ## Removing a member
//!
//! The leaf takes a fresh key at the new epoch, which makes it the source of
//! every node above it. Along the path each node's edge goes away and its
//! sibling's edge is rewritten to the new parent key: `capacity_log` wraps,
//! and one chain entry. Nothing else moves.
//!
//! **Why that is final.** A member only ever learns keys of its own
//! ancestors: it starts from its leaf, opens wraps made under keys of its
//! ancestors, and steps up one way. Removing it replaces every ancestor key
//! with one derived from a leaf key it never held, and the new wraps are made
//! under sibling keys, which are not its ancestors. A stamp never falls, so
//! an old key never comes back. That holds for any set of removed members
//! together: `tests/tree.rs` runs the closure over everything they held.
//!
//! **A sibling of the removed leaf** opens a wrap of its parent's new key and
//! goes on one way, so it learns the new keys above it and none below.
//!
//! ## Adding a member needs no write
//!
//! Every leaf of the capacity exists from the start, member or not, so the
//! server cannot count contacts below the capacity bucket. A new contact is
//! given the key of a free leaf: no edge changes, and no epoch moves. A leaf
//! a member left stays in the tree under its fresh key, free for the next.
//! Past the capacity, the tree doubles: the next subtree's edges are written
//! and the root key does not change, because the old top stays the source of
//! the node above it. A member may read older rows through the chain, so a
//! join needs no new keys for the past (ADR-0086 section 3).
//!
//! ## What the server sees
//!
//! Which edges a write touches: the path of a removed leaf, as a position and
//! never as a person. A member's path read names its position too. ADR-0086
//! section 8 records what a read reveals; the position is one more thing it
//! reveals, and the relay does not hide it: over time the positions read say
//! how many leaves are in use, which the capacity bucket does not.

use std::collections::{BTreeMap, BTreeSet};

use lakko_crypto::SecretBytes;

use crate::codec::Reader;
use crate::epoch::EpochKey;
use crate::error::CircleError;
use crate::head::{DirectoryHead, MemberGrant};
use crate::keys::{
    self, leaf_key, node_wrap_key, open_wrap, seal_wrap, up, DIRECTORY_ID_LEN, KEY_LEN, SALT_LEN,
    WRAP_LEN,
};

/// Height of the tree.
pub const TREE_HEIGHT: u32 = 16;
/// Most leaves a tree holds.
pub const MAX_LEAVES: u32 = 1 << TREE_HEIGHT;
/// The highest node number.
pub const MAX_NODE: u32 = 2 * MAX_LEAVES - 1;
/// A new tree holds `1 << MIN_CAPACITY_LOG` leaves. Chosen, not measured.
pub const MIN_CAPACITY_LOG: u8 = 4;
/// The largest capacity.
pub const MAX_CAPACITY_LOG: u8 = 16;

/// The last epoch of a tree. A chain entry stays for good, so this is what
/// bounds a directory's chain: 1,048,576 removals, 60 bytes each. A tree at the
/// end of its epochs is started over. Chosen, not measured.
pub const MAX_EPOCH: u64 = 1 << 20;

const ROOT: u32 = 1;
const EDGE_AAD_DOMAIN: &[u8] = b"lakko/circle/edge/v1\0";

/// A wrapped key in the header.
pub type Wrap = [u8; WRAP_LEN];

/// The heap number of `leaf`.
#[must_use]
pub const fn leaf_node(leaf: u32) -> u32 {
    MAX_LEAVES + leaf
}

const fn parent(node: u32) -> u32 {
    node / 2
}

const fn sibling(node: u32) -> u32 {
    node ^ 1
}

/// Levels below `node`: 0 for a leaf.
fn height(node: u32) -> u32 {
    TREE_HEIGHT.saturating_sub(node.checked_ilog2().unwrap_or(0))
}

/// The leaves below `node`, as `first .. end`.
fn leaf_range(node: u32) -> (u32, u32) {
    let h = height(node);
    let first = (node << h) - MAX_LEAVES;
    (first, first + (1 << h))
}

fn edge_aad(directory_id: &[u8; DIRECTORY_ID_LEN], node: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(EDGE_AAD_DOMAIN.len() + DIRECTORY_ID_LEN + 4);
    aad.extend_from_slice(EDGE_AAD_DOMAIN);
    aad.extend_from_slice(directory_id);
    aad.extend_from_slice(&node.to_be_bytes());
    aad
}

/// The nodes whose edges a member at `leaf` reads, leaf first:
/// `capacity_log` of them. Above those no edge exists.
///
/// # Errors
///
/// [`CircleError::NotAMember`] for a leaf outside the capacity.
pub fn path_nodes(leaf: u32, capacity_log: u8) -> Result<Vec<u32>, CircleError> {
    if capacity_log > MAX_CAPACITY_LOG || u64::from(leaf) >= 1u64 << capacity_log {
        return Err(CircleError::NotAMember);
    }
    let mut node = leaf_node(leaf);
    let mut out = Vec::with_capacity(usize::from(capacity_log));
    for _ in 0..capacity_log {
        out.push(node);
        node = parent(node);
    }
    Ok(out)
}

/// Derive the current epoch key from a grant, the head, and the edges of the
/// grant's path. An edge the server does not return reads as absent, which
/// gives a key that does not match the head: the result is a refusal, never a
/// wrong key used.
///
/// # Errors
///
/// [`CircleError::NotAMember`] when the leaf is outside the capacity,
/// [`CircleError::Authentication`] when an edge does not open under the key
/// reached so far, [`CircleError::HeaderMismatch`] when the walk ends on
/// another read key than the head names: a stale or partial header, or a
/// member who was removed.
pub fn member_epoch_key(
    grant: &MemberGrant,
    head: &DirectoryHead,
    edges: &[(u32, Wrap)],
) -> Result<EpochKey, CircleError> {
    let path = path_nodes(grant.leaf, head.capacity_log)?;
    let mut key = grant.leaf_key.clone();
    let mut node = leaf_node(grant.leaf);
    while node != ROOT {
        let edge = path
            .contains(&node)
            .then(|| edges.iter().find(|(at, _)| *at == node))
            .flatten();
        key = match edge {
            Some((_, wrap)) => open_wrap(
                &node_wrap_key(&key),
                &edge_aad(&grant.directory_id, node),
                wrap,
            )?,
            None => up(&key),
        };
        node = parent(node);
    }
    let epoch_key = EpochKey::from_root(grant.directory_id, head.epoch, &key);
    if epoch_key.read_public_key() == head.read_pk {
        Ok(epoch_key)
    } else {
        Err(CircleError::HeaderMismatch)
    }
}

/// The edges a change writes. A later operation overrides an earlier one on
/// the same node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeUpdate {
    /// Edges to write.
    pub puts: BTreeMap<u32, Wrap>,
    /// Edges to delete.
    pub deletes: BTreeSet<u32>,
}

impl TreeUpdate {
    fn put(&mut self, node: u32, wrap: Wrap) {
        self.deletes.remove(&node);
        self.puts.insert(node, wrap);
    }

    fn delete(&mut self, node: u32) {
        self.puts.remove(&node);
        self.deletes.insert(node);
    }

    /// Fold `later` over this update.
    pub fn merge(&mut self, later: Self) {
        for node in later.deletes {
            self.delete(node);
        }
        for (node, wrap) in later.puts {
            self.put(node, wrap);
        }
    }

    /// Whether nothing is to be written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.puts.is_empty() && self.deletes.is_empty()
    }
}

/// The owner's side of the tree. Holds the owner seed and no node key at rest.
#[derive(Clone)]
pub struct TreeOwner {
    owner_seed: SecretBytes<KEY_LEN>,
    directory_id: [u8; DIRECTORY_ID_LEN],
    salt: [u8; SALT_LEN],
    epoch: u64,
    capacity_log: u8,
    assigned: BTreeSet<u32>,
    stamps: BTreeMap<u32, u64>,
}

impl core::fmt::Debug for TreeOwner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TreeOwner")
            .field("epoch", &self.epoch)
            .field("capacity_log", &self.capacity_log)
            .field("members", &self.assigned.len())
            .finish_non_exhaustive()
    }
}

impl TreeOwner {
    /// A new tree at epoch 1, and every edge it needs.
    ///
    /// `salt` must be fresh: see [`crate::keys`].
    ///
    /// # Errors
    ///
    /// [`CircleError::Seal`] on a cipher failure.
    pub fn create(
        owner_seed: SecretBytes<KEY_LEN>,
        salt: [u8; SALT_LEN],
    ) -> Result<(Self, TreeUpdate), CircleError> {
        let directory_id = keys::directory_id(&keys::owner_public_key(&owner_seed));
        let tree = Self {
            owner_seed,
            directory_id,
            salt,
            epoch: 1,
            capacity_log: MIN_CAPACITY_LOG,
            assigned: BTreeSet::new(),
            stamps: BTreeMap::new(),
        };
        let mut update = TreeUpdate::default();
        tree.emit_subtree(tree.top(), &mut update)?;
        Ok((tree, update))
    }

    /// The directory this tree belongs to.
    #[must_use]
    pub const fn directory_id(&self) -> &[u8; DIRECTORY_ID_LEN] {
        &self.directory_id
    }

    /// The current epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The tree holds `1 << capacity_log` leaves.
    #[must_use]
    pub const fn capacity_log(&self) -> u8 {
        self.capacity_log
    }

    /// The leaves that are assigned to a contact.
    #[must_use]
    pub const fn members(&self) -> &BTreeSet<u32> {
        &self.assigned
    }

    /// The owner seed.
    pub(crate) const fn owner_seed(&self) -> &SecretBytes<KEY_LEN> {
        &self.owner_seed
    }

    /// The node at the top of the existing leaves.
    const fn top(&self) -> u32 {
        1 << (TREE_HEIGHT - self.capacity_log as u32)
    }

    const fn capacity(&self) -> u32 {
        1 << self.capacity_log
    }

    fn stamp_of(&self, leaf: u32) -> u64 {
        self.stamps.get(&leaf).copied().unwrap_or(0)
    }

    /// The leaf whose key `node`'s key comes from, with its stamp.
    fn source(&self, node: u32) -> (u32, u64) {
        let (first, end) = leaf_range(node);
        let end = end.min(self.capacity());
        let mut best = (first, 0u64);
        for (&leaf, &stamp) in self.stamps.range(first..end) {
            if stamp > best.1 {
                best = (leaf, stamp);
            }
        }
        best
    }

    fn key_of(&self, node: u32) -> SecretBytes<KEY_LEN> {
        let (leaf, stamp) = self.source(node);
        let mut key = leaf_key(&self.owner_seed, &self.salt, leaf, stamp);
        for _ in 0..height(node) {
            key = up(&key);
        }
        key
    }

    /// The current epoch key.
    #[must_use]
    pub fn epoch_key(&self) -> EpochKey {
        EpochKey::from_root(self.directory_id, self.epoch, &self.key_of(ROOT))
    }

    fn wrap_edge(
        &self,
        node: u32,
        node_key: &SecretBytes<KEY_LEN>,
        parent_key: &SecretBytes<KEY_LEN>,
    ) -> Result<Wrap, CircleError> {
        seal_wrap(
            &node_wrap_key(node_key),
            &edge_aad(&self.directory_id, node),
            parent_key,
        )
    }

    /// Emit every edge below `node`, in one pass, and return its key and the
    /// stamp of its source.
    fn emit_subtree(
        &self,
        node: u32,
        update: &mut TreeUpdate,
    ) -> Result<(SecretBytes<KEY_LEN>, u64), CircleError> {
        if height(node) == 0 {
            let leaf = node - MAX_LEAVES;
            let stamp = self.stamp_of(leaf);
            return Ok((leaf_key(&self.owner_seed, &self.salt, leaf, stamp), stamp));
        }
        let (left_key, left_stamp) = self.emit_subtree(2 * node, update)?;
        let (right_key, right_stamp) = self.emit_subtree(2 * node + 1, update)?;
        if right_stamp > left_stamp {
            let key = up(&right_key);
            let wrap = self.wrap_edge(2 * node, &left_key, &key)?;
            update.put(2 * node, wrap);
            Ok((key, right_stamp))
        } else {
            let key = up(&left_key);
            let wrap = self.wrap_edge(2 * node + 1, &right_key, &key)?;
            update.put(2 * node + 1, wrap);
            Ok((key, left_stamp))
        }
    }

    /// Give a contact a free leaf. The tree doubles when none is free, which
    /// is the only time this writes edges; the epoch never moves.
    ///
    /// # Errors
    ///
    /// [`CircleError::TreeFull`] at [`MAX_LEAVES`] members,
    /// [`CircleError::Seal`] on a cipher failure.
    pub fn add_member(&mut self) -> Result<(MemberGrant, TreeUpdate), CircleError> {
        let mut update = TreeUpdate::default();
        let free = self.first_free_leaf();
        let leaf = if let Some(leaf) = free {
            leaf
        } else {
            if self.capacity_log == MAX_CAPACITY_LOG {
                return Err(CircleError::TreeFull);
            }
            let leaf = self.capacity();
            self.grow(&mut update)?;
            leaf
        };
        self.assigned.insert(leaf);
        let grant = MemberGrant {
            directory_id: self.directory_id,
            leaf,
            leaf_key: leaf_key(&self.owner_seed, &self.salt, leaf, self.stamp_of(leaf)),
        };
        Ok((grant, update))
    }

    fn first_free_leaf(&self) -> Option<u32> {
        let mut candidate = 0u32;
        for &leaf in &self.assigned {
            if leaf != candidate {
                break;
            }
            candidate += 1;
        }
        (candidate < self.capacity()).then_some(candidate)
    }

    /// Double the leaves. The new subtree is the right child of the node
    /// above the old top; nothing in it was ever stamped, so the old top stays
    /// that node's source and no key above it changes.
    fn grow(&mut self, update: &mut TreeUpdate) -> Result<(), CircleError> {
        let old_top = self.top();
        self.capacity_log += 1;
        let new_subtree = sibling(old_top);
        let (subtree_key, _) = self.emit_subtree(new_subtree, update)?;
        let parent_key = self.key_of(parent(old_top));
        let wrap = self.wrap_edge(new_subtree, &subtree_key, &parent_key)?;
        update.put(new_subtree, wrap);
        Ok(())
    }

    /// Remove the members at `leaves` in one epoch. Returns the edges to
    /// write and the chain entry of the new epoch.
    ///
    /// # Errors
    ///
    /// [`CircleError::Empty`] for no leaves, [`CircleError::NotAMember`] when
    /// one of them is not assigned (nothing changes), [`CircleError::Overflow`]
    /// at the last epoch, [`CircleError::Seal`] on a cipher failure.
    pub fn remove_members(&mut self, leaves: &[u32]) -> Result<(TreeUpdate, Wrap), CircleError> {
        if leaves.is_empty() {
            return Err(CircleError::Empty);
        }
        if leaves.iter().any(|leaf| !self.assigned.contains(leaf)) {
            return Err(CircleError::NotAMember);
        }
        if self.epoch >= MAX_EPOCH {
            return Err(CircleError::Overflow);
        }
        let next = self.epoch + 1;
        let previous = self.epoch_key();
        for leaf in leaves {
            self.assigned.remove(leaf);
            self.stamps.insert(*leaf, next);
        }
        self.epoch = next;

        let mut changed = BTreeSet::new();
        for leaf in leaves {
            let mut node = leaf_node(*leaf);
            for _ in 0..self.capacity_log {
                node = parent(node);
                changed.insert(node);
            }
        }
        let mut update = TreeUpdate::default();
        for &node in &changed {
            let (left, right) = (2 * node, 2 * node + 1);
            let (source_leaf, _) = self.source(node);
            let (first, end) = leaf_range(left);
            let (source_child, other) = if (first..end).contains(&source_leaf) {
                (left, right)
            } else {
                (right, left)
            };
            let wrap = self.wrap_edge(other, &self.key_of(other), &self.key_of(node))?;
            update.delete(source_child);
            update.put(other, wrap);
        }
        let chain = self.epoch_key().wrap_previous(&previous)?;
        Ok((update, chain))
    }

    /// The tree's state, which holds no key: the salt, the epoch, the capacity,
    /// which leaves are taken and which were stamped.
    ///
    /// ```text
    /// salt(16) || epoch(8) || capacity_log(1)
    ///   || member_count(4) || member_count x leaf(4)       ascending
    ///   || stamp_count(4)  || stamp_count x (leaf(4) || stamp(8))  ascending
    /// ```
    #[must_use]
    pub fn encode_state(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            SALT_LEN + 8 + 1 + 4 + self.assigned.len() * 4 + 4 + self.stamps.len() * 12,
        );
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.push(self.capacity_log);
        // Both sets hold at most MAX_LEAVES entries.
        #[allow(clippy::cast_possible_truncation)]
        out.extend_from_slice(&(self.assigned.len() as u32).to_be_bytes());
        for leaf in &self.assigned {
            out.extend_from_slice(&leaf.to_be_bytes());
        }
        #[allow(clippy::cast_possible_truncation)]
        out.extend_from_slice(&(self.stamps.len() as u32).to_be_bytes());
        for (leaf, stamp) in &self.stamps {
            out.extend_from_slice(&leaf.to_be_bytes());
            out.extend_from_slice(&stamp.to_be_bytes());
        }
        out
    }

    /// Rebuild a tree from [`Self::encode_state`], fail-closed. Lists must be
    /// strictly ascending, inside the capacity, and no stamp above the epoch.
    ///
    /// # Errors
    ///
    /// [`CircleError::Length`] or [`CircleError::Malformed`].
    pub fn decode_state(
        owner_seed: SecretBytes<KEY_LEN>,
        bytes: &[u8],
    ) -> Result<Self, CircleError> {
        let mut r = Reader::new(bytes);
        let salt = r.array()?;
        let epoch = r.u64()?;
        let capacity_log = r.u8()?;
        if epoch == 0
            || epoch > MAX_EPOCH
            || !(MIN_CAPACITY_LOG..=MAX_CAPACITY_LOG).contains(&capacity_log)
        {
            return Err(CircleError::Malformed);
        }
        let capacity = 1u32 << capacity_log;

        let members = r.u32()?;
        if members > capacity || (members as usize) * 4 > r.remaining() {
            return Err(CircleError::Length);
        }
        let mut assigned = BTreeSet::new();
        let mut last = None;
        for _ in 0..members {
            let leaf = r.u32()?;
            if leaf >= capacity || last.is_some_and(|l| leaf <= l) {
                return Err(CircleError::Malformed);
            }
            last = Some(leaf);
            assigned.insert(leaf);
        }

        let stamp_count = r.u32()?;
        if stamp_count > capacity || (stamp_count as usize) * 12 > r.remaining() {
            return Err(CircleError::Length);
        }
        let mut stamps = BTreeMap::new();
        let mut last = None;
        for _ in 0..stamp_count {
            let leaf = r.u32()?;
            let stamp = r.u64()?;
            if leaf >= capacity || last.is_some_and(|l| leaf <= l) {
                return Err(CircleError::Malformed);
            }
            if stamp == 0 || stamp > epoch {
                return Err(CircleError::Malformed);
            }
            last = Some(leaf);
            stamps.insert(leaf, stamp);
        }
        r.finish()?;
        let directory_id = keys::directory_id(&keys::owner_public_key(&owner_seed));
        Ok(Self {
            owner_seed,
            directory_id,
            salt,
            epoch,
            capacity_log,
            assigned,
            stamps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> SecretBytes<KEY_LEN> {
        keys::owner_seed(&[0x31; KEY_LEN])
    }

    fn tree() -> (TreeOwner, TreeUpdate) {
        TreeOwner::create(owner(), [0x41; SALT_LEN]).unwrap()
    }

    fn head_of(tree: &TreeOwner) -> DirectoryHead {
        DirectoryHead {
            epoch: tree.epoch(),
            version: 1,
            filter_version: 0,
            read_pk: tree.epoch_key().read_public_key(),
            capacity_log: tree.capacity_log(),
        }
    }

    #[test]
    fn node_numbers_are_a_heap() {
        assert_eq!(height(ROOT), TREE_HEIGHT);
        assert_eq!(height(leaf_node(0)), 0);
        assert_eq!(height(leaf_node(MAX_LEAVES - 1)), 0);
        assert_eq!(leaf_node(MAX_LEAVES - 1), MAX_NODE);
        assert_eq!(leaf_range(ROOT), (0, MAX_LEAVES));
        assert_eq!(leaf_range(leaf_node(5)), (5, 6));
        assert_eq!(leaf_range(sibling(2)), (MAX_LEAVES / 2, MAX_LEAVES));
    }

    #[test]
    fn a_new_tree_has_one_edge_per_internal_node() {
        let (t, update) = tree();
        assert_eq!(update.puts.len(), (1 << MIN_CAPACITY_LOG) - 1);
        assert!(update.deletes.is_empty());
        assert_eq!(t.epoch(), 1);
    }

    #[test]
    fn every_derivation_of_the_owner_reaches_one_key() {
        // The dense pass and the sparse lookup give the same root.
        let (t, update) = tree();
        let (mut grown, _) = (t.clone(), update);
        for _ in 0..=(1 << MIN_CAPACITY_LOG) {
            grown.add_member().unwrap();
        }
        assert_eq!(grown.capacity_log(), MIN_CAPACITY_LOG + 1);
        assert_eq!(grown.epoch_key(), t.epoch_key());
    }

    #[test]
    fn the_first_epoch_key_is_pinned() {
        let (t, _) = tree();
        assert_eq!(
            keys::hex(&t.epoch_key().read_public_key()),
            "a510822a3181b154b1c655af8f49df8ebdb56f78a52722196678a6958c3ef7f8"
        );
    }

    #[test]
    fn a_member_reads_exactly_its_path() {
        let (mut t, update) = tree();
        let (grant, _) = t.add_member().unwrap();
        let path = path_nodes(grant.leaf, t.capacity_log()).unwrap();
        assert_eq!(path.len(), usize::from(MIN_CAPACITY_LOG));
        let edges: Vec<(u32, Wrap)> = path
            .iter()
            .filter_map(|n| update.puts.get(n).map(|w| (*n, *w)))
            .collect();
        let head = head_of(&t);
        assert_eq!(
            member_epoch_key(&grant, &head, &edges).unwrap(),
            t.epoch_key()
        );
    }

    #[test]
    fn a_missing_edge_is_a_refusal_not_a_wrong_key() {
        let (mut t, update) = tree();
        t.add_member().unwrap();
        let (grant, _) = t.add_member().unwrap();
        assert_eq!(
            member_epoch_key(&grant, &head_of(&t), &[]),
            Err(CircleError::HeaderMismatch)
        );
        let _ = update;
    }

    #[test]
    fn an_edge_of_another_node_does_not_open() {
        let (mut t, update) = tree();
        t.add_member().unwrap();
        let (grant, _) = t.add_member().unwrap();
        // Leaf 1's first edge exists (its sibling, leaf 0, is the source).
        let node = leaf_node(grant.leaf);
        let some_other = update.puts.iter().find(|(n, _)| **n != node).unwrap();
        assert_eq!(
            member_epoch_key(&grant, &head_of(&t), &[(node, *some_other.1)]),
            Err(CircleError::Authentication)
        );
    }

    #[test]
    fn removing_writes_one_edge_per_level() {
        let (mut t, _) = tree();
        for _ in 0..4 {
            t.add_member().unwrap();
        }
        let (update, _chain) = t.remove_members(&[2]).unwrap();
        assert_eq!(update.puts.len(), usize::from(MIN_CAPACITY_LOG));
        assert_eq!(t.epoch(), 2);
        assert!(!t.members().contains(&2));
    }

    #[test]
    fn removing_someone_who_is_not_a_member_changes_nothing() {
        let (mut t, _) = tree();
        t.add_member().unwrap();
        let before = t.encode_state();
        assert_eq!(
            t.remove_members(&[0, 7]).map(|_| ()),
            Err(CircleError::NotAMember)
        );
        assert_eq!(t.encode_state(), before);
        assert_eq!(t.remove_members(&[]).map(|_| ()), Err(CircleError::Empty));
    }

    #[test]
    fn a_freed_leaf_goes_to_the_next_contact_under_a_new_key() {
        let (mut t, _) = tree();
        let (first, _) = t.add_member().unwrap();
        t.remove_members(&[first.leaf]).unwrap();
        let (second, _) = t.add_member().unwrap();
        assert_eq!(second.leaf, first.leaf);
        assert_ne!(second.leaf_key, first.leaf_key);
    }

    #[test]
    fn the_tree_stops_at_its_maximum() {
        let (mut t, _) = tree();
        t.capacity_log = MAX_CAPACITY_LOG;
        t.assigned = (0..MAX_LEAVES).collect();
        assert_eq!(t.add_member().map(|_| ()), Err(CircleError::TreeFull));
    }

    /// Adds and removes over a small tree, keeping the public edges as a
    /// reader sees them.
    #[derive(Clone, Debug)]
    enum Op {
        Add,
        Remove(usize),
        RemoveTwo(usize, usize),
    }

    fn apply_update(edges: &mut BTreeMap<u32, Wrap>, update: TreeUpdate) {
        for node in update.deletes {
            edges.remove(&node);
        }
        edges.extend(update.puts);
    }

    /// What a set of people could work out from what they held and the public
    /// edges: every key one way up, every edge any key opens. Pairs already
    /// tried are remembered, so a run stays quick.
    #[derive(Default)]
    struct Closure {
        keys: BTreeSet<[u8; KEY_LEN]>,
        tried: BTreeSet<([u8; KEY_LEN], u32, Wrap)>,
    }

    impl Closure {
        fn learn(&mut self, key: &SecretBytes<KEY_LEN>) {
            let mut current = key.clone();
            for _ in 0..TREE_HEIGHT {
                if !self.keys.insert(*current.as_bytes()) {
                    return;
                }
                current = up(&current);
            }
        }

        fn run(&mut self, directory_id: &[u8; 32], edges: &BTreeMap<u32, Wrap>) {
            loop {
                let mut found = Vec::new();
                for key in &self.keys {
                    for (node, wrap) in edges {
                        if !self.tried.insert((*key, *node, *wrap)) {
                            continue;
                        }
                        let wrapping = node_wrap_key(&SecretBytes::new(*key));
                        if let Ok(opened) =
                            open_wrap(&wrapping, &edge_aad(directory_id, *node), wrap)
                        {
                            found.push(opened);
                        }
                    }
                }
                if found.is_empty() {
                    return;
                }
                for key in &found {
                    self.learn(key);
                }
            }
        }
    }

    /// Every node key of the tree that is current, the root's included.
    fn all_current_keys(tree: &TreeOwner) -> Vec<(u32, [u8; KEY_LEN])> {
        let mut out = Vec::new();
        let mut node = tree.top();
        while node >= ROOT {
            out.push((node, *tree.key_of(node).as_bytes()));
            node = parent(node);
        }
        for node in tree.top()..=MAX_NODE {
            let (first, _) = leaf_range(node);
            if first < tree.capacity() {
                out.push((node, *tree.key_of(node).as_bytes()));
            }
        }
        out
    }

    fn check_run(ops: &[Op]) {
        let (mut t, first) = tree();
        let mut edges = BTreeMap::new();
        apply_update(&mut edges, first);
        let directory_id = *t.directory_id();
        let mut present: BTreeMap<u32, MemberGrant> = BTreeMap::new();
        let mut knowledge: BTreeMap<u32, Closure> = BTreeMap::new();
        // Everything every removed person ever knew, pooled.
        let mut removed = Closure::default();
        let mut removed_grants: Vec<MemberGrant> = Vec::new();
        let mut epoch_keys = vec![t.epoch_key()];

        for op in ops {
            let leaves: Vec<u32> = present.keys().copied().collect();
            let chosen: Vec<u32> = match op {
                Op::Add => {
                    let (grant, update) = t.add_member().unwrap();
                    apply_update(&mut edges, update);
                    let mut held = Closure::default();
                    held.learn(&grant.leaf_key);
                    knowledge.insert(grant.leaf, held);
                    present.insert(grant.leaf, grant);
                    vec![]
                },
                Op::Remove(i) if !leaves.is_empty() => vec![leaves[i % leaves.len()]],
                Op::RemoveTwo(i, j) if !leaves.is_empty() => {
                    let mut two = vec![leaves[i % leaves.len()], leaves[j % leaves.len()]];
                    two.dedup();
                    two
                },
                _ => vec![],
            };
            if !chosen.is_empty() {
                // What they know up to the moment they go.
                for leaf in &chosen {
                    let mut held = knowledge.remove(leaf).unwrap();
                    held.run(&directory_id, &edges);
                    for key in &held.keys {
                        removed.keys.insert(*key);
                    }
                    removed_grants.push(present.remove(leaf).unwrap());
                }
                let before = t.epoch_key();
                let (update, chain) = t.remove_members(&chosen).unwrap();
                apply_update(&mut edges, update);
                let now = t.epoch_key();
                assert_eq!(now.previous(&chain).unwrap(), before);
                epoch_keys.push(now);
            }

            let head = head_of(&t);
            // A member who stayed reaches the current key from its leaf and
            // its path, whatever happened around it.
            for grant in present.values() {
                let path = path_nodes(grant.leaf, t.capacity_log()).unwrap();
                let mine: Vec<(u32, Wrap)> = path
                    .iter()
                    .filter_map(|n| edges.get(n).map(|w| (*n, *w)))
                    .collect();
                assert_eq!(
                    member_epoch_key(grant, &head, &mine).unwrap(),
                    t.epoch_key()
                );
            }
            // Nobody who left reaches it, alone or together.
            for grant in &removed_grants {
                let path = path_nodes(grant.leaf, t.capacity_log()).unwrap();
                let theirs: Vec<(u32, Wrap)> = path
                    .iter()
                    .filter_map(|n| edges.get(n).map(|w| (*n, *w)))
                    .collect();
                assert!(member_epoch_key(grant, &head, &theirs).is_err());
            }
            removed.run(&directory_id, &edges);
            for (node, key) in all_current_keys(&t) {
                assert!(
                    !removed.keys.contains(&key),
                    "the people removed hold the current key of node {node}"
                );
            }
        }
    }

    fn op() -> impl proptest::strategy::Strategy<Value = Op> {
        use proptest::prelude::*;
        prop_oneof![
            3 => Just(Op::Add),
            2 => any::<usize>().prop_map(Op::Remove),
            1 => (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::RemoveTwo(a, b)),
        ]
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(32))]

        /// A member who stays always derives the current key. Nobody removed
        /// does, and everything all the removed people ever held, pooled and
        /// closed over the public edges, reaches no current node key.
        #[test]
        fn removal_is_final_for_any_set_of_people(
            ops in proptest::collection::vec(op(), 1..40),
        ) {
            check_run(&ops);
        }

        /// Restoring from the state gives the same tree, at any point.
        #[test]
        fn a_restored_tree_is_the_same_tree(
            ops in proptest::collection::vec(op(), 1..40),
        ) {
            let (mut t, _) = tree();
            for op in &ops {
                let leaves: Vec<u32> = t.members().iter().copied().collect();
                match op {
                    Op::Add => { t.add_member().unwrap(); },
                    Op::Remove(i) | Op::RemoveTwo(i, _) if !leaves.is_empty() => {
                        t.remove_members(&[leaves[i % leaves.len()]]).unwrap();
                    },
                    _ => {},
                }
                let back = TreeOwner::decode_state(owner(), &t.encode_state()).unwrap();
                proptest::prop_assert_eq!(back.epoch_key(), t.epoch_key());
                proptest::prop_assert_eq!(back.encode_state(), t.encode_state());
            }
        }

        /// The tree state decoder never panics and is canonical.
        #[test]
        fn a_tree_state_decodes_or_is_refused(
            bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..400),
        ) {
            if let Ok(t) = TreeOwner::decode_state(owner(), &bytes) {
                proptest::prop_assert_eq!(t.encode_state(), bytes);
            }
        }
    }

    #[test]
    fn a_tree_started_over_does_not_bring_old_keys_back() {
        // Same seed, new salt: a person removed from the old tree holds nothing
        // in the new one.
        let (mut old, _) = TreeOwner::create(owner(), [1; SALT_LEN]).unwrap();
        let (grant, _) = old.add_member().unwrap();
        let (mut new, _) = TreeOwner::create(owner(), [2; SALT_LEN]).unwrap();
        let (fresh, _) = new.add_member().unwrap();
        assert_eq!(grant.leaf, fresh.leaf);
        assert_ne!(grant.leaf_key, fresh.leaf_key);
        assert_ne!(
            old.epoch_key().read_public_key(),
            new.epoch_key().read_public_key()
        );
    }

    #[test]
    fn the_state_round_trips_and_holds_no_key() {
        let (mut t, _) = tree();
        for _ in 0..3 {
            t.add_member().unwrap();
        }
        t.remove_members(&[1]).unwrap();
        let state = t.encode_state();
        let back = TreeOwner::decode_state(owner(), &state).unwrap();
        assert_eq!(back.epoch_key(), t.epoch_key());
        assert_eq!(back.encode_state(), state);
    }

    #[test]
    fn a_state_is_refused_on_every_bound() {
        let (mut t, _) = tree();
        t.add_member().unwrap();
        t.add_member().unwrap();
        t.remove_members(&[0]).unwrap();
        let state = t.encode_state();
        assert_eq!(
            TreeOwner::decode_state(owner(), &state[..state.len() - 1]).map(|_| ()),
            Err(CircleError::Length)
        );
        let mut trailing = state.clone();
        trailing.push(0);
        assert_eq!(
            TreeOwner::decode_state(owner(), &trailing).map(|_| ()),
            Err(CircleError::Length)
        );
        // A stamp above the epoch.
        let mut stamp = state.clone();
        let at = state.len() - 8;
        stamp[at..].copy_from_slice(&99u64.to_be_bytes());
        assert_eq!(
            TreeOwner::decode_state(owner(), &stamp).map(|_| ()),
            Err(CircleError::Malformed)
        );
        // A member list past the capacity.
        let mut members = state;
        members[SALT_LEN + 8 + 1..SALT_LEN + 8 + 1 + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            TreeOwner::decode_state(owner(), &members).map(|_| ()),
            Err(CircleError::Length)
        );
    }
}
