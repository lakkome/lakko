// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json

/*
 * ADR-0091 phase 2 — a device that fell behind a conversation notices it and
 * comes back (`docs/ux/conversation-rejoin.md`).
 *
 * Everything here is the client half the core does not own:
 *
 *  - [ConversationBehindTracker]: per conversation, what the reads showed
 *    (refused reads, rows skipped above the local epoch, a refused publish,
 *    the time of the first row this device could not read) folded through the
 *    ONE core rule (`conversationBehindVerdict`, injected as [BehindVerdict]),
 *    kept durably so the waiting note survives a restart;
 *  - [ReentryRecordStore]: the durable record of the requests this device sent
 *    (the KeyPackages `Group.joinReentry` is handed), written BEFORE a send;
 *  - [ReentryAdmissionLedger]: the re-admitter's per-leaf facts the core's
 *    `reentryAdmissionVerdict` needs and cannot see;
 *  - [KnockKeyStore]: the group's knock public key the owner hands every
 *    member in-group (route 3), newest version kept;
 *  - [ReentryGapStore]: the gap lines of `docs/ux/conversation-rejoin.md` 2.4.
 *
 * Pure Kotlin with the native calls injected, so the host JVM tests (which do
 * not load the native core) drive every rule. Log lines carry no identifier.
 */

/** The Android log tag of the ADR-0091 phase-2 lines. */
internal const val REENTRY_LOG_TAG: String = "ChatReentry"

/** One identifier-free line; never throws (a host JVM has no `Log`). */
internal fun reentryLog(warn: Boolean, line: String) {
    try {
        if (warn) android.util.Log.w(REENTRY_LOG_TAG, line) else android.util.Log.i(REENTRY_LOG_TAG, line)
    } catch (_: Throwable) {
        // no log on the host
    }
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/** Where this device stands in one conversation (mirror of `FfiBehindState`). */
@Serializable
internal enum class BehindLevel {
    /** Nothing suggests the device is behind. */
    IN_STEP,

    /** It stops acting there and asks to come back; nothing is shown yet. */
    SUSPECTED,

    /** Shown: the composer is replaced by the waiting note. */
    CONFIRMED,
}

/** The state the core's fold takes and returns. */
internal data class BehindState(val level: BehindLevel, val sinceMs: Long = 0L) {
    companion object {
        val IN_STEP: BehindState = BehindState(BehindLevel.IN_STEP, 0L)
    }
}

/** One read of a conversation journal (mirror of `FfiBehindObservation`). */
internal data class BehindObservation(
    val ownCommitOwed: Boolean,
    val drained: Boolean,
    val latestEpoch: ULong?,
    val localEpoch: ULong,
    val refusedReads: Int,
    val failedRowAboveLocal: Boolean,
    val publishRefused: Boolean,
)

/** The core's rule (`conversationBehindVerdict`), injected. */
internal fun interface BehindVerdict {
    fun fold(previous: BehindState, observation: BehindObservation, nowMs: Long): BehindState
}

/** The production [BehindVerdict]: the native fold. Unknown in a host test. */
internal object NativeBehindVerdict : BehindVerdict {
    override fun fold(previous: BehindState, observation: BehindObservation, nowMs: Long): BehindState {
        val prev = when (previous.level) {
            BehindLevel.IN_STEP -> uniffi.lakko_ffi_uniffi.FfiBehindState.InStep
            BehindLevel.SUSPECTED -> uniffi.lakko_ffi_uniffi.FfiBehindState.Suspected(previous.sinceMs.toULong())
            BehindLevel.CONFIRMED -> uniffi.lakko_ffi_uniffi.FfiBehindState.Confirmed(previous.sinceMs.toULong())
        }
        val next = uniffi.lakko_ffi_uniffi.conversationBehindVerdict(
            prev,
            uniffi.lakko_ffi_uniffi.FfiBehindObservation(
                ownCommitOwed = observation.ownCommitOwed,
                drained = observation.drained,
                latestEpoch = observation.latestEpoch,
                localEpoch = observation.localEpoch,
                refusedReads = observation.refusedReads.coerceAtLeast(0).toUInt(),
                failedRowAboveLocal = observation.failedRowAboveLocal,
                publishRefused = observation.publishRefused,
            ),
            nowMs.coerceAtLeast(0L).toULong(),
        )
        return when (next) {
            is uniffi.lakko_ffi_uniffi.FfiBehindState.InStep -> BehindState.IN_STEP
            is uniffi.lakko_ffi_uniffi.FfiBehindState.Suspected -> BehindState(BehindLevel.SUSPECTED, next.sinceMs.toLong())
            is uniffi.lakko_ffi_uniffi.FfiBehindState.Confirmed -> BehindState(BehindLevel.CONFIRMED, next.sinceMs.toLong())
        }
    }
}

/**
 * The `ownDeviceInConversation` the core's `reentryRoute` is asked with:
 * another own device holds a leaf in the conversation AND this device can
 * reach it (its DeviceGroup sync runs). Without that route 1 cannot send, and
 * the device would wait on it for ever (live walk 5, 2026-11-20).
 */
internal fun reentryOwnDeviceReachable(inConversation: Boolean, deviceGroupSyncActive: Boolean): Boolean =
    inConversation && deviceGroupSyncActive

/**
 * The core's request cadence (`reentryRequestDue`), injected.
 * [lastRouteWasOwnDevice]: the last request was sent to another own device
 * (live walk 5: it is followed 2 minutes later by the conversation's route).
 */
internal fun interface ReentryRequestDue {
    fun due(state: BehindState, lastSentMs: Long?, lastRouteWasOwnDevice: Boolean, nowMs: Long): Boolean
}

/** The production [ReentryRequestDue]. */
internal object NativeReentryRequestDue : ReentryRequestDue {
    override fun due(state: BehindState, lastSentMs: Long?, lastRouteWasOwnDevice: Boolean, nowMs: Long): Boolean {
        val s = when (state.level) {
            BehindLevel.IN_STEP -> uniffi.lakko_ffi_uniffi.FfiBehindState.InStep
            BehindLevel.SUSPECTED -> uniffi.lakko_ffi_uniffi.FfiBehindState.Suspected(state.sinceMs.toULong())
            BehindLevel.CONFIRMED -> uniffi.lakko_ffi_uniffi.FfiBehindState.Confirmed(state.sinceMs.toULong())
        }
        return uniffi.lakko_ffi_uniffi.reentryRequestDue(
            s,
            lastSentMs?.coerceAtLeast(0L)?.toULong(),
            lastRouteWasOwnDevice,
            nowMs.coerceAtLeast(0L).toULong(),
        )
    }
}

/**
 * What this device keeps per conversation. [latestEpoch] is the newest
 * `latest_epoch` a read named (bookkeeping only: since walk 3, W3-R1, a
 * request's `epoch_left` always comes from a fresh probe,
 * [probeReentryEpochLeft]), [gapFromMs] the
 * time of the first row this device could not read (the request's
 * `gap_from_ms` and the start of the gap line), [lastRequestSentMs] when the
 * last request went out, [lastRequestOwnDevice] whether that request was sent
 * to another own device, and [ownDeviceAsked] whether a request of this behind
 * episode was (live walk 5, 2026-11-20: from then on every request takes the
 * conversation's own route, `reentryRoute`'s `ownDeviceUnanswered`). The
 * three go with the episode when the device is back in step.
 */
@Serializable
internal data class ConversationBehindEntry(
    val level: BehindLevel = BehindLevel.IN_STEP,
    val sinceMs: Long = 0L,
    val refusedReads: Int = 0,
    val failedRowAboveLocal: Boolean = false,
    val publishRefused: Boolean = false,
    val gapFromMs: Long? = null,
    val latestEpoch: Long? = null,
    val lastRequestSentMs: Long? = null,
    val lastRequestOwnDevice: Boolean = false,
    val ownDeviceAsked: Boolean = false,
    /**
     * Walk 2, R1 — the DS time of the newest row this device applied in the
     * conversation, or of its own publish the DS accepted. Not gap
     * bookkeeping: it survives "back in step".
     */
    val lastKnownRowMs: Long? = null,
    /** Walk 2, R1 — local time of the last read of the journal that was not
     *  refused (written at most once a minute). Survives "back in step". */
    val lastReadOkMs: Long? = null,
) {
    val state: BehindState get() = BehindState(level, sinceMs)

    /** Nothing worth keeping on disk. */
    val isEmpty: Boolean
        get() = level == BehindLevel.IN_STEP && refusedReads == 0 && !failedRowAboveLocal &&
            !publishRefused && gapFromMs == null && lastRequestSentMs == null &&
            !lastRequestOwnDevice && !ownDeviceAsked &&
            lastKnownRowMs == null && lastReadOkMs == null

    /** What survives the conversation coming back in step: the read progress. */
    fun progressOnly(): ConversationBehindEntry =
        ConversationBehindEntry(lastKnownRowMs = lastKnownRowMs, lastReadOkMs = lastReadOkMs)
}

/**
 * Walk 2, defect 3 — after a refused read is folded, whether this device asks
 * to be brought back from the refused-read path: whenever it now stops acting.
 * With every read refused no poll applies, so the poll tail never runs;
 * whether a request is in fact due stays the core's rule.
 */
internal fun reentryRequestAfterRefusedRead(state: BehindState): Boolean = state.level != BehindLevel.IN_STEP

/**
 * Walk 3, W3-R1 — the `epoch_left` of a re-entry request: what [probe] (an
 * empty knock made right before minting, after the route is resolved) names.
 * Never a stored epoch: one kept from an earlier read goes stale while reads
 * are refused, and the re-admitter does not act on a request naming another
 * epoch. `null` when the probe failed or named none: nothing is minted,
 * recorded or sent, and the request is tried again shortly.
 */
internal suspend fun probeReentryEpochLeft(probe: suspend () -> ULong?): ULong? =
    try {
        probe()
    } catch (cause: kotlinx.coroutines.CancellationException) {
        throw cause
    } catch (_: Throwable) {
        null
    }

/**
 * Walk 3, W3-R2 — whether automatic sends into this conversation (repeat
 * envelopes, typing, read receipts, the committer beacon, the knock key, the
 * push routing secret, the own-device fan-out) are held: while the device is
 * suspected or confirmed behind, and while a read of the journal was refused
 * (an unskewed `401`) with no answered read since ([ConversationBehindTracker.observe]
 * resets the count). The person's own content is not decided here.
 */
internal fun automaticSendsHeld(e: ConversationBehindEntry): Boolean =
    e.level != BehindLevel.IN_STEP || e.refusedReads > 0

/**
 * Walk 3, W3-R5 — whether the person's own action (not a message) is held:
 * only while the device is suspected or confirmed behind. A refused read
 * alone does not hold it; it is attempted, and a `401` on it is evidence.
 */
internal fun personActionHeld(e: ConversationBehindEntry): Boolean = e.level != BehindLevel.IN_STEP

/**
 * Walk 3, W3-R5 — the refusal of a held person action. It carries no text of
 * its own, so each action shows its ordinary failure message
 * ([chatErrorText] falls back to the action's key).
 */
internal fun personActionHeldError(): ChatError =
    ChatError("this device may be behind in this conversation", IllegalStateException("behind"))

/** `lastReadOkMs` is written at most this often. Chosen, not measured. */
internal const val READ_OK_WRITE_INTERVAL_MS: Long = 60_000L

/**
 * Walk 2, R1 — where the gap starts: the first skipped row's time when there
 * is one, else (and never later than) the newest row this device knows it
 * applied, else the last read that was not refused, else when the suspicion
 * began, else [nowMs]. The request's `gap_from_ms` and the gap line both use
 * this value, so the re-admitter bundles the rows this device in fact lacks.
 */
internal fun reentryGapStart(e: ConversationBehindEntry, nowMs: Long): Long {
    val fallback = e.lastKnownRowMs ?: e.lastReadOkMs ?: e.sinceMs.takeIf { it > 0L } ?: nowMs
    val skipped = e.gapFromMs ?: return fallback
    return minOf(skipped, fallback)
}

/** Where [ConversationBehindTracker] keeps its entries. */
internal interface ConversationBehindStore {
    fun load(accountIdB64: String): Map<String, ConversationBehindEntry>

    /** `false` when the write did not land (or the account's stores are sealed). */
    fun save(accountIdB64: String, entries: Map<String, ConversationBehindEntry>): Boolean

    fun clearForAccount(accountIdB64: String)
}

/** The production store, behind the ADR-0077 gate. Holds no key and no text. */
internal class SharedPrefsConversationBehindStore(context: Context) : ConversationBehindStore {
    private val appContext = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }
    private val serializer = MapSerializer(String.serializer(), ConversationBehindEntry.serializer())

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String) = "v1:$accountIdB64"

    override fun load(accountIdB64: String): Map<String, ConversationBehindEntry> = try {
        prefs().getString(key(accountIdB64), null)
            ?.let { runCatching { json.decodeFromString(serializer, it) }.getOrNull() }
            .orEmpty()
    } catch (_: Throwable) {
        emptyMap()
    }

    override fun save(accountIdB64: String, entries: Map<String, ConversationBehindEntry>): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs().edit().putString(key(accountIdB64), json.encodeToString(serializer, entries)).commit()
        } ?: false

    override fun clearForAccount(accountIdB64: String) {
        prefs().edit().remove(key(accountIdB64)).commit()
    }

    internal companion object {
        const val PREFS_NAME = "lakko-conversation-behind"
    }
}

