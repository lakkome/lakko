// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.util.concurrent.ConcurrentHashMap

/**
 * Who is behind each remote tile of a call, of either kind, and what the tile
 * is called (`CallTileName.kt`). The Kotlin twin of the iOS
 * `GroupCallPeerNames` + `LinkCallTileNames` and of the naming in the web
 * `sfu-call-ui.ts` / `multi-joiner-call.ts`.
 *
 * ADR-0078 section 2: the SFU knows no account and says nothing about who a
 * peer is. Each participant seals a signed announcement of its SFU peer id
 * under the call's relay key; the session verifies it against the call's MLS
 * roster ([CallPeerAnnouncer]) and reports the person it names
 * ([peerIdentified]). Until then a tile is unnamed: an empty name, which the
 * call screen draws as its unnamed state, never a piece of an id.
 *
 * In a link call the joiners are strangers, so each also seals its ADR-0048
 * profile into the call group ([memberAnnounced]). The announced display name
 * and username name a peer only when that peer announced the very leaf that
 * sealed the profile: a relayed copy of someone else's profile names nobody.
 *
 * The name is read when asked, so a profile that arrives mid-call, or a
 * contact renamed, reaches a tile already on screen through [reapply], which
 * the host runs whenever the profile directory moves.
 */
public class CallPeerNames(
    /** What the one resolver calls a person (rename, profile name, verified
     *  `@username`), or `null` when it has only its placeholder. */
    private val personName: (personKeyB64: String) -> String?,
    /** The username the stored profile of a person claims, or `null`. */
    private val storedUsername: (personKeyB64: String) -> String? = { null },
    /** Where a failed lookup is reported (injected: `android.util.Log` is
     *  not available to a JVM test). */
    private val warn: (message: String, error: Throwable) -> Unit = { _, _ -> },
) {
    /** Every remote peer on the roster. */
    private val peers = ConcurrentHashMap.newKeySet<String>()

    /** SFU peer id -> the person its verified announcement names. */
    private val persons = ConcurrentHashMap<String, String>()

    /** SFU peer id -> the leaf key its verified announcement names. */
    private val memberKeys = ConcurrentHashMap<String, String>()

    /** SFU peer id -> the display name that peer's own sealed profile carries. */
    private val announcedNames = ConcurrentHashMap<String, String>()

    /** SFU peer id -> the username that peer's own sealed profile claims. */
    private val claimedUsernames = ConcurrentHashMap<String, String>()

    /** The tile name of [peerId], which is now on the roster; empty while it
     *  is unnamed. */
    public fun name(peerId: String): String {
        if (peerId.isNotEmpty()) peers.add(peerId)
        return currentName(peerId)
    }

    /**
     * [peerId]'s sealed announcement verified (ADR-0078 section 2): it is the
     * person [personKeyB64], through the leaf [memberKeyHex]. Returns whether
     * that changed anything (a re-announcement of the same person does not).
     */
    public fun peerIdentified(peerId: String, personKeyB64: String, memberKeyHex: String): Boolean {
        val before = persons.put(peerId, personKeyB64)
        val beforeKey = memberKeys.put(peerId, memberKeyHex)
        return before != personKeyB64 || beforeKey != memberKeyHex
    }

    /** The member [memberKeyHex] [peerId]'s announcement verified as, or
     *  `null` before it did. */
    public fun memberKeyOf(peerId: String): String? = memberKeys[peerId]

    /**
     * A link-call member sealed its profile into the call, relayed by
     * [peerId]. [senderLeafKeyHex] is the leaf key of the MLS-authenticated
     * sender. The announced name and username name [peerId] only when [peerId]
     * announced that leaf.
     */
    public fun memberAnnounced(
        peerId: String,
        senderLeafKeyHex: String?,
        displayName: String?,
        username: String?,
    ) {
        if (senderLeafKeyHex == null || memberKeys[peerId] != senderLeafKeyHex) return
        putOrRemove(announcedNames, peerId, displayName)
        putOrRemove(claimedUsernames, peerId, username)
    }

    /** [peerId] left: a later peer under the same id starts unnamed. */
    public fun forget(peerId: String) {
        peers.remove(peerId)
        persons.remove(peerId)
        memberKeys.remove(peerId)
        announcedNames.remove(peerId)
        claimedUsernames.remove(peerId)
    }

    /** Re-run the naming rule for every remote tile on [ui]. [participantIdOf]
     *  maps a peer id to its roster id. */
    public fun reapply(ui: CallUiState, participantIdOf: (peerId: String) -> String) {
        for (peerId in peers) ui.setParticipantName(participantIdOf(peerId), currentName(peerId))
    }

    private fun currentName(peerId: String): String {
        val person = persons[peerId]
        return callTileName(
            resolved = person?.let { safely { personName(it) } },
            announcedName = announcedNames[peerId],
            claimedUsername = claimedUsernames[peerId] ?: person?.let { safely { storedUsername(it) } },
        ).orEmpty()
    }

    private fun safely(read: () -> String?): String? =
        try {
            read()
        } catch (error: Throwable) {
            warn("person not resolved", error)
            null
        }

    private fun putOrRemove(map: ConcurrentHashMap<String, String>, peerId: String, value: String?) {
        val trimmed = value?.trim()
        if (trimmed.isNullOrEmpty()) map.remove(peerId) else map[peerId] = trimmed
    }
}
