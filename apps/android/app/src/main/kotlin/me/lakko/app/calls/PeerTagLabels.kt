// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.concurrent.ConcurrentHashMap

/**
 * ADR-0021 — the per-call `peer_tag -> label` map behind the group-call
 * roster, and the resolver that keeps it current while the call is up.
 *
 * The SFU never sends a name. It sends a `peer_tag` bound to the JWT-verified
 * account id (`sfuPeerTag(call_id, account_id)`), and this device re-derives
 * the same tag over every account id it knows and matches. Two sources know
 * one:
 *
 *  * an accepted contact whose acknowledgement carried it
 *    (`IncomingContactRequest.fromAccountId`), and
 *  * a received profile (ADR-0048 v2), which is how a group member who is
 *    NOT a contact — the common case in a group — gets a name on their tile
 *    instead of `Participant N`.
 *
 * Both are keyed on the person's signature key, so the label is whatever the
 * one resolver (`ProfileDirectory.resolve`) says for that key: the rename,
 * the profile name or the verified `@username`; a call tile shows the
 * primary label alone (UX `docs/ux/profile-names.md` §3).
 *
 * ## The collision rule is the impersonation defence
 *
 * An account id in a profile is a self-claim, exactly as it is on a contact
 * acknowledgement. A member who claims someone ELSE's account id would have
 * that person's tag resolve to the claimant's name — unless the person's own
 * key claims the same id, which it does the moment their own profile or
 * contact row is known here. Two DIFFERENT keys behind one tag therefore hide
 * the tag on both sides ([PeerTagLabels.collided]) and the tile falls back to
 * `Participant N`; one key reached from both sources is the same person and
 * is not a collision.
 */

/** One `(account id, signature key)` claim, from either source. */
public data class PeerTagSource(
    /** Lowercase hex of the 16-byte account id. */
    val accountIdHex: String,
    /** The person's signature key, standard base64 — the resolver's key. */
    val signatureKeyB64: String,
)

/** The built map: a tag either labels one person or is hidden. */
public class PeerTagLabels(
    private val labels: Map<String, String>,
    /** Tags two different keys claim; never labelled. */
    public val collided: Set<String>,
) {
    /** The label for [peerTag], or `null` when unknown or collided. */
    public fun labelFor(peerTag: String): String? =
        if (peerTag.isEmpty() || peerTag in collided) null else labels[peerTag]

    /** Every labelled tag (tests + diagnostics). */
    public val size: Int get() = labels.size

    public companion object {
        /** No claims at all: every peer is unresolved. */
        public val EMPTY: PeerTagLabels = PeerTagLabels(emptyMap(), emptySet())
    }
}

/**
 * Build the map from every claim of both sources.
 *
 * @param sources accepted contacts and stored profiles, in any order, with
 *   duplicates allowed (a contact who also sent a profile appears twice
 *   under the same key).
 * @param tagOf the ADR-0021 derivation for this call, `sfuPeerTag(callId, ·)`.
 * @param labelOf the display resolver for a signature key.
 */
public fun buildPeerTagLabels(
    sources: Iterable<PeerTagSource>,
    tagOf: (accountIdHex: String) -> String,
    labelOf: (signatureKeyB64: String) -> String,
): PeerTagLabels {
    // Distinct keys per tag: the same person from both sources is one key.
    val keysByTag = LinkedHashMap<String, LinkedHashSet<String>>()
    for (source in sources) {
        val tag = tagOf(source.accountIdHex)
        keysByTag.getOrPut(tag) { LinkedHashSet() }.add(source.signatureKeyB64)
    }
    val labels = HashMap<String, String>()
    val collided = HashSet<String>()
    for ((tag, keys) in keysByTag) {
        if (keys.size == 1) labels[tag] = labelOf(keys.first()) else collided.add(tag)
    }
    return PeerTagLabels(labels, collided)
}

/**
 * The live roster resolver for one call: the [buildPeerTagLabels] map,
 * rebuilt when the sources change, plus the arrival-order `Participant N`
 * fallback for the peers it cannot name.
 *
 * ## Rebuilt lazily, not frozen at call start
 *
 * A profile can arrive DURING the call — a newcomer's introduce-yourself
 * request is answered within about half a minute of them joining — so the map
 * is rebuilt whenever [sourcesVersion] has moved since the last build. The
 * version is read on every [resolve] (one per roster event) and by [reapply],
 * which the host runs on the directory's change flow so a tile already on
 * screen picks the name up.
 *
 * ## The fallback never renumbers
 *
 * `Participant N` is numbered in arrival order among the peers that failed to
 * resolve, keyed on the peer id: the resolver is asked about the same peer
 * more than once (the welcome snapshot, then peer-joined, then every reapply),
 * and a label that renumbered itself between two of those would be worse than
 * the pseudonym it replaces. A peer that resolves later keeps its number
 * reserved; nobody else moves down.
 */
public class GroupCallNameResolver(
    private val sourcesVersion: () -> Long,
    private val sources: () -> List<PeerTagSource>,
    private val tagOf: (accountIdHex: String) -> String,
    private val labelOf: (signatureKeyB64: String) -> String,
    /** Where a failed rebuild is reported (injected: `android.util.Log` is
     *  not available to a JVM test). */
    private val warn: (message: String, error: Throwable) -> Unit = { _, _ -> },
) {
    private val lock = Any()
    private var builtVersion: Long? = null
    private var labels: PeerTagLabels = PeerTagLabels.EMPTY

    /** Arrival-order numbers for the peers that did not resolve. */
    private val unresolved = ConcurrentHashMap<String, String>()

    /** What each remote peer was last asked about, for [reapply]. */
    private val seen = ConcurrentHashMap<String, Pair<String, String>>()

    private fun current(): PeerTagLabels = synchronized(lock) {
        val version = sourcesVersion()
        if (builtVersion != version) {
            labels = try {
                buildPeerTagLabels(sources(), tagOf, labelOf)
            } catch (error: Throwable) {
                // A source that fails to load costs names, never the call.
                warn("peer-tag map not rebuilt", error)
                labels
            }
            builtVersion = version
        }
        labels
    }

    /** The roster label for a peer; the `SfuCallUi.resolveDisplayName` shape. */
    public fun resolve(peerId: String, accountPseudonym: String, peerTag: String): String {
        if (peerId.isNotEmpty()) seen[peerId] = accountPseudonym to peerTag
        return current().labelFor(peerTag) ?: run {
            // NOT the pseudonym. It reads as an identifier the user should
            // recognise and cannot; see [unresolvedParticipantLabel].
            val key = peerId.ifEmpty { accountPseudonym }
            unresolved.computeIfAbsent(key) { unresolvedParticipantLabel(unresolved.size + 1) }
        }
    }

    /**
     * Re-resolve every peer seen so far and rename the tiles whose label
     * changed. [participantIdOf] maps a peer id to its roster id.
     */
    public fun reapply(ui: CallUiState, participantIdOf: (peerId: String) -> String) {
        for ((peerId, asked) in seen) {
            val (pseudonym, peerTag) = asked
            ui.setParticipantName(participantIdOf(peerId), resolve(peerId, pseudonym, peerTag))
        }
    }
}