/** Consecutive refused reads that count as evidence of real progress
 *  (`reentryRefusedReadsToSuspect`, 3; read from the core in production). */
internal const val REENTRY_REFUSED_READS_DEFAULT: Int = 3

/**
 * Per conversation, what the reads showed, folded through the core's rule.
 * Process-wide per account: [bind] loads the account's entries, [unbind]
 * forgets them (Forget device clears the store separately). Thread-safe.
 */
internal class ConversationBehindTracker(
    private val store: ConversationBehindStore,
    private val verdict: BehindVerdict = NativeBehindVerdict,
    private val clock: () -> Long = System::currentTimeMillis,
    private val refusedReadsToSuspect: () -> Int = { REENTRY_REFUSED_READS_DEFAULT },
) {
    @Volatile
    private var account: String? = null
    private val entries = MutableStateFlow<Map<String, ConversationBehindEntry>>(emptyMap())

    /** Every conversation's entry, by group id hex (lower case). */
    val state: StateFlow<Map<String, ConversationBehindEntry>> = entries.asStateFlow()

    /**
     * Walk 3, W3-R5 — told the group id hex when a conversation goes from
     * behind (suspected or confirmed) back in step: by a read, or by the
     * re-entry joining ([cameBack]); and (W3-R6) when a read is answered
     * after refused reads that never made it behind. Called synchronously;
     * the listener launches its own work.
     */
    @Volatile
    var onBackInStep: ((groupIdHex: String) -> Unit)? = null

    private fun notifyBackInStep(groupIdHex: String) {
        try {
            onBackInStep?.invoke(groupIdHex.lowercase())
        } catch (_: Throwable) {
            // the listener's own failure is its own
        }
    }

    fun bind(accountIdB64: String) {
        if (account == accountIdB64) return
        account = accountIdB64
        entries.value = store.load(accountIdB64).mapKeys { it.key.lowercase() }
    }

    fun unbind() {
        account = null
        entries.value = emptyMap()
    }

    fun entry(groupIdHex: String): ConversationBehindEntry =
        entries.value[groupIdHex.lowercase()] ?: ConversationBehindEntry()

    /** The device stops acting in [groupIdHex] (`behindStateStopsActing`). */
    fun stopsActing(groupIdHex: String): Boolean = entry(groupIdHex).level != BehindLevel.IN_STEP

    /** Walk 3, W3-R2 — automatic sends into [groupIdHex] are held ([automaticSendsHeld]). */
    fun holdsAutomaticSends(groupIdHex: String): Boolean = automaticSendsHeld(entry(groupIdHex))

    /** Shown: the composer is replaced by the waiting note. */
    fun isConfirmed(groupIdHex: String): Boolean = entry(groupIdHex).level == BehindLevel.CONFIRMED

    /**
     * Evidence that the epochs above are real (section 1): a row framed above
     * the local epoch failed, a publish was refused, or the reads were refused.
     * ADR-0057 journal repair does not Replace while this holds (section 2).
     */
    fun realProgressEvidence(groupIdHex: String): Boolean {
        val e = entry(groupIdHex)
        return e.failedRowAboveLocal || e.publishRefused || e.refusedReads >= refusedReadsToSuspect()
    }

    /**
     * A row of [groupIdHex] was skipped (it could not be applied). [rowAtMs]
     * is the first skipped row's DS time; when it is unknown the gap start
     * falls back to the read progress ([reentryGapStart]), never to the
     * moment of the skip.
     */
    fun noteSkipped(groupIdHex: String, sealedEpoch: ULong?, localEpoch: ULong?, rowAtMs: Long?) {
        update(groupIdHex) { e ->
            val above = sealedEpoch != null && localEpoch != null && sealedEpoch > localEpoch
            e.copy(
                failedRowAboveLocal = e.failedRowAboveLocal || above,
                gapFromMs = e.gapFromMs ?: rowAtMs?.takeIf { it > 0L },
            )
        }
    }

    /**
     * Walk 2, R1 — a row of [groupIdHex] was applied, or this device's own
     * publish was accepted, at DS time [atMs]. Only ever moves forward.
     */
    fun noteRowKnown(groupIdHex: String, atMs: Long?) {
        val at = atMs?.takeIf { it > 0L } ?: return
        val known = entry(groupIdHex).lastKnownRowMs
        if (known != null && known >= at) return
        update(groupIdHex) { e -> if ((e.lastKnownRowMs ?: Long.MIN_VALUE) >= at) e else e.copy(lastKnownRowMs = at) }
    }

    /** Walk 2, R1 — a read of [groupIdHex] was answered (not refused). */
    fun noteReadOk(groupIdHex: String) {
        val now = clock()
        val last = entry(groupIdHex).lastReadOkMs
        // A clock that moved back writes once.
        if (last != null && now - last in 0 until READ_OK_WRITE_INTERVAL_MS) return
        update(groupIdHex) { it.copy(lastReadOkMs = now) }
    }

    /** Walk 2, R1 — the gap start of [groupIdHex] now ([reentryGapStart]). */
    fun gapStartMs(groupIdHex: String): Long = reentryGapStart(entry(groupIdHex), clock())

    /** A row of [groupIdHex] applied: "since the last applied row" restarts. */
    fun noteApplied(groupIdHex: String) {
        val e = entry(groupIdHex)
        if (!e.failedRowAboveLocal) return
        update(groupIdHex) { it.copy(failedRowAboveLocal = false) }
    }

    /** A publish of this device to [groupIdHex] was refused `401`, clock not skewed. */
    fun notePublishRefused(groupIdHex: String) {
        update(groupIdHex) { it.copy(publishRefused = true) }
    }

    /**
     * A signed read of [groupIdHex] was refused `401` with the clock not
     * skewed. Folded at once: three in a row suspect the device.
     */
    fun noteReadRefused(groupIdHex: String, ownCommitOwed: Boolean, localEpoch: ULong): BehindState {
        val bumped = update(groupIdHex) { it.copy(refusedReads = it.refusedReads + 1) }
        return fold(groupIdHex, bumped, ownCommitOwed, drained = false, latestEpoch = null, localEpoch = localEpoch)
    }

    /**
     * One read answered: fold it. [drained] means it returned no row and no
     * "more", and the cursor was not held.
     */
    fun observe(
        groupIdHex: String,
        ownCommitOwed: Boolean,
        drained: Boolean,
        latestEpoch: ULong?,
        localEpoch: ULong,
    ): BehindState {
        noteReadOk(groupIdHex)
        val before = entry(groupIdHex)
        // Walk 3, W3-R6 — a refused-read hold that ends without the device
        // ever having been behind: what it held is re-sent as on back-in-step.
        val holdEnding = before.refusedReads > 0 && before.level == BehindLevel.IN_STEP
        val reset = if (before.refusedReads != 0) update(groupIdHex) { it.copy(refusedReads = 0) } else before
        val next = fold(groupIdHex, reset, ownCommitOwed, drained, latestEpoch, localEpoch)
        if (holdEnding && next.level == BehindLevel.IN_STEP) notifyBackInStep(groupIdHex)
        // The request's `epoch_left`: kept only while the device stops acting
        // (an in-step device keeps no entry, so a poll writes nothing).
        if (next.level != BehindLevel.IN_STEP && latestEpoch != null) {
            val asLong = if (latestEpoch > Long.MAX_VALUE.toULong()) Long.MAX_VALUE else latestEpoch.toLong()
            if (entry(groupIdHex).latestEpoch != asLong) update(groupIdHex) { it.copy(latestEpoch = asLong) }
        }
        return next
    }

    private fun fold(
        groupIdHex: String,
        e: ConversationBehindEntry,
        ownCommitOwed: Boolean,
        drained: Boolean,
        latestEpoch: ULong?,
        localEpoch: ULong,
    ): BehindState {
        val next = verdict.fold(
            e.state,
            BehindObservation(
                ownCommitOwed = ownCommitOwed,
                drained = drained,
                latestEpoch = latestEpoch,
                localEpoch = localEpoch,
                refusedReads = e.refusedReads,
                failedRowAboveLocal = e.failedRowAboveLocal,
                publishRefused = e.publishRefused,
            ),
            clock(),
        )
        if (next.level == BehindLevel.IN_STEP && e.level != BehindLevel.IN_STEP) {
            // Cleared: what the suspicion rested on goes with it; the read
            // progress stays (walk 2, R1).
            replace(groupIdHex, entry(groupIdHex).progressOnly())
            reentryLog(false, "conversation back in step (local=$localEpoch)")
            notifyBackInStep(groupIdHex)
        } else if (next.level == BehindLevel.IN_STEP) {
            // In step and stays so: a drained read at or below the local epoch
            // also forgets a stale skip time (a garbage row skipped earlier).
            if (drained && latestEpoch != null && latestEpoch <= localEpoch && (e.gapFromMs != null || e.publishRefused)) {
                replace(groupIdHex, e.copy(gapFromMs = null, publishRefused = false, failedRowAboveLocal = false))
            }
        } else if (next != e.state) {
            update(groupIdHex) { it.copy(level = next.level, sinceMs = next.sinceMs) }
            reentryLog(
                true,
                "conversation behind: level=${next.level.name.lowercase()} local=$localEpoch " +
                    "latest=${latestEpoch?.toString() ?: "-"}",
            )
        }
        return next
    }

    /**
     * A request by route own-device could not be sent (live walk 5): the own
     * device is unavailable for this episode, as if asked and unanswered.
     */
    fun noteOwnDeviceUnavailable(groupIdHex: String) {
        update(groupIdHex) { it.copy(ownDeviceAsked = true) }
    }

    /**
     * A request to come back went out at [atMs] by [route], or (with no
     * [route]) one was not sent and [atMs] is the back-dated time that makes
     * it due again shortly on the 30-minute rule.
     */
    fun noteRequestSent(groupIdHex: String, atMs: Long, route: ReentryRouteKind? = null) {
        val ownDevice = route == ReentryRouteKind.OWN_DEVICE
        update(groupIdHex) {
            it.copy(
                lastRequestSentMs = atMs,
                lastRequestOwnDevice = ownDevice,
                ownDeviceAsked = it.ownDeviceAsked || ownDevice,
            )
        }
    }

    /**
     * The device came back by a Welcome at [atMs]: the suspicion goes, and
     * so does the newest known row (it applied none since joining); the last
     * good read is the join (walk 2, R1).
     */
    fun cameBack(groupIdHex: String, atMs: Long) {
        replace(groupIdHex, ConversationBehindEntry(lastReadOkMs = atMs))
        notifyBackInStep(groupIdHex)
    }

    /** The conversation went from this device (an end, a delete): everything
     *  about [groupIdHex] goes, the read progress included. */
    fun forget(groupIdHex: String) {
        replace(groupIdHex, ConversationBehindEntry())
    }

    private fun update(groupIdHex: String, change: (ConversationBehindEntry) -> ConversationBehindEntry): ConversationBehindEntry {
        val k = groupIdHex.lowercase()
        var result = ConversationBehindEntry()
        var changed = false
        entries.update { map ->
            val previous = map[k] ?: ConversationBehindEntry()
            result = change(previous)
            changed = result != previous
            if (result.isEmpty) map - k else map + (k to result)
        }
        // A poll of a conversation in step changes nothing and writes nothing.
        if (changed) persist()
        return result
    }

    private fun replace(groupIdHex: String, entry: ConversationBehindEntry) {
        update(groupIdHex) { entry }
    }

    private fun persist() {
        val acct = account ?: return
        try {
            if (!store.save(acct, entries.value)) {
                reentryLog(true, "conversation behind state not saved")
            }
        } catch (ended: me.lakko.app.StaleBindingException) {
            throw ended
        } catch (cause: Throwable) {
            reentryLog(true, "conversation behind state not saved (${cause.javaClass.simpleName})")
        }
    }
}

