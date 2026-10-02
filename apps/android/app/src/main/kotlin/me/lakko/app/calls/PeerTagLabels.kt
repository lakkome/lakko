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
 *    NOT a contact — the common case in a group — gets a name on their tile.
 *
 * Both are keyed on the person's signature key, so the label is whatever
 * [callNameOfPerson] says for that key: the one resolver's rename, profile
 * name or verified `@username`, then the username the stored profile claims,
 * as plain text (`CallTileName.kt`).
 *
 * ## The collision rule is the impersonation defence
 *
 * An account id in a profile is a self-claim, exactly as it is on a contact
 * acknowledgement. A member who claims someone ELSE's account id would have
 * that person's tag resolve to the claimant's name — unless the person's own
 * key claims the same id, which it does the moment their own profile or
 * contact row is known here. Two DIFFERENT keys behind one tag therefore hide
 * the tag on both sides ([PeerTagLabels.collided]) and the tile is named by
 * nothing this device knows; one key reached from both sources is the same
 * person and is not a collision.
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
 * @param labelOf the person's name for a signature key, or `null` when this
 *   device has none for them; such a tag is left unlabelled.
 */
public fun buildPeerTagLabels(
    sources: Iterable<PeerTagSource>,
    tagOf: (accountIdHex: String) -> String,
    labelOf: (signatureKeyB64: String) -> String?,
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
        if (keys.size == 1) {
            labelOf(keys.first())?.takeIf { it.isNotBlank() }?.let { labels[tag] = it }
        } else {
            collided.add(tag)
        }
    }
    return PeerTagLabels(labels, collided)
}

/**
 * The live roster resolver for one call, of either kind: the
 * [buildPeerTagLabels] map, rebuilt when the sources change, and, in a link
 * call, what each member announced inside the call. It applies the one rule of
 * `CallTileName.kt`.
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
 * ## An unnamed peer has an empty name
 *
 * A peer nothing has been learned about resolves to the empty string, which
 * the call screen draws as its unnamed state (`call.participant.unnamed`).
 * Never the SFU pseudonym, which is a piece of the account id, and never a
 * made-up numbered name.
 *
 * ## A link call's announcements
 *
 * The joiners of a link call are strangers to each other, so each member seals
 * its own profile into the call's group ([memberAnnounced]). The announced
 * display name and the claimed username name the tile as plain text; the
 * member is bound to a person, and the profile filed in the profile store,
 * only when [profileClaimMatchesPeer] holds for the relaying peer.
 */
public class GroupCallNameResolver(
    private val sourcesVersion: () -> Long,
    private val sources: () -> List<PeerTagSource>,
    private val tagOf: (accountIdHex: String) -> String,
    private val labelOf: (signatureKeyB64: String) -> String?,
    /** Where a failed rebuild is reported (injected: `android.util.Log` is
     *  not available to a JVM test). */
    private val warn: (message: String, error: Throwable) -> Unit = { _, _ -> },
) {
    private val lock = Any()
    private var builtVersion: Long? = null
    private var labels: PeerTagLabels = PeerTagLabels.EMPTY

    /** What each remote peer was last asked about, for [reapply]. */
    private val seen = ConcurrentHashMap<String, Pair<String, String>>()

    /** What a link-call member announced inside the call. */
    private class Announcement(
        /** The display name it announced, or `null`. */
        val displayName: String?,
        /** The username it claims, or `null`. */
        val username: String?,
        /** The person (account key, base64) it is bound to, or `null` when
         *  the claim did not match the relaying peer. */
        val personKeyB64: String?,
    )

    /** SFU peer id -> what that member announced (link calls). */
    private val announcements = ConcurrentHashMap<String, Announcement>()

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

    private fun nameOfPerson(signatureKeyB64: String): String? =
        try {
            labelOf(signatureKeyB64)
        } catch (error: Throwable) {
            warn("person not resolved", error)
            null
        }

    /**
     * The roster name for a peer, empty while it is unnamed; the
     * `SfuCallUi.resolveDisplayName` shape. [accountPseudonym] is kept for
     * [reapply] and never shown.
     */
    public fun resolve(peerId: String, accountPseudonym: String, peerTag: String): String {
        if (peerId.isNotEmpty()) seen[peerId] = accountPseudonym to peerTag
        val announcement = announcements[peerId]
        return callTileName(
            resolved = announcement?.personKeyB64?.let(::nameOfPerson) ?: current().labelFor(peerTag),
            announcedName = announcement?.displayName,
            claimedUsername = announcement?.username,
        ).orEmpty()
    }

    /**
     * A link-call member's sealed profile, relayed by [peerId].
     *
     * [senderKeyB64] is the MLS-authenticated sender, resolved to the account
     * key at receipt (C-68), or `null` when it did not resolve.
     * [claimedAccountIdHex] is the account id the profile states.
     *
     * @return whether the caller may file the profile in the profile store
     *   under [senderKeyB64]: only when the claimed account is the one the SFU
     *   admitted [peerId] as ([profileClaimMatchesPeer]). Otherwise the peer
     *   is bound to nobody and the tile shows what was announced.
     */
    public fun memberAnnounced(
        peerId: String,
        senderKeyB64: String?,
        displayName: String?,
        username: String?,
        claimedAccountIdHex: String?,
    ): Boolean {
        val file = senderKeyB64 != null &&
            profileClaimMatchesPeer(claimedAccountIdHex, seen[peerId]?.second, tagOf)
        announcements[peerId] = Announcement(
            displayName = displayName?.trim()?.takeIf { it.isNotEmpty() },
            username = username?.trim()?.takeIf { it.isNotEmpty() },
            personKeyB64 = senderKeyB64.takeIf { file },
        )
        return file
    }

    /** [peerId] left: forget what it announced and what it was asked about,
     *  so a later peer under the same id starts unnamed. */
    public fun forget(peerId: String) {
        seen.remove(peerId)
        announcements.remove(peerId)
    }

    /**
     * Re-resolve every peer seen so far and rename the tiles whose name
     * changed. [participantIdOf] maps a peer id to its roster id.
     */
    public fun reapply(ui: CallUiState, participantIdOf: (peerId: String) -> String) {
        for ((peerId, asked) in seen) {
            val (pseudonym, peerTag) = asked
            ui.setParticipantName(participantIdOf(peerId), resolve(peerId, pseudonym, peerTag))
        }
    }
}
