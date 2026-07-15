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
 * ## Presence — what the reference can and cannot bind
 *
 * The election needs "is member X present?" but the SFU roster is keyed on
 * an opaque per-connection `peer_id`, not on an MLS leaf. Two bindings are
 * available WITHOUT any new wire (invariant #2):
 *
 *   * **Self** — this client is connected, so its own signature key is
 *     always present.
 *   * **Owner** — the owner's per-call `peer_tag`
 *     (`sfuPeerTag(call_id, owner_account_id)`, ADR-0021) is re-derivable
 *     from the capability's `owner_account_id`, so the owner is present iff
 *     that tag appears in the current roster. This makes the common case
 *     (owner-present -> owner commits) and the reclaim case (owner returns
 *     -> roster tag reappears -> committer reverts to the owner) correct.
 *
 * A NON-owner member's leaf<->peer binding is the documented residual: a
 * stranger joiner only knows its own + the owner's account id, so it cannot
 * in general map another joiner's peer to that joiner's leaf. The reference
 * binds a non-owner member opportunistically when it observes a relay FROM
 * that peer ([CallPresence.bindPeerToMember]); members it has never heard
 * from are treated as absent for the successor election. Per ADR-0032 this
 * is a low-severity residual (a redirected joiner lands in an isolated
 * group, never a plaintext or cross-group E2EE break); a strong-auth
 * successor delegation is deferred.
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
 * @param ownerPeerTag The owner's per-call `peer_tag` (`sfuPeerTag(call_id,
 *                     owner_account_id)`), or `null` when this client IS the
 *                     owner (then owner-presence rides [selfKeyHex]).
 */
class CallPresence(
    private val selfKeyHex: String,
    private val ownerKeyHex: String,
    private val ownerPeerTag: String?,
) {
    /** peer_id -> peer_tag (empty string when the SFU emits no tag). */
    private val roster = LinkedHashMap<String, String>()
    /** peer_id -> member signature-key hex, learned from a relay's `from`. */
    private val peerToMember = HashMap<String, String>()

    /** Replace the roster from an SFU `Welcome` snapshot. */
    fun setRoster(entries: List<Pair<String, String>>) {
        roster.clear()
        for ((peerId, peerTag) in entries) roster[peerId] = peerTag
    }

    /** A peer joined the SFU room. */
    fun addPeer(peerId: String, peerTag: String) {
        roster[peerId] = peerTag
    }

    /** A peer left the SFU room. Also drops any member binding it had. */
    fun removePeer(peerId: String) {
        roster.remove(peerId)
        peerToMember.remove(peerId)
    }

    /** Opportunistically bind a peer to a member signature key — learned
     *  when a relay arrives FROM that peer carrying a KeyPackage (committer
     *  side) or a Welcome (joiner side, binds the committer/owner peer). */
    fun bindPeerToMember(peerId: String, memberKeyHex: String) {
        peerToMember[peerId] = memberKeyHex
    }

    /** `true` iff the owner is currently present (this client is the owner,
     *  or the owner's peer_tag is in the roster). Load-bearing for the common
     *  case + reclaim. */
    fun ownerPresent(): Boolean {
        if (ownerKeyHex == selfKeyHex) return true
        val tag = ownerPeerTag ?: return false
        return roster.values.any { it == tag }
    }

    /** The present member signature-key set for the election: self, the
     *  owner (when present), plus every peer->member binding whose peer is in
     *  the current roster. */
    fun presentKeySet(): Set<String> {
        val present = HashSet<String>()
        present.add(selfKeyHex)
        if (ownerPresent()) present.add(ownerKeyHex)
        for ((peerId, memberKeyHex) in peerToMember) {
            if (roster.containsKey(peerId)) present.add(memberKeyHex)
        }
        return present
    }
}