// ---------------------------------------------------------------------------
// The durable record of the requests this device sent
// ---------------------------------------------------------------------------

/** Which route a request took (mirror of `FfiReentryRoute`). */
@Serializable
internal enum class ReentryRouteKind { OWN_DEVICE, MAILBOX, KNOCK }

/** One request this device sent: its KeyPackage, for `Group.joinReentry`,
 *  and the public half of its gap key ([gapKeyB64], standard base64; the
 *  secret is in a [GapKeyStore]). */
@Serializable
internal data class ReentryRequestRow(
    val keyPackageB64: String,
    val epochLeft: Long,
    val gapFromMs: Long,
    val sentAtMs: Long,
    val route: ReentryRouteKind,
    val gapKeyB64: String = "",
)

/** The requests of one conversation, oldest first. */
@Serializable
internal data class ReentryRecord(val groupIdHex: String, val requests: List<ReentryRequestRow>)

/** The rows kept per conversation (`reentryRequestsKept`, 2). */
internal const val REENTRY_REQUESTS_KEPT_DEFAULT: Int = 2

/**
 * [record] with [row] appended and only the newest [kept] rows left: the
 * record to write and the rows that left it (whose KeyPackage private keys
 * the caller forgets, after the write landed).
 */
internal fun withReentryRequest(
    record: ReentryRecord?,
    groupIdHex: String,
    row: ReentryRequestRow,
    kept: Int = REENTRY_REQUESTS_KEPT_DEFAULT,
): Pair<ReentryRecord, List<ReentryRequestRow>> {
    val all = (record?.requests.orEmpty()) + row
    val keep = all.takeLast(kept.coerceAtLeast(1))
    val dropped = all.dropLast(keep.size)
    return ReentryRecord(groupIdHex.lowercase(), keep) to dropped
}

/** Where the re-entry records live. */
internal interface ReentryRecordStore {
    fun load(accountIdB64: String): Map<String, ReentryRecord>

    fun get(accountIdB64: String, groupIdHex: String): ReentryRecord? = load(accountIdB64)[groupIdHex.lowercase()]

    /** `false` when the row did not reach the disk: the request is then not sent. */
    fun put(accountIdB64: String, record: ReentryRecord): Boolean

    fun remove(accountIdB64: String, groupIdHex: String): Boolean

    fun clearForAccount(accountIdB64: String)
}

