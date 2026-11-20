// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.moderation.AccountStanding
import me.lakko.app.moderation.StandingAnswer
import uniffi.lakko_ffi_uniffi.InfoSection

/*
 * ADR-0099 step 5, UX `reporting-and-moderation.md` section 3 ("A channel
 * that was removed") — a channel Lakko's moderators removed.
 *
 * The channels service answers every route of such a channel with
 * `410 channel-removed`. Unlike `channel-deleted`, nothing ends on the
 * device: the channel stays in the list, the posts already on the device
 * stay readable, and nothing that writes (composer, comments, reactions,
 * publisher tools, invite links) is offered. What the conversation and the
 * info say depends on the moderation log: "This channel was removed for
 * breaking Lakko's rules." only when the log proves it (C-98), else the
 * neutral "This channel isn't available.".
 *
 * Network reads of a removed channel stop: [mayAsk] lets one read through
 * per period (the verified answer's reuse time, else the standing service's
 * failure back-off). A normal answer on any route of the channel (a restore)
 * forgets the removal ([noteAnswered]) and everything comes back as before.
 * Kept in memory only.
 */

/** A channel the service answered `410 channel-removed` for. */
public data class ChannelRemoval(
    /** The moderation log proved the removal. */
    val verified: Boolean,
    /** The log was asked (whatever it said). */
    val checked: Boolean,
    /** No read of the channel before this time. */
    val nextAskAtMs: Long,
    /** How far apart reads of the channel are. */
    val periodMs: Long,
)

public object RemovedChannels {
    /** The `410` code of a removed channel. */
    public const val CODE: String = "channel-removed"

    /** The read period when nothing verified the removal (the standing
     *  service's failure back-off). */
    public const val FALLBACK_PERIOD_MS: Long = me.lakko.app.moderation.SuspensionStandings.FAILURE_BACKOFF_MS

    /** Bounds of a verified answer's reuse period (chosen). */
    public const val MIN_PERIOD_MS: Long = 60_000L
    public const val MAX_PERIOD_MS: Long = 6L * 60L * 60L * 1000L

    private val _state = MutableStateFlow<Map<String, ChannelRemoval>>(emptyMap())

    /** Removed channels by lower-case id. */
    public val state: StateFlow<Map<String, ChannelRemoval>> = _state.asStateFlow()

    /** The clock (tests replace it). */
    @Volatile
    internal var nowMs: () -> Long = System::currentTimeMillis

    /** Where the log is asked (tests replace it). */
    @Volatile
    internal var standingOf: suspend (channelIdHex: String) -> StandingAnswer =
        { me.lakko.app.moderation.SuspensionMarks.channelAnswer(it) }

    /** Where the check runs (tests run it inline). */
    @Volatile
    internal var launcher: (suspend () -> Unit) -> Unit = { block -> scope.launch { block() } }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** Whether [cause] (or a cause under it) is a `410 channel-removed`. */
    public fun isRemovedError(cause: Throwable?): Boolean {
        var c: Throwable? = cause
        val seen = HashSet<Throwable>()
        while (c != null && seen.add(c)) {
            if (c is ChannelsApiException && c.status == 410 && c.code == CODE) return true
            c = c.cause
        }
        return false
    }

    /** The service answered `410 channel-removed` for [channelIdHex]: mark
     *  it and ask the log. */
    public fun noteRemoved(channelIdHex: String) {
        val id = channelIdHex.lowercase()
        val now = nowMs()
        _state.update { m ->
            val held = m[id]
            m + (id to (held ?: ChannelRemoval(false, false, now + FALLBACK_PERIOD_MS, FALLBACK_PERIOD_MS)))
        }
        launcher { verify(id) }
    }

    /** Ask the log about [id] and file what it said. */
    internal suspend fun verify(id: String) {
        val answer = try {
            standingOf(id)
        } catch (cause: CancellationException) {
            throw cause
        } catch (_: Throwable) {
            StandingAnswer.UNKNOWN
        }
        val now = nowMs()
        val verified = answer.standing == AccountStanding.SUSPENDED
        val period = periodOf(answer, now)
        _state.update { m ->
            val held = m[id] ?: return@update m
            m + (id to held.copy(verified = verified, checked = true, periodMs = period))
        }
    }

