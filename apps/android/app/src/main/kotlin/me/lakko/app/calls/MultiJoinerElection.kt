// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.CommitterCandidate
import uniffi.lakko_ffi_uniffi.electCommitter

/**
 * ADR-0032 committer election + presence tracking (pure, no I/O) — the
 * Kotlin twin of `apps/web/src/calls/multi-joiner-election.ts`.
 *
 * The multi-joiner call link (ADR-0032, G-21/G-22) survives the owner
 * leaving mid-call because the acting committer is a **pure function of
 * public state** — every member computes the same answer locally, no
 * election protocol:
 *
 *   > committer = the owner if the owner is present (connected to the SFU
 *   > room); otherwise the present member with the lowest MLS leaf index.
 *
 * The pure decision lives in the Rust core (`electCommitter`, UniFFI); this
 * module is the thin glue that (a) builds the [CommitterCandidate] list it
 * consumes from a snapshot of `Group.members()` + the owner's known
 * signature key + the present-key set, and (b) computes that present-key set
 * from what the client can observe on the SFU roster.
 *
 * ## Presence — how a peer is bound to a member
 *
 * The election needs "is member X present?" but the SFU roster is keyed on
 * an opaque random `peer_id`, not on an MLS leaf.
 *
 *   * **Self** — this client is connected, so its own signature key is
 *     always present.
 *   * **Every other member, the owner included** — each participant
 *     announces its own SFU peer id, signed with its MLS leaf key and sealed
 *     under the call seed (ADR-0078 section 2, `CallRelay.kt`); a verified
 *     announcement binds that peer to that leaf
 *     ([CallPresence.bindPeerToMember]). The owner is present iff a peer in
 *     the roster announced the capability's owner key. This replaces the
 *     ADR-0021 peer tag the SFU used to send; a member that has not announced
 *     yet is treated as absent until it does. A joiner holds no group before
 *     its Welcome, so before it it sees no owner and only re-broadcasts its
 *     KeyPackage, which is harmless.
 */

/** One member of the call MLS group, projected for the election. */
data class ElectionMember(
    /** OpenMLS leaf index at the current epoch. */
    val leafIndex: Int,
    /** Lowercase-hex of the member's 32-byte Ed25519 signature key — the
     *  stable identity handle used everywhere in this module (leaf indices
     *  renumber across removes; signature keys do not). */
    val signatureKeyHex: String,
)

/** Lowercase-hex of a byte slice — the canonical key form for the presence
 *  set + owner matching. */
fun hexKey(bytes: ByteArray): String {
    val sb = StringBuilder(bytes.size * 2)
    for (b in bytes) sb.append("%02x".format(b.toInt() and 0xff))
    return sb.toString()
}

/**
 * Compute the acting committer's leaf index via the Rust election, or `null`
 * when no member is present. Deterministic given identical inputs on every
 * client — the property the ADR relies on for split-brain-free failover.
 *
 * `is_owner[i]` marks the member whose signature key equals the capability's
 * owner key; `is_present[i]` marks members in [presentKeys].
 */
fun electActingCommitterLeaf(
    members: List<ElectionMember>,
    ownerKeyHex: String,
    presentKeys: Set<String>,
): Int? {
    if (members.isEmpty()) return null
    val candidates = members.map { m ->
        CommitterCandidate(
            leafIndex = m.leafIndex.toUInt(),
            isOwner = m.signatureKeyHex == ownerKeyHex,
            isPresent = presentKeys.contains(m.signatureKeyHex),
        )
    }
    return electCommitter(candidates)?.toInt()
}

/**
 * Tracks which MLS members are present (connected to the SFU room) from the
 * observable roster, and produces the present-key set the election consumes.
 * See the module doc for the binding model.
 *
 * @param selfKeyHex   This client's own signature-key hex — always present.
 * @param ownerKeyHex  The capability owner's signature-key hex.
 */
class CallPresence(
    private val selfKeyHex: String,
    private val ownerKeyHex: String,
) {
    /** The peer ids currently in the SFU room. */
    private val roster = LinkedHashSet<String>()
    /** peer_id -> member signature-key hex, from a verified announcement
     *  (ADR-0078 section 2) or the committer's own admission of a KeyPackage
     *  relayed by that peer. */
    private val peerToMember = HashMap<String, String>()

    /** Replace the roster from an SFU `Welcome` snapshot. */
    fun setRoster(peerIds: List<String>) {
        roster.clear()
        roster.addAll(peerIds)
    }

    /** A peer joined the SFU room. */
    fun addPeer(peerId: String) {
        roster.add(peerId)
    }

    /** The member signature-key hex bound to a peer, or `null` when this
     *  client never learnt who that peer is. Read it BEFORE [removePeer],
     *  which forgets the binding: it is what lets the committer remove a
     *  departed member's leaf and forget its admission so the person can
     *  come back. */
    fun memberKeyFor(peerId: String): String? = peerToMember[peerId]

    /** A peer left the SFU room. Also drops any member binding it had. */
    fun removePeer(peerId: String) {
        roster.remove(peerId)
        peerToMember.remove(peerId)
    }

    /** Bind a peer to a member signature key: from that peer's verified
     *  announcement, or (committer side) from the KeyPackage it relayed and
     *  this client admitted. A later announcement replaces it. */
    fun bindPeerToMember(peerId: String, memberKeyHex: String) {
        peerToMember[peerId] = memberKeyHex
    }

    /** `true` iff the owner is currently present: this client is the owner,
     *  or a peer in the roster announced the owner's key. Load-bearing for the
     *  common case + reclaim. */
    fun ownerPresent(): Boolean {
        if (ownerKeyHex == selfKeyHex) return true
        return peerToMember.any { (peerId, memberKeyHex) -> memberKeyHex == ownerKeyHex && peerId in roster }
    }

    /** The present member signature-key set for the election: self, the
     *  owner (when present), plus every peer->member binding whose peer is in
     *  the current roster. */
    fun presentKeySet(): Set<String> {
        val present = HashSet<String>()
        present.add(selfKeyHex)
        if (ownerPresent()) present.add(ownerKeyHex)
        for ((peerId, memberKeyHex) in peerToMember) {
            if (peerId in roster) present.add(memberKeyHex)
        }
        return present
    }
}