/** The production store, behind the ADR-0077 gate. A KeyPackage is public. */
internal class SharedPrefsReentryRecordStore(context: Context) : ReentryRecordStore {
    private val appContext = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }
    private val serializer = MapSerializer(String.serializer(), ReentryRecord.serializer())

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String) = "v1:$accountIdB64"

    override fun load(accountIdB64: String): Map<String, ReentryRecord> = try {
        prefs().getString(key(accountIdB64), null)
            ?.let { runCatching { json.decodeFromString(serializer, it) }.getOrNull() }
            .orEmpty()
    } catch (_: Throwable) {
        emptyMap()
    }

    private fun write(accountIdB64: String, rows: Map<String, ReentryRecord>): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs().edit().putString(key(accountIdB64), json.encodeToString(serializer, rows)).commit()
        } ?: false

    override fun put(accountIdB64: String, record: ReentryRecord): Boolean = synchronized(LOCK) {
        write(accountIdB64, load(accountIdB64) + (record.groupIdHex.lowercase() to record))
    }

    override fun remove(accountIdB64: String, groupIdHex: String): Boolean = synchronized(LOCK) {
        write(accountIdB64, load(accountIdB64) - groupIdHex.lowercase())
    }

    override fun clearForAccount(accountIdB64: String) {
        prefs().edit().remove(key(accountIdB64)).commit()
    }

    internal companion object {
        const val PREFS_NAME = "lakko-reentry-records"
        private val LOCK = Any()
    }
}

// ---------------------------------------------------------------------------
// The re-admitter's facts
// ---------------------------------------------------------------------------

/** What the core's `reentryAdmissionVerdict` answered (mirror). */
internal enum class ReentryAdmission { ADMIT, NOT_THE_ACCOUNTS_DEVICE, LEAF_IS_WRITING, ALREADY_HANDLED, TOO_SOON }

/** The facts handed to the verdict (mirror of `FfiReentryAdmissionFacts`). */
internal data class ReentryAdmissionFacts(
    val deviceStillTheAccounts: Boolean,
    val lastCurrentEpochMessageFromLeafMs: Long?,
    val handledForLeafAtThisEpoch: Boolean,
    val lastAcceptedForLeafMs: Long?,
)

/** The core's rule, injected. */
internal fun interface ReentryAdmissionRule {
    fun verdict(facts: ReentryAdmissionFacts, nowMs: Long): ReentryAdmission
}

/** The production [ReentryAdmissionRule]. */
internal object NativeReentryAdmissionRule : ReentryAdmissionRule {
    override fun verdict(facts: ReentryAdmissionFacts, nowMs: Long): ReentryAdmission {
        val answer = uniffi.lakko_ffi_uniffi.reentryAdmissionVerdict(
            uniffi.lakko_ffi_uniffi.FfiReentryAdmissionFacts(
                deviceStillTheAccounts = facts.deviceStillTheAccounts,
                lastCurrentEpochMessageFromLeafMs = facts.lastCurrentEpochMessageFromLeafMs?.coerceAtLeast(0L)?.toULong(),
                handledForLeafAtThisEpoch = facts.handledForLeafAtThisEpoch,
                lastAcceptedForLeafMs = facts.lastAcceptedForLeafMs?.coerceAtLeast(0L)?.toULong(),
            ),
            nowMs.coerceAtLeast(0L).toULong(),
        )
        return when (answer) {
            uniffi.lakko_ffi_uniffi.FfiReentryAdmission.ADMIT -> ReentryAdmission.ADMIT
            uniffi.lakko_ffi_uniffi.FfiReentryAdmission.NOT_THE_ACCOUNTS_DEVICE -> ReentryAdmission.NOT_THE_ACCOUNTS_DEVICE
            uniffi.lakko_ffi_uniffi.FfiReentryAdmission.LEAF_IS_WRITING -> ReentryAdmission.LEAF_IS_WRITING
            uniffi.lakko_ffi_uniffi.FfiReentryAdmission.ALREADY_HANDLED -> ReentryAdmission.ALREADY_HANDLED
            uniffi.lakko_ffi_uniffi.FfiReentryAdmission.TOO_SOON -> ReentryAdmission.TOO_SOON
        }
    }
}

/**
 * Per (conversation, leaf signature key): when this device last read an
 * application message from the leaf at the current epoch, which epochs a
 * re-entry was handled at, and when one was last accepted. In memory: a
 * restart forgets it, which at most allows one more re-entry of a leaf
 * within the hour (each still needs a valid request).
 */
internal class ReentryAdmissionLedger(private val clock: () -> Long = System::currentTimeMillis) {
    private data class Seen(val epoch: ULong, val atMs: Long)

    private val lastMessage = ConcurrentHashMap<String, Seen>()
    private val handled = ConcurrentHashMap<String, MutableSet<ULong>>()
    private val accepted = ConcurrentHashMap<String, Long>()

    private fun key(groupIdHex: String, leafKeyHex: String) = groupIdHex.lowercase() + ":" + leafKeyHex.lowercase()

    /** An application message from [leafKeyHex] sealed at [epoch] was read. */
    fun noteMessage(groupIdHex: String, leafKeyHex: String, epoch: ULong, atMs: Long = clock()) {
        lastMessage[key(groupIdHex, leafKeyHex)] = Seen(epoch, atMs)
    }

    /** The time of the last message read from the leaf, at any epoch: the
     *  re-admitter's `leafLastWriteMs` (section 7). */
    fun lastMessageAtAnyEpoch(groupIdHex: String, leafKeyHex: String): Long? =
        lastMessage[key(groupIdHex, leafKeyHex)]?.atMs

    /** The time of the last message read from the leaf at [currentEpoch]. */
    fun lastMessageAt(groupIdHex: String, leafKeyHex: String, currentEpoch: ULong): Long? =
        lastMessage[key(groupIdHex, leafKeyHex)]?.takeIf { it.epoch == currentEpoch }?.atMs

    fun handledAt(groupIdHex: String, leafKeyHex: String, epoch: ULong): Boolean =
        handled[key(groupIdHex, leafKeyHex)]?.contains(epoch) == true

    fun noteHandled(groupIdHex: String, leafKeyHex: String, epoch: ULong) {
        handled.getOrPut(key(groupIdHex, leafKeyHex)) { ConcurrentHashMap.newKeySet() }.add(epoch)
    }

    fun lastAccepted(groupIdHex: String, leafKeyHex: String): Long? = accepted[key(groupIdHex, leafKeyHex)]

    fun noteAccepted(groupIdHex: String, leafKeyHex: String, atMs: Long = clock()) {
        accepted[key(groupIdHex, leafKeyHex)] = atMs
    }

    /** The facts for one request. */
    fun facts(groupIdHex: String, leafKeyHex: String, epoch: ULong, deviceStillTheAccounts: Boolean) =
        ReentryAdmissionFacts(
            deviceStillTheAccounts = deviceStillTheAccounts,
            lastCurrentEpochMessageFromLeafMs = lastMessageAt(groupIdHex, leafKeyHex, epoch),
            handledForLeafAtThisEpoch = handledAt(groupIdHex, leafKeyHex, epoch),
            lastAcceptedForLeafMs = lastAccepted(groupIdHex, leafKeyHex),
        )

    fun clear() {
        lastMessage.clear()
        handled.clear()
        accepted.clear()
    }
}

/**
 * Why a request was not acted on; for the identifier-free log line only.
 * [ReentryRequestCheck.check] returns `null` when every client-side check
 * passed (the core's verdict then decides).
 */
internal enum class ReentryRequestSkip {
    /** This device does not hold the conversation. */
    NOT_HELD,

    /** This device is itself behind there: it acts on nothing. */
    SELF_BEHIND,

    /** The request names another epoch than this device's. */
    OTHER_EPOCH,
}

/** The pre-checks a re-admitter runs before the core's checks (section 4). */
internal object ReentryRequestCheck {
    fun check(held: Boolean, selfBehind: Boolean, localEpoch: ULong?, epochLeft: ULong): ReentryRequestSkip? = when {
        !held || localEpoch == null -> ReentryRequestSkip.NOT_HELD
        selfBehind -> ReentryRequestSkip.SELF_BEHIND
        localEpoch != epochLeft -> ReentryRequestSkip.OTHER_EPOCH
        else -> null
    }
}

/**
 * The re-admitter's route-2/3 device check (section 4): the account's
 * directory entry lists a device with the leaf's signature key. A directory
 * that could not be read, or an account this device cannot name, is `false`:
 * fail closed, the request waits for its re-send.
 */
internal fun directoryListsDevice(listedDeviceKeysHex: List<String>?, leafKeyHex: String): Boolean =
    listedDeviceKeysHex?.any { it.equals(leafKeyHex, ignoreCase = true) } == true

// ---------------------------------------------------------------------------
// The returning device: Welcome handling
// ---------------------------------------------------------------------------

/**
 * A Welcome the core refused because this device holds the group as active:
 * is it to be tried as a re-entry? Only while a re-entry record exists for
 * that group (section 5).
 */
internal fun tryAsReentry(stillActiveGroupHex: String?, recordedGroups: Set<String>): Boolean =
    stillActiveGroupHex != null && stillActiveGroupHex.lowercase() in recordedGroups

// ---------------------------------------------------------------------------
// The knock key a member holds (route 3)
// ---------------------------------------------------------------------------

/** One group's knock public key, as the owner handed it in-group. */
@Serializable
internal data class HeldKnockKey(val version: Int, val publicKeyB64: String)

/**
 * The receipt rule: kept only from the owner or an admin resolved at
 * receipt ([senderIsOwnerOrAdmin]), and only when strictly newer
 * ([isNewer], the core's `knockKeyIsNewer`).
 */
internal fun keepKnockKey(
    senderIsOwnerOrAdmin: Boolean,
    incomingVersion: Int,
    stored: HeldKnockKey?,
    isNewer: (incoming: Int, stored: Int?) -> Boolean,
): Boolean = senderIsOwnerOrAdmin && isNewer(incomingVersion, stored?.version)