    /** One read per verified reuse time; the failure back-off otherwise. */
    internal fun periodOf(answer: StandingAnswer, now: Long): Long {
        val until = answer.reuseUntilMs
        if (answer.standing != AccountStanding.SUSPENDED || until == null) return FALLBACK_PERIOD_MS
        return (until - now).coerceIn(MIN_PERIOD_MS, MAX_PERIOD_MS)
    }

    /** The channel answered normally (a restore) or ended: forget it. */
    public fun noteAnswered(channelIdHex: String) {
        val id = channelIdHex.lowercase()
        if (id !in _state.value) return
        _state.update { it - id }
    }

    /** Whether [channelIdHex] is marked removed. */
    public fun isRemoved(channelIdHex: String): Boolean = channelIdHex.lowercase() in _state.value

    /**
     * Whether a network read of [channelIdHex] may go out now: always for a
     * channel not marked removed; for a removed one, once per period (the
     * caller that is let through takes the period's one read).
     */
    public fun mayAsk(channelIdHex: String): Boolean {
        val id = channelIdHex.lowercase()
        if (id !in _state.value) return true
        val now = nowMs()
        var allowed = false
        _state.update { m ->
            val held = m[id] ?: return@update m.also { allowed = true }
            if (now < held.nextAskAtMs) {
                allowed = false
                m
            } else {
                allowed = true
                m + (id to held.copy(nextAskAtMs = now + held.periodMs))
            }
        }
        return allowed
    }

    /** Sign-out, forget device. */
    public fun clear() {
        _state.value = emptyMap()
    }
}

/**
 * The line a removed channel shows (catalog key): the removal only when the
 * log proved it, the neutral line otherwise; `null` for a channel that is not
 * removed.
 */
public fun channelRemovedLineKey(removal: ChannelRemoval?): String? = when {
    removal == null -> null
    removal.verified -> "suspended.channel_removed"
    else -> "channels.unavailable"
}

/**
 * The channel info sections a removed channel keeps: what only reads the
 * device (the name, the description, the person's own find-me switch) and
 * the leave row. Report goes too: moderators already acted on the channel. Everything that writes to the channel or reads
 * it over the network (rename, invite link, invite people, subscribers,
 * reactions, publishers, the channel type, delete) is withheld. The owner
 * and publishers, who have no Unsubscribe, get it too, so they can take the
 * channel out of their list ([ChannelUnsubscribe.leaveRemoved]).
 */
public fun removedChannelSections(sections: List<InfoSection>, removed: Boolean): List<InfoSection> {
    if (!removed) return sections
    val kept = sections.filter { it in REMOVED_CHANNEL_KEPT_SECTIONS }
    return if (InfoSection.UNSUBSCRIBE in kept) kept else kept + InfoSection.UNSUBSCRIBE
}

private val REMOVED_CHANNEL_KEPT_SECTIONS: Set<InfoSection> = setOf(
    InfoSection.HANDLE,
    InfoSection.CHANNEL_DESCRIPTION,
    InfoSection.MEMBERS_CAN_FIND_ME,
    InfoSection.UNSUBSCRIBE,
)

/**
 * The timeline of a removed channel when the reader holds nothing this
 * session: the text posts the device kept in its channel-post log (ADR-0063),
 * oldest first. Attachments are not in that log.
 */
public fun timelineFromPostLog(posts: List<me.lakko.app.feed.PersistedChannelPost>): ChannelTimeline =
    ChannelTimeline(
        posts = posts.sortedBy { it.cursor }.map { p ->
            ChannelPostItem(
                cursor = p.cursor,
                tsMs = p.serverTsMs,
                epoch = 0u,
                text = p.text,
                attachments = emptyList(),
                edited = false,
                wire = p.wireB64?.let { b ->
                    try {
                        java.util.Base64.getUrlDecoder().decode(b.trimEnd('='))
                    } catch (_: IllegalArgumentException) {
                        null
                    }
                },
            )
        },
        earlierUnavailable = false,
    )
