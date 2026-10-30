<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->
<!-- GENERATED SPEC: regenerate from the source when tree.rs changes -->

# `tree` — the one-way key tree of a circle directory (spec)

**Module:** `crates/lakko-circle/src/tree.rs`   **One line:** who can reach a directory's epoch key, and how a removal ends that for one person at the cost of one path.

## Why this module exists

ADR-0086 replaces data that moves on every removal with a key that changes. A contact holds one leaf key; the tree turns that into the current epoch key. Removing a contact must take effect in one small write, at any circle size, with nobody else doing anything.

## Public contract

- `TreeOwner::create(owner_seed, salt)`: a tree at epoch 1 and every edge it needs. The salt must be fresh.
- `TreeOwner::add_member`: gives a free leaf. Writes no edge unless the tree doubles, never moves the epoch, never changes the root key.
- `TreeOwner::remove_members(leaves)`: all-or-nothing. Moves the epoch once, for any number of leaves, and returns the edges to write and the chain entry. Nothing changes on a refusal.
- `TreeOwner::epoch_key`: the current key.
- `TreeOwner::encode_state` / `decode_state`: a state with no key in it. The owner seed plus the state rebuild the tree exactly.
- `path_nodes(leaf, capacity_log)`: the nodes a member reads, leaf first.
- `member_epoch_key(grant, head, edges)`: the current key from a grant and a path, or a refusal. It never returns a key the head does not name.

## State

Every key is a pure function of the owner seed, the salt and `stamps`, the sparse map of leaves that lost a member to the epoch they did:

- a leaf's key comes from `(owner seed, salt, leaf, stamp)`, stamp 0 by default;
- the **source** of a node is the leaf below it with the highest stamp, leftmost on a tie;
- a node's key is the source's key taken `height` steps up the one-way function.

An edge at node `c` is a wrap of the parent's key under `c`'s key. It exists for the child that is not the parent's source; the source child has none. Above `capacity_log` the tree is a left spine with no edges. Absent means one way up.

## Invariants upheld

- A member only ever learns keys of its own ancestors. A removal replaces all of them, from a leaf key the member never held, and wraps under sibling keys, which are not its ancestors.
- A stamp never falls, so an old key never comes back.
- Those two hold for any set of removed members together: pooling everything they held and closing it over every public edge reaches no current node key (`removal_is_final_for_any_set_of_people`).
- A member who stays reaches the current key from its leaf and its path after any sequence of adds and removals (same test).
- Two trees of one seed share no leaf key, because of the salt (`a_tree_started_over_does_not_bring_old_keys_back`).
- Growth does not change the root key (`every_derivation_of_the_owner_reaches_one_key`).

## Failure modes

- `Empty`, `NotAMember`: a removal of nothing, or of a leaf that is not assigned. Nothing changes.
- `TreeFull`: 65,536 members.
- `Overflow`: the last epoch, `MAX_EPOCH`.
- `Authentication`: an edge did not open under the key reached so far.
- `HeaderMismatch`: the walk ended on another read key. A stale or partial header, or a removed member. A missing edge reads as absent and lands here, never as a wrong key used.
- `Length`, `Malformed`: a state that does not decode.

No panic on any input.

## What this module does NOT do

- It holds no keys at rest and does no I/O.
- It does not know who is at a leaf. The leaf-to-contact map is the client's.
- It gives no backward secrecy on purpose: a contact who joins can reach older epochs through the chain (ADR-0086 section 3).
- It does not hide positions. The server sees which path a removal writes and which path a member reads, so the positions read over time say how many leaves are in use.

## Attacker exposure

`decode_state` takes device-local bytes; `member_epoch_key` takes a head and edges from the Delivery Service. Fuzz targets `state_decode` and `path_walk`.

## References

ADR-0086 section 3; Canetti et al. 1999; `tests/directory.rs`.