/** Where the held knock keys live (a public key; no secret). */
internal interface KnockKeyStore {
    fun get(accountIdB64: String, groupIdHex: String): HeldKnockKey?

    fun put(accountIdB64: String, groupIdHex: String, key: HeldKnockKey): Boolean

    fun remove(accountIdB64: String, groupIdHex: String)
}

/** The production store, behind the ADR-0077 gate. */
internal class SharedPrefsKnockKeyStore(context: Context) : KnockKeyStore {
    private val appContext = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String, groupIdHex: String) = "v1:$accountIdB64:${groupIdHex.lowercase()}"

    override fun get(accountIdB64: String, groupIdHex: String): HeldKnockKey? = try {
        prefs().getString(key(accountIdB64, groupIdHex), null)
            ?.let { runCatching { json.decodeFromString(HeldKnockKey.serializer(), it) }.getOrNull() }
    } catch (_: Throwable) {
        null
    }

    override fun put(accountIdB64: String, groupIdHex: String, key: HeldKnockKey): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs().edit().putString(key(accountIdB64, groupIdHex), json.encodeToString(HeldKnockKey.serializer(), key)).commit()
        } ?: false

    override fun remove(accountIdB64: String, groupIdHex: String) {
        prefs().edit().remove(key(accountIdB64, groupIdHex)).commit()
    }

    /** Forget device: every group of [accountIdB64]. */
    fun clearForAccount(accountIdB64: String) {
        val prefix = "v1:$accountIdB64:"
        val editor = prefs().edit()
        for (k in prefs().all.keys) if (k.startsWith(prefix)) editor.remove(k)
        editor.commit()
    }

    internal companion object {
        const val PREFS_NAME = "lakko-knock-keys"
    }
}

/**
 * Who sends the knock key in-group: the owner always (on a new version and
 * on growth); after bringing a device back, also an admin, who holds the
 * knock seeds too, so the returning device has the key at once whoever
 * re-admitted it.
 */
internal fun knockKeySender(isOwner: Boolean, isAdmin: Boolean, afterReentry: Boolean): Boolean =
    isOwner || (afterReentry && isAdmin)

/**
 * What the owner last sent its knock key with, successfully: the key's
 * version and the group's members (leaf signature keys, hex) at that send.
 */
internal data class KnockKeySentMark(val version: Int, val members: Set<String>)

/**
 * Walk 2, R4 — whether the owner (re)sends its knock key in-group: on a
 * version it has not sent, and while the group holds a member that was not in
 * it at the last successful send (a newcomer holds none). Judged against the
 * members themselves, never a "grew" flag of one poll: a growth seen while a
 * send was skipped or failed keeps the send due. [mark] is set only after a
 * successful broadcast, and cleared at re-entry.
 */
internal fun knockKeyResendDue(mark: KnockKeySentMark?, currentVersion: Int, currentMembers: Set<String>): Boolean =
    mark == null || mark.version != currentVersion || !mark.members.containsAll(currentMembers)

// ---------------------------------------------------------------------------
// The gap line (`docs/ux/conversation-rejoin.md` 2.4)
// ---------------------------------------------------------------------------

/**
 * One gap of a conversation: from the first row this device could not read
 * to the moment it came back. [anchorMessageId] places the line in the
 * timeline (the first row id the device held after coming back, or `0`
 * before one is known: the line then sits at the end).
 */
@Serializable
internal data class ReentryGap(
    val fromMs: Long,
    val toMs: Long,
    val anchorMessageId: Long = 0L,
)

/** Gaps kept per conversation. Chosen, not measured. */
internal const val MAX_REENTRY_GAPS: Int = 8

/** [gaps] with a new one, the oldest gone past [MAX_REENTRY_GAPS]. */
internal fun withGap(gaps: List<ReentryGap>, gap: ReentryGap): List<ReentryGap> {
    if (gap.toMs <= gap.fromMs) return gaps
    return (gaps + gap).takeLast(MAX_REENTRY_GAPS)
}

/**
 * The gap left once the bringing-back device's history bundle landed:
 * `null` when the bundle carried rows from the gap's start (nothing is
 * missing as far as this device can tell), else the gap up to the first
 * bundled row. [bundledRowTimesMs] are the bundle rows' times inside the gap.
 * [capped]: the bundle was cut to its size cap, so the older rows are missing.
 */
internal fun gapAfterBundle(gap: ReentryGap, bundledRowTimesMs: List<Long>, capped: Boolean): ReentryGap? {
    val inside = bundledRowTimesMs.filter { it in gap.fromMs..gap.toMs }
    if (inside.isEmpty()) return if (capped) gap else null
    if (!capped) return null
    val first = inside.min()
    return if (first <= gap.fromMs) null else gap.copy(toMs = first)
}

/** Where the gap lines live (times only). */
internal interface ReentryGapStore {
    fun load(accountIdB64: String, groupIdHex: String): List<ReentryGap>

    fun save(accountIdB64: String, groupIdHex: String, gaps: List<ReentryGap>): Boolean

    fun delete(accountIdB64: String, groupIdHex: String)
}

/** The production store, behind the ADR-0077 gate. */
internal class SharedPrefsReentryGapStore(context: Context) : ReentryGapStore {
    private val appContext = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }
    private val serializer = kotlinx.serialization.builtins.ListSerializer(ReentryGap.serializer())

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String, groupIdHex: String) = "v1:$accountIdB64:${groupIdHex.lowercase()}"

    override fun load(accountIdB64: String, groupIdHex: String): List<ReentryGap> = try {
        prefs().getString(key(accountIdB64, groupIdHex), null)
            ?.let { runCatching { json.decodeFromString(serializer, it) }.getOrNull() }
            .orEmpty()
    } catch (_: Throwable) {
        emptyList()
    }

    override fun save(accountIdB64: String, groupIdHex: String, gaps: List<ReentryGap>): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val editor = prefs().edit()
            if (gaps.isEmpty()) {
                editor.remove(key(accountIdB64, groupIdHex))
            } else {
                editor.putString(key(accountIdB64, groupIdHex), json.encodeToString(serializer, gaps))
            }
            editor.commit()
        } ?: false

    override fun delete(accountIdB64: String, groupIdHex: String) {
        prefs().edit().remove(key(accountIdB64, groupIdHex)).commit()
    }

    /** Forget device: every group of [accountIdB64]. */
    fun clearForAccount(accountIdB64: String) {
        val prefix = "v1:$accountIdB64:"
        val editor = prefs().edit()
        for (k in prefs().all.keys) if (k.startsWith(prefix)) editor.remove(k)
        editor.commit()
    }

    internal companion object {
        const val PREFS_NAME = "lakko-reentry-gaps"
    }
}

/**
 * The gap line's text (`chat.reentry.gap`, UX 2.4): a plain fact, no times
 * and no mention of this device (maintainer, 2026-11-20). The line sits in
 * the timeline where the missing messages were.
 */
internal fun gapLineText(): me.lakko.app.l10n.UiText = me.lakko.app.l10n.UiText("chat.reentry.gap")

// ---------------------------------------------------------------------------
// The history of the gap (section 7)
// ---------------------------------------------------------------------------

/**
 * A gap bundle the re-admitter still owes (section 7): the rows from the
 * returning leaf's gap on, sealed to the request's [gapKey] (the public half
 * the returning device put in its request) and posted to
 * `gapHistoryRendezvous(gapKey)`. [leafSignatureKey] names the returning leaf
 * (for when this device first saw it and last read from it); [gapFromMs] is
 * the request's claim, clamped by `gapHistoryStart` at send time;
 * [epochLeft] is the epoch the request named, so the bundle goes once the
 * group moved past it (the replacement landed). Every field is public (no
 * secret): kept durably in an [OwedGapBundleStore], so a restart between the
 * admission and the merge of the replacement does not lose it.
 */
internal data class OwedGapBundle(
    val groupIdHex: String,
    val leafSignatureKey: ByteArray,
    val epochLeft: ULong,
    val gapFromMs: Long,
    val owedSinceMs: Long,
    val gapKey: ByteArray,
)

/** What to do with an owed bundle now that the group is at [epoch]. */
internal enum class OwedBundleStep { WAIT, SEND, DROP }

/**
 * Give up on an owed bundle after 30 days: the returning device looks for
 * it as long ([GAP_HISTORY_LOOK_RETENTION_MS]), which is the DS retention.
 * Chosen, not measured.
 */
internal const val GAP_OWED_TTL_MS: Long = 30L * 24L * 60L * 60L * 1_000L

/**
 * The bundle is sealed to the request's gap key, not to an epoch secret, so
 * it is sent at any epoch past the one the request named: the replacement
 * landed (a replacement that lost to another commit costs nothing, since only
 * the returning device can open it).
 */
internal fun owedBundleStep(owed: OwedGapBundle, epoch: ULong?, nowMs: Long): OwedBundleStep = when {
    nowMs - owed.owedSinceMs > GAP_OWED_TTL_MS -> OwedBundleStep.DROP
    owed.gapKey.size != GAP_KEY_LEN -> OwedBundleStep.DROP
    epoch == null -> OwedBundleStep.WAIT
    epoch > owed.epochLeft -> OwedBundleStep.SEND
    else -> OwedBundleStep.WAIT
}

/** Length of a gap key, either half. */
internal const val GAP_KEY_LEN: Int = 32

/**
 * Keep an opened gap history only when its signer is one the joined group
 * accepts (`Group.gapHistorySignerAccepted`: the own account, the owner or an
 * admin, or in a one-to-one the other person). Anything else is dropped and
 * the gap line stays.
 */
