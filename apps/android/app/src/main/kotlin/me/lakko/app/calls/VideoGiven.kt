// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.TileVideoQuality

/**
 * What the SFU actually gives this device of each person's video, and the
 * picture a tile therefore shows (ADR-0070 step 6): the twin of the web
 * `video-given.ts`.
 *
 * This device tells the SFU what each tile wants (`VideoWants.kt`). With
 * downlink control on, the SFU may give less when this device's downlink is
 * short, and says so in a `video-given` frame: the whole set each time, one
 * entry per person it sends video of. A track it does not give is paused on
 * purpose and resumed at a key frame.
 *
 * The rule here:
 *
 *   * the picture a tile shows is the **lower** of what the tile wants and
 *     what the SFU gives ([effectiveQuality]);
 *   * no `video-given` ever received, or a person the latest one does not
 *     name: what the tile wants, so an SFU with downlink control off behaves
 *     exactly as before this file;
 *   * a new frame replaces the whole previous set; a person who leaves is
 *     forgotten ([VideoGivenBook]);
 *   * what this device **asks** for never changes because of a `video-given`:
 *     the SFU must keep knowing the real wish to be able to give it back.
 *     Nothing here feeds [VideoWantsBook].
 *
 * Pure: no Compose, no Android, no timers.
 */

/**
 * The picture a tile shows: the lower of what it wants and what the SFU
 * gives. [given] is `null` when the SFU has said nothing about the person;
 * the tile then shows what it wants.
 */
internal fun effectiveQuality(wanted: TileVideoQuality, given: TileVideoQuality?): TileVideoQuality =
    if (given == null) wanted else lowerQuality(wanted, given)

/**
 * [effectiveQuality] as the quality the tile's sinks are placed for. `light`
 * means the light track alone, which is nothing at all for a person who
 * sends no light copy: such a tile keeps the heavy picture it wants (as
 * [askedQuality] has it), paused or not, because there is nothing else to
 * show.
 */
internal fun shownQuality(wanted: TileVideoQuality, given: TileVideoQuality?, hasLight: Boolean): TileVideoQuality {
    val effective = effectiveQuality(wanted, given)
    return if (effective == TileVideoQuality.LIGHT && !hasLight) askedQuality(wanted, false) else effective
}

/**
 * The track a tile's heavy sink is on, given what the tile wants and what
 * the SFU gives: the main track at `main`; the middle track at `middle` (the
 * main one while the person has no middle track); none at `light`, where the
 * light track is shown alone and nothing waits for frames the SFU is not
 * sending.
 */
internal fun <T> heavyTrackForGiven(
    wanted: TileVideoQuality,
    given: TileVideoQuality?,
    main: T?,
    middle: T?,
    hasLight: Boolean,
): T? = heavyTrackFor(shownQuality(wanted, given, hasLight), main, middle)

/**
 * The latest `video-given` set, by person.
 *
 * Empty until the first frame. Each frame replaces the whole set, so a person
 * an earlier frame named and the latest does not is back to "nothing said".
 */
internal class VideoGivenBook {
    private var given: Map<String, TileVideoQuality> = emptyMap()

    /** A `video-given` frame arrived: its entries are the whole set now. A
     *  person named twice has the later entry. */
    fun replace(entries: Iterable<Pair<String, TileVideoQuality>>) {
        val next = LinkedHashMap<String, TileVideoQuality>()
        for ((peer, quality) in entries) next[peer] = quality
        given = next
    }

    /** A person left. Returns whether they had an entry. */
    fun removePeer(peerId: String): Boolean {
        if (peerId !in given) return false
        given = given - peerId
        return true
    }

    /** What the SFU gives of [peerId]; `null` when it has not said. */
    fun givenOf(peerId: String): TileVideoQuality? = given[peerId]

    /** How many people the set names. */
    fun size(): Int = given.size

    /** The set as it stands, a copy. */
    fun snapshot(): Map<String, TileVideoQuality> = LinkedHashMap(given)

    /** Forget everything: the call is over. */
    fun clear() {
        given = emptyMap()
    }
}

/** One person whose shown picture a `video-given` frame changed. */
internal data class VideoGivenChange(
    val peer: String,
    val wanted: TileVideoQuality,
    /** What the SFU gives now; `null` when the frame does not name them. */
    val given: TileVideoQuality?,
)

/**
 * The people whose effective quality differs between two sets, in the order
 * of [wants] (what this device asks of each person now).
 */
internal fun videoGivenChanges(
    wants: Map<String, TileVideoQuality>,
    before: (peerId: String) -> TileVideoQuality?,
    after: (peerId: String) -> TileVideoQuality?,
): List<VideoGivenChange> {
    val out = ArrayList<VideoGivenChange>()
    for ((peer, wanted) in wants) {
        val now = after(peer)
        if (effectiveQuality(wanted, before(peer)) != effectiveQuality(wanted, now)) {
            out += VideoGivenChange(peer, wanted, now)
        }
    }
    return out
}

/**
 * One line for one `video-given` frame that changed the picture a tile shows:
 * every person it changed, each as `peer= want= given=` (`given=-` when the
 * frame no longer names them). `null` when it changed nobody's.
 */
internal fun describeGivenChanges(label: String, changes: List<VideoGivenChange>): String? {
    if (changes.isEmpty()) return null
    return "$label " + changes.joinToString(" | ") { c ->
        "peer=${c.peer} want=${c.wanted.wireName} given=${c.given?.wireName ?: "-"}"
    }
}