internal fun <B> acceptedGapHistory(signerLeafKey: ByteArray, bundle: B, signerAccepted: (ByteArray) -> Boolean): B? =
    try {
        if (signerAccepted(signerLeafKey)) bundle else null
    } catch (_: Throwable) {
        null
    }

/**
 * A gap bundle row's direction on this device. A placeholder carries no
 * author, so it is read from the sender's: the same account keeps it, a
 * one-to-one's other person swaps it, a group's re-admitter reads as
 * incoming (its own rows are someone else's here, and its incoming ones
 * may be anyone's). A system row stays one.
 */
internal fun reentryBundleDirection(route: ReentryRouteKind, senderDirection: String): String = when {
    senderDirection == "system" -> "system"
    route == ReentryRouteKind.OWN_DEVICE -> senderDirection
    route == ReentryRouteKind.MAILBOX -> if (senderDirection == "outgoing") "incoming" else "outgoing"
    else -> "incoming"
}

/**
 * Whether a gap bundle row is taken in at all. A call row (placeholder
 * version 7 carries the call) records the SENDER's own call: from a device of
 * this account ([ReentryRouteKind.OWN_DEVICE]) it is this account's call and
 * is kept; from anyone else's device it describes their call, not this
 * person's ("Outgoing call" for a call this person never placed, with a call
 * back button), so it is left out. Every other row is kept.
 */
internal fun reentryBundleKeepsRow(row: PersistedMessage, route: ReentryRouteKind): Boolean =
    row.call == null || route == ReentryRouteKind.OWN_DEVICE

/**
 * Walk 3, W3-R3 — a gap bundle row as this device keeps it. A placeholder
 * that names its author ([authorKeyB64], base64) gives the row that author
 * and the direction it has here: outgoing when the author is this account's
 * own key in the conversation ([ownKeyB64]), else incoming, so the row
 * renders with the author's name and avatar exactly like a live one. With
 * no author the direction is read from the sender's ([reentryBundleDirection]).
 * A system row stays one and names nobody.
 */
internal fun reentryBundleRow(
    row: PersistedMessage,
    authorKeyB64: String?,
    ownKeyB64: String?,
    route: ReentryRouteKind,
): PersistedMessage = reentryBundleRow(row, authorKeyB64, listOfNotNull(ownKeyB64), route)

/**
 * [reentryBundleRow] over every key that names this account ([ownKeysB64]):
 * its key in the conversation now and, after an ADR-0101 key reset, the
 * previous account key its earlier messages were authored under (live walk
 * 2026-11-26, D2: a returned one-to-one drew the person's own earlier
 * messages as received). Pure.
 */
internal fun reentryBundleRow(
    row: PersistedMessage,
    authorKeyB64: String?,
    ownKeysB64: Collection<String>,
    route: ReentryRouteKind,
): PersistedMessage = when {
    row.direction == "system" -> row.copy(authorKeyB64 = null)
    authorKeyB64 != null -> row.copy(
        authorKeyB64 = authorKeyB64,
        direction = if (ownKeysB64.any { sameKeyB64(authorKeyB64, it) }) "outgoing" else "incoming",
    )
    else -> row.copy(authorKeyB64 = null, direction = reentryBundleDirection(route, row.direction))
}

/** Two standard-base64 keys name the same bytes. */
private fun sameKeyB64(a: String, b: String): Boolean {
    if (a == b) return true
    return try {
        java.util.Base64.getDecoder().decode(a).contentEquals(java.util.Base64.getDecoder().decode(b))
    } catch (_: IllegalArgumentException) {
        false
    }
}

/**
 * Whether a gap bundle is marked as not holding everything of the gap
 * (`countCapped`): the cap trimmed it, or the sender holds no row older than
 * [fromMs]. Live walk 2026-11-19: the same rule as web and iOS, so a
 * receiver keeps its gap line when the sender was away for part of it.
 */
internal fun gapBundleIncomplete(
    rows: List<PersistedMessage>,
    fromMs: Long,
    cut: Boolean,
    timeOf: (PersistedMessage) -> Long?,
): Boolean = cut || rows.none { (timeOf(it) ?: Long.MAX_VALUE) < fromMs }

/** The rows of [rows] a gap bundle carries: from [fromMs] on, at most [cap]. */
internal fun gapRows(rows: List<PersistedMessage>, fromMs: Long, cap: Int, timeOf: (PersistedMessage) -> Long?): Pair<List<PersistedMessage>, Boolean> {
    val inGap = rows.filter { (timeOf(it) ?: 0L) >= fromMs }
    val kept = inGap.takeLast(cap.coerceAtLeast(1))
    return kept to (kept.size < inGap.size)
}

/** The key an owed bundle is filed under: one per (group, returning leaf). */
internal fun owedGapBundleKey(groupIdHex: String, leafKeyHex: String): String =
    groupIdHex.lowercase() + ":" + leafKeyHex.lowercase()

/** The persisted form of an [OwedGapBundle]. [epochLeft] is a decimal
 *  string, since a `ULong` does not fit a JSON number. */
@Serializable
internal data class OwedGapBundleRow(
    val groupIdHex: String,
    val leafSignatureKeyB64: String,
    val epochLeft: String,
    val gapFromMs: Long,
    val owedSinceMs: Long,
    val gapKeyB64: String,
)

internal fun OwedGapBundle.toRow(): OwedGapBundleRow = OwedGapBundleRow(
    groupIdHex = groupIdHex.lowercase(),
    leafSignatureKeyB64 = java.util.Base64.getEncoder().encodeToString(leafSignatureKey),
    epochLeft = epochLeft.toString(),
    gapFromMs = gapFromMs,
    owedSinceMs = owedSinceMs,
    gapKeyB64 = java.util.Base64.getEncoder().encodeToString(gapKey),
)

/** `null` for a row that does not decode, or whose gap key is not 32 bytes
 *  (dropped, never acted on). */
internal fun OwedGapBundleRow.toOwed(): OwedGapBundle? = try {
    val gap = java.util.Base64.getDecoder().decode(gapKeyB64)
    if (gap.size != GAP_KEY_LEN) {
        null
    } else {
        OwedGapBundle(
            groupIdHex = groupIdHex.lowercase(),
            leafSignatureKey = java.util.Base64.getDecoder().decode(leafSignatureKeyB64),
            epochLeft = epochLeft.toULong(),
            gapFromMs = gapFromMs,
            owedSinceMs = owedSinceMs,
            gapKey = gap,
        )
    }
} catch (_: IllegalArgumentException) {
    null
}

/** Where the re-admitter's owed bundles live (section 7). */
internal interface OwedGapBundleStore {
    fun load(accountIdB64: String): Map<String, OwedGapBundle>

    /** `false` when the row did not reach the disk. */
    fun put(accountIdB64: String, key: String, bundle: OwedGapBundle): Boolean

    /**
     * Remove [key] and return what it held, or `null` when it was gone: the
     * one claim a SEND or a DROP makes, so two poll paths running at once
     * act on a bundle once.
     */
    fun take(accountIdB64: String, key: String): OwedGapBundle?

    /** The group left this device: every bundle owed into it. */
    fun removeGroup(accountIdB64: String, groupIdHex: String)

    fun clearForAccount(accountIdB64: String)
}

/** The production [OwedGapBundleStore], behind the ADR-0077 gate. Survives
 *  a sign-out (the bundle is still owed when the account lands again); goes
 *  on Forget device and with the group. */
internal class SharedPrefsOwedGapBundleStore(context: Context) : OwedGapBundleStore {
    private val appContext = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }
    private val serializer = MapSerializer(String.serializer(), OwedGapBundleRow.serializer())

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String) = "v1:$accountIdB64"

    private fun rows(accountIdB64: String): Map<String, OwedGapBundleRow> = try {
        prefs().getString(key(accountIdB64), null)
            ?.let { runCatching { json.decodeFromString(serializer, it) }.getOrNull() }
            .orEmpty()
    } catch (_: Throwable) {
        emptyMap()
    }

    private fun write(accountIdB64: String, rows: Map<String, OwedGapBundleRow>): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val editor = prefs().edit()
            if (rows.isEmpty()) {
                editor.remove(key(accountIdB64))
            } else {
                editor.putString(key(accountIdB64), json.encodeToString(serializer, rows))
            }
            editor.commit()
        } ?: false

    override fun load(accountIdB64: String): Map<String, OwedGapBundle> = synchronized(LOCK) {
        rows(accountIdB64).mapNotNull { (k, row) -> row.toOwed()?.let { k to it } }.toMap()
    }

    override fun put(accountIdB64: String, key: String, bundle: OwedGapBundle): Boolean = synchronized(LOCK) {
        write(accountIdB64, rows(accountIdB64) + (key to bundle.toRow()))
    }

    override fun take(accountIdB64: String, key: String): OwedGapBundle? = synchronized(LOCK) {
        val all = rows(accountIdB64)
        val row = all[key] ?: return null
        if (!write(accountIdB64, all - key)) return null
        row.toOwed()
    }

    override fun removeGroup(accountIdB64: String, groupIdHex: String) {
        synchronized(LOCK) {
            val hex = groupIdHex.lowercase()
            val all = rows(accountIdB64)
            val kept = all.filterValues { it.groupIdHex.lowercase() != hex }
            if (kept.size != all.size) write(accountIdB64, kept)
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        synchronized(LOCK) { prefs().edit().remove(key(accountIdB64)).commit() }
    }

    internal companion object {
        const val PREFS_NAME = "lakko-reentry-owed-bundles"
        private val LOCK = Any()
    }
}

/**
 * Who re-sends the push routing secret after bringing a device back: the
 * group's owner or an admin, as for an ordinary growth. A plain member (a
 * sibling device that is a plain member, the other person of a one-to-one
 * with no role) leaves it to them; the receive gate is unchanged.
 */
internal fun reentryResendsPushRoutingSecret(isOwner: Boolean, isAdmin: Boolean): Boolean = isOwner || isAdmin

// ---------------------------------------------------------------------------
// The returning device's look for the gap bundle (section 7)
// ---------------------------------------------------------------------------

/** At most one look per conversation per minute. Chosen, not measured. */
internal const val GAP_HISTORY_LOOK_INTERVAL_MS: Long = 60_000L

/** A gap look is kept 30 days from the moment the device came back (the DS
 *  retention): past it no bundle can still be read, and the gap line stays.
 *  Chosen, not measured. */
internal const val GAP_HISTORY_LOOK_RETENTION_MS: Long = 30L * 24L * 60L * 60L * 1_000L

/** One gap key: the public half names the rendezvous, the secret opens
 *  what is posted there. Standard base64. */
@Serializable
internal data class GapKeyEntry(val publicKeyB64: String, val secretB64: String) {
    /** A fresh copy of the secret; the caller zeroes it after use. */
    fun secret(): ByteArray = java.util.Base64.getDecoder().decode(secretB64)

    fun publicKey(): ByteArray = java.util.Base64.getDecoder().decode(publicKeyB64)

    override fun toString(): String = "GapKeyEntry(publicKeyB64=$publicKeyB64)"
}

/**
 * One conversation whose gap bundle this device has not read yet. [keys] are
 * the gap keys of the requests it held when it came back (it cannot know which
 * one the re-admitter answered): a [GapLookStore] keeps them sealed at rest.
 * [gapFromMs] and [gapToMs] name the gap line it narrows; [lastLookMs] (0
 * before the first look) throttles the looks; [route] decides the bundle
 * rows' direction.
 */
@Serializable
internal data class GapLookRecord(
    val groupIdHex: String,
    val keys: List<GapKeyEntry>,
    val gapFromMs: Long,
    val gapToMs: Long,
    val lastLookMs: Long = 0L,
    val route: ReentryRouteKind,
) {
    override fun toString(): String =
        "GapLookRecord(keys=${keys.size}, gapFromMs=$gapFromMs, gapToMs=$gapToMs, lastLookMs=$lastLookMs, route=$route)"
}

/** What one pass of the gap look decided for a record. */
internal enum class GapLookStep { LOOK, WAIT, DROP }

internal fun gapLookStep(record: GapLookRecord, nowMs: Long): GapLookStep {
    if (nowMs - record.gapToMs > GAP_HISTORY_LOOK_RETENTION_MS) return GapLookStep.DROP
    val since = nowMs - record.lastLookMs
    // A clock that moved back reads as due: one look, then throttled again.
    if (record.lastLookMs > 0L && since in 0 until GAP_HISTORY_LOOK_INTERVAL_MS) return GapLookStep.WAIT
    return GapLookStep.LOOK
}

/** Where the gap looks live. The production store seals the whole row
 *  (secret included) under the device-held key. */
internal interface GapLookStore {
    fun load(accountIdB64: String): Map<String, GapLookRecord>

    fun get(accountIdB64: String, groupIdHex: String): GapLookRecord? = load(accountIdB64)[groupIdHex.lowercase()]

    /** `false` when the row did not reach the disk. */
    fun put(accountIdB64: String, record: GapLookRecord): Boolean

    fun remove(accountIdB64: String, groupIdHex: String)

    fun clearForAccount(accountIdB64: String)
}

/** The production [GapLookStore]: EncryptedSharedPreferences (the Android
 *  Keystore key), behind the ADR-0077 gate. */
internal class EncryptedGapLookStore(context: Context) : GapLookStore {
    private val app = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }
    private val serializer = MapSerializer(String.serializer(), GapLookRecord.serializer())

    private val prefs: SharedPreferences by lazy {
        val master = androidx.security.crypto.MasterKey.Builder(app)
            .setKeyScheme(androidx.security.crypto.MasterKey.KeyScheme.AES256_GCM)
            .build()
        androidx.security.crypto.EncryptedSharedPreferences.create(
            app,
            PREFS_NAME,
            master,
            androidx.security.crypto.EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            androidx.security.crypto.EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    private fun key(accountIdB64: String) = "v1:$accountIdB64"

    private fun rows(accountIdB64: String): Map<String, GapLookRecord> = try {
        prefs.getString(key(accountIdB64), null)
            ?.let { runCatching { json.decodeFromString(serializer, it) }.getOrNull() }
            .orEmpty()
    } catch (_: Throwable) {
        emptyMap()
    }

    private fun write(accountIdB64: String, rows: Map<String, GapLookRecord>): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val editor = prefs.edit()
            if (rows.isEmpty()) {
                editor.remove(key(accountIdB64))
            } else {
                editor.putString(key(accountIdB64), json.encodeToString(serializer, rows))
            }
            editor.commit()
        } ?: false

    override fun load(accountIdB64: String): Map<String, GapLookRecord> = synchronized(LOCK) { rows(accountIdB64) }

    override fun put(accountIdB64: String, record: GapLookRecord): Boolean = synchronized(LOCK) {
        write(accountIdB64, rows(accountIdB64) + (record.groupIdHex.lowercase() to record))
    }

    override fun remove(accountIdB64: String, groupIdHex: String) {
        synchronized(LOCK) {
            val all = rows(accountIdB64)
            val hex = groupIdHex.lowercase()
            if (hex in all) write(accountIdB64, all - hex)
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        synchronized(LOCK) { prefs.edit().remove(key(accountIdB64)).commit() }
    }

    internal companion object {
        const val PREFS_NAME = "lakko-reentry-gap-looks"
        private val LOCK = Any()
    }
}

/** What one call of [GapHistoryLooker.lookIfDue] did. */
internal enum class GapLookOutcome {
    /** No look is kept for this conversation. */
    NOT_HELD,

    /** Another look of this conversation is running. */
    IN_FLIGHT,

    /** Looked less than [GAP_HISTORY_LOOK_INTERVAL_MS] ago. */
    NOT_DUE,

    /** Past [GAP_HISTORY_LOOK_RETENTION_MS]: the look is dropped, the line stays. */
    DROPPED,

    /** Looked: the read failed, held no row, or no row opened. Kept. */
    NOTHING_YET,

    /** A bundle opened and was applied: the look is dropped. */
    RECEIVED,
}

/**
 * The returning device's look for its gap bundle (section 7): one short read
 * of the rendezvous the re-admitter posts to, at most once a minute per
 * conversation, for 30 days, until a bundle opens. The read, the open and
 * what is done with an opened bundle are injected, so the decisions are
 * tested without a network or the native core.
 */
internal class GapHistoryLooker(
    private val store: GapLookStore,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    private val inFlight: MutableSet<String> = ConcurrentHashMap.newKeySet()

    /** The join: the record is written, its first look due at once. */
    fun start(accountIdB64: String, record: GapLookRecord): Boolean =
        store.put(accountIdB64, record.copy(groupIdHex = record.groupIdHex.lowercase(), lastLookMs = 0L))

    /**
     * One look for [groupIdHex] when one is due: for each gap key, one read of
     * its rendezvous ([rendezvousOf] the public key). [read] returns the
     * payloads oldest first (`null` on a failed read); [open] opens, checks
     * the signer and applies one, `null` (or a throw) when it is not a bundle
     * this device keeps; [received] narrows the gap line once one did.
     */
    suspend fun <T> lookIfDue(
        accountIdB64: String,
        groupIdHex: String,
        read: suspend (rendezvous: ByteArray) -> List<ByteArray>?,
        open: suspend (secret: ByteArray, payload: ByteArray, route: ReentryRouteKind) -> T?,
        rendezvousOf: (gapPublicKey: ByteArray) -> ByteArray = { uniffi.lakko_ffi_uniffi.gapHistoryRendezvous(it) },
        received: (record: GapLookRecord, applied: T) -> Unit,
    ): GapLookOutcome {
        val hex = groupIdHex.lowercase()
        val claim = "$accountIdB64:$hex"
        if (!inFlight.add(claim)) return GapLookOutcome.IN_FLIGHT
        try {
            val record = store.get(accountIdB64, hex) ?: return GapLookOutcome.NOT_HELD
            val now = clock()
            when (gapLookStep(record, now)) {
                GapLookStep.DROP -> {
                    store.remove(accountIdB64, hex)
                    return GapLookOutcome.DROPPED
                }
                GapLookStep.WAIT -> return GapLookOutcome.NOT_DUE
                GapLookStep.LOOK -> Unit
            }
            // The time is written before the read: a read that fails, hangs
            // or ends with the process still counts against the minute.
            store.put(accountIdB64, record.copy(lastLookMs = now))
            if (record.keys.isEmpty()) {
                store.remove(accountIdB64, hex)
                return GapLookOutcome.DROPPED
            }
            for (key in record.keys) {
                val (public, secret) = try {
                    key.publicKey() to key.secret()
                } catch (_: IllegalArgumentException) {
                    continue
                }
                try {
                    val rendezvous = try {
                        rendezvousOf(public)
                    } catch (_: Throwable) {
                        continue
                    }
                    val payloads = try {
                        read(rendezvous)
                    } catch (cause: kotlinx.coroutines.CancellationException) {
                        throw cause
                    } catch (_: Throwable) {
                        null
                    } ?: continue
                    // Newest first; a row that is not a bundle this device
                    // keeps is passed over.
                    for (payload in payloads.asReversed()) {
                        val applied = try {
                            open(secret, payload, record.route)
                        } catch (cause: kotlinx.coroutines.CancellationException) {
                            throw cause
                        } catch (_: Throwable) {
                            null
                        } ?: continue
                        received(record, applied)
                        store.remove(accountIdB64, hex)
                        return GapLookOutcome.RECEIVED
                    }
                } finally {
                    secret.fill(0)
                }
            }
            return GapLookOutcome.NOTHING_YET
        } finally {
            inFlight.remove(claim)
        }
    }
}

// ---------------------------------------------------------------------------
// The returning device's gap keys (section 7)
// ---------------------------------------------------------------------------

/**
 * The secrets of the gap keys this device put in its re-entry requests, per
 * conversation and public key, kept until the device came back (then they
 * move to its [GapLookRecord]) or the request left the record.
 */
internal interface GapKeyStore {
    /** `false` when the row did not reach the disk. */
    fun put(accountIdB64: String, groupIdHex: String, entry: GapKeyEntry): Boolean

    /** The kept keys of [groupIdHex] whose public half is in [publicKeysB64]. */
    fun entries(accountIdB64: String, groupIdHex: String, publicKeysB64: Collection<String>): List<GapKeyEntry>

    /** Forget the keys of [groupIdHex] named in [publicKeysB64]. */
    fun remove(accountIdB64: String, groupIdHex: String, publicKeysB64: Collection<String>)

    /** The group left this device, or the device came back: every key. */
    fun removeGroup(accountIdB64: String, groupIdHex: String)

    fun clearForAccount(accountIdB64: String)
}

/** The production [GapKeyStore]: EncryptedSharedPreferences (the Android
 *  Keystore key), behind the ADR-0077 gate. */
internal class EncryptedGapKeyStore(context: Context) : GapKeyStore {
    private val app = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }
    private val serializer = MapSerializer(
        String.serializer(),
        MapSerializer(String.serializer(), String.serializer()),
    )

    private val prefs: SharedPreferences by lazy {
        val master = androidx.security.crypto.MasterKey.Builder(app)
            .setKeyScheme(androidx.security.crypto.MasterKey.KeyScheme.AES256_GCM)
            .build()
        androidx.security.crypto.EncryptedSharedPreferences.create(
            app,
            PREFS_NAME,
            master,
            androidx.security.crypto.EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            androidx.security.crypto.EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    private fun key(accountIdB64: String) = "v1:$accountIdB64"

    private fun rows(accountIdB64: String): Map<String, Map<String, String>> = try {
        prefs.getString(key(accountIdB64), null)
            ?.let { runCatching { json.decodeFromString(serializer, it) }.getOrNull() }
            .orEmpty()
    } catch (_: Throwable) {
        emptyMap()
    }

    private fun write(accountIdB64: String, rows: Map<String, Map<String, String>>): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val editor = prefs.edit()
            val kept = rows.filterValues { it.isNotEmpty() }
            if (kept.isEmpty()) {
                editor.remove(key(accountIdB64))
            } else {
                editor.putString(key(accountIdB64), json.encodeToString(serializer, kept))
            }
            editor.commit()
        } ?: false

    override fun put(accountIdB64: String, groupIdHex: String, entry: GapKeyEntry): Boolean = synchronized(LOCK) {
        val all = rows(accountIdB64)
        val hex = groupIdHex.lowercase()
        write(accountIdB64, all + (hex to (all[hex].orEmpty() + (entry.publicKeyB64 to entry.secretB64))))
    }

    override fun entries(accountIdB64: String, groupIdHex: String, publicKeysB64: Collection<String>): List<GapKeyEntry> =
        synchronized(LOCK) {
            val held = rows(accountIdB64)[groupIdHex.lowercase()].orEmpty()
            publicKeysB64.distinct().mapNotNull { pk -> held[pk]?.let { GapKeyEntry(pk, it) } }
        }

    override fun remove(accountIdB64: String, groupIdHex: String, publicKeysB64: Collection<String>) {
        synchronized(LOCK) {
            val all = rows(accountIdB64)
            val hex = groupIdHex.lowercase()
            val held = all[hex] ?: return
            val kept = held - publicKeysB64.toSet()
            if (kept.size != held.size) write(accountIdB64, all + (hex to kept))
        }
    }

    override fun removeGroup(accountIdB64: String, groupIdHex: String) {
        synchronized(LOCK) {
            val all = rows(accountIdB64)
            val hex = groupIdHex.lowercase()
            if (hex in all) write(accountIdB64, all - hex)
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        synchronized(LOCK) { prefs.edit().remove(key(accountIdB64)).commit() }
    }

    internal companion object {
        const val PREFS_NAME = "lakko-reentry-gap-keys"
        private val LOCK = Any()
    }
}

// ---------------------------------------------------------------------------
// When this device first saw a leaf (section 7, the re-admitter's floor)
// ---------------------------------------------------------------------------

/** The key a first observation is filed under: one per (group, leaf). */
internal fun leafFirstSeenKey(groupIdHex: String, leafKeyHex: String): String =
    groupIdHex.lowercase() + ":" + leafKeyHex.lowercase()

/**
 * [seen] with a first observation at [nowMs] for each of [leafKeysHex] in
 * [groupIdHex] it did not hold; an existing observation is never moved.
 * `null` when nothing is new (nothing to write).
 */
internal fun withFirstSeen(
    seen: Map<String, Long>,
    groupIdHex: String,
    leafKeysHex: Collection<String>,
    nowMs: Long,
): Map<String, Long>? {
    val added = leafKeysHex
        .map { leafFirstSeenKey(groupIdHex, it) }
        .filter { it !in seen }
        .distinct()
    if (added.isEmpty()) return null
    return seen + added.associateWith { nowMs }
}

/**
 * The first time this device saw each leaf in each group's roster: the
 * re-admitter's `leafFirstSeenMs`, so the rows it sends a returning device
 * never start before its own evidence that the leaf was in the conversation.
 */
internal interface LeafFirstSeenStore {
    fun firstSeen(accountIdB64: String, groupIdHex: String, leafKeyHex: String): Long?

    /** Record [leafKeysHex] as seen in [groupIdHex] at [nowMs] where none
     *  was; the first observation stays. */
    fun noteSeen(accountIdB64: String, groupIdHex: String, leafKeysHex: Collection<String>, nowMs: Long)

    fun removeGroup(accountIdB64: String, groupIdHex: String)

    fun clearForAccount(accountIdB64: String)
}

/** The production [LeafFirstSeenStore], behind the ADR-0077 gate. Leaf keys
 *  and times only, no secret. A read cache per account keeps the poll tail
 *  from reading the disk on every pass. */
internal class SharedPrefsLeafFirstSeenStore(context: Context) : LeafFirstSeenStore {
    private val appContext = context.applicationContext
    private val json = Json { ignoreUnknownKeys = true }
    private val serializer = MapSerializer(String.serializer(), Long.serializer())
    private val cache = ConcurrentHashMap<String, Map<String, Long>>()

    private fun prefs(): SharedPreferences = appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String) = "v1:$accountIdB64"

    private fun rows(accountIdB64: String): Map<String, Long> = cache[accountIdB64] ?: try {
        prefs().getString(key(accountIdB64), null)
            ?.let { runCatching { json.decodeFromString(serializer, it) }.getOrNull() }
            .orEmpty()
    } catch (_: Throwable) {
        emptyMap()
    }.also { cache[accountIdB64] = it }

    private fun write(accountIdB64: String, rows: Map<String, Long>): Boolean {
        val ok = me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val editor = prefs().edit()
            if (rows.isEmpty()) editor.remove(key(accountIdB64)) else editor.putString(key(accountIdB64), json.encodeToString(serializer, rows))
            editor.commit()
        } ?: false
        if (ok) cache[accountIdB64] = rows else cache.remove(accountIdB64)
        return ok
    }

    override fun firstSeen(accountIdB64: String, groupIdHex: String, leafKeyHex: String): Long? =
        synchronized(LOCK) { rows(accountIdB64)[leafFirstSeenKey(groupIdHex, leafKeyHex)] }

    override fun noteSeen(accountIdB64: String, groupIdHex: String, leafKeysHex: Collection<String>, nowMs: Long) {
        synchronized(LOCK) {
            val next = withFirstSeen(rows(accountIdB64), groupIdHex, leafKeysHex, nowMs) ?: return
            write(accountIdB64, next)
        }
    }

    override fun removeGroup(accountIdB64: String, groupIdHex: String) {
        synchronized(LOCK) {
            val prefix = groupIdHex.lowercase() + ":"
            val all = rows(accountIdB64)
            val kept = all.filterKeys { !it.startsWith(prefix) }
            if (kept.size != all.size) write(accountIdB64, kept)
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        synchronized(LOCK) {
            cache.remove(accountIdB64)
            prefs().edit().remove(key(accountIdB64)).commit()
        }
    }

    internal companion object {
        const val PREFS_NAME = "lakko-reentry-leaf-first-seen"
        private val LOCK = Any()
    }
}
