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

/** The core's request cadence (`reentryRequestDue`), injected. */
internal fun interface ReentryRequestDue {
    fun due(state: BehindState, lastSentMs: Long?, nowMs: Long): Boolean
}

/** The production [ReentryRequestDue]. */
internal object NativeReentryRequestDue : ReentryRequestDue {
    override fun due(state: BehindState, lastSentMs: Long?, nowMs: Long): Boolean {
        val s = when (state.level) {
            BehindLevel.IN_STEP -> uniffi.lakko_ffi_uniffi.FfiBehindState.InStep
            BehindLevel.SUSPECTED -> uniffi.lakko_ffi_uniffi.FfiBehindState.Suspected(state.sinceMs.toULong())
            BehindLevel.CONFIRMED -> uniffi.lakko_ffi_uniffi.FfiBehindState.Confirmed(state.sinceMs.toULong())
        }
        return uniffi.lakko_ffi_uniffi.reentryRequestDue(
            s,
            lastSentMs?.coerceAtLeast(0L)?.toULong(),
            nowMs.coerceAtLeast(0L).toULong(),
        )
    }
}

/**
 * What this device keeps per conversation. [latestEpoch] is the newest
 * `latest_epoch` a read named (the request's `epoch_left`), [gapFromMs] the
 * time of the first row this device could not read (the request's
 * `gap_from_ms` and the start of the gap line), [lastRequestSentMs] when the
 * last request went out.
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
) {
    val state: BehindState get() = BehindState(level, sinceMs)

    /** Nothing worth keeping on disk. */
    val isEmpty: Boolean
        get() = level == BehindLevel.IN_STEP && refusedReads == 0 && !failedRowAboveLocal &&
            !publishRefused && gapFromMs == null && lastRequestSentMs == null
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

    /** A row of [groupIdHex] was skipped (it could not be applied). */
    fun noteSkipped(groupIdHex: String, sealedEpoch: ULong?, localEpoch: ULong?, rowAtMs: Long?) {
        update(groupIdHex) { e ->
            val above = sealedEpoch != null && localEpoch != null && sealedEpoch > localEpoch
            e.copy(
                failedRowAboveLocal = e.failedRowAboveLocal || above,
                gapFromMs = e.gapFromMs ?: rowAtMs?.takeIf { it > 0L } ?: clock(),
            )
        }
    }

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
        val before = entry(groupIdHex)
        val reset = if (before.refusedReads != 0) update(groupIdHex) { it.copy(refusedReads = 0) } else before
        val next = fold(groupIdHex, reset, ownCommitOwed, drained, latestEpoch, localEpoch)
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
            // Cleared: what the suspicion rested on goes with it.
            replace(groupIdHex, ConversationBehindEntry())
            reentryLog(false, "conversation back in step (local=$localEpoch)")
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

    /** A request to come back went out at [atMs]. */
    fun noteRequestSent(groupIdHex: String, atMs: Long) {
        update(groupIdHex) { it.copy(lastRequestSentMs = atMs) }
    }

    /** The device came back: everything about [groupIdHex] goes. */
    fun cleared(groupIdHex: String) {
        replace(groupIdHex, ConversationBehindEntry())
    }

    /** The conversation went from this device (an end, a delete). */
    fun forget(groupIdHex: String) = cleared(groupIdHex)

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

/** One request this device sent: its KeyPackage, for `Group.joinReentry`. */
@Serializable
internal data class ReentryRequestRow(
    val keyPackageB64: String,
    val epochLeft: Long,
    val gapFromMs: Long,
    val sentAtMs: Long,
    val route: ReentryRouteKind,
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
 * Whether the owner (re)sends its knock key in-group: on a version it has not
 * sent yet, and on growth (a newcomer holds none), as the group name is.
 */
internal fun knockKeyResendDue(sentVersion: Int?, currentVersion: Int, grew: Boolean): Boolean =
    sentVersion == null || sentVersion != currentVersion || grew

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
 * The gap line's text (`chat.reentry.gap`): [from] is the date and time of the
 * first row this device could not read, [to] the moment it came back, with
 * the date left out when both fall on one day. [dayOf] and [dateText] /
 * [timeText] are the platform's formatters, injected.
 */
internal fun gapLineText(
    gap: ReentryGap,
    dayOf: (Long) -> Long,
    dateText: (Long) -> String,
    timeText: (Long) -> String,
): me.lakko.app.l10n.UiText {
    val from = dateText(gap.fromMs) + ", " + timeText(gap.fromMs)
    val to = if (dayOf(gap.fromMs) == dayOf(gap.toMs)) {
        timeText(gap.toMs)
    } else {
        dateText(gap.toMs) + ", " + timeText(gap.toMs)
    }
    return me.lakko.app.l10n.UiText("chat.reentry.gap", texts = mapOf("from" to from, "to" to to))
}

// ---------------------------------------------------------------------------
// The waiting note (`docs/ux/conversation-rejoin.md` 2.2)
// ---------------------------------------------------------------------------

/** Who can bring this device back: decides the note's second line. */
internal enum class ReentryHelper {
    /** Another device of the person is in this chat. */
    OWN_DEVICE,

    /** A one-to-one: the other person. */
    OTHER_PERSON,

    /** A group: its owner or an admin. */
    OWNER_OR_ADMIN,
}

/** No second line in the first 30 seconds. Chosen, not measured (open question 5). */
internal const val REENTRY_SECOND_LINE_AFTER_MS: Long = 30_000L

/**
 * The waiting note's second line, or `null` while the first 30 seconds of
 * waiting run. [otherName] is the other person's display name (one-to-one).
 */
internal fun waitingSecondLine(
    helper: ReentryHelper,
    waitingSinceMs: Long,
    nowMs: Long,
    otherName: String?,
): me.lakko.app.l10n.UiText? {
    if (nowMs - waitingSinceMs < REENTRY_SECOND_LINE_AFTER_MS) return null
    return when (helper) {
        ReentryHelper.OWN_DEVICE -> me.lakko.app.l10n.UiText("chat.reentry.second.own_device")
        ReentryHelper.OTHER_PERSON -> otherName?.takeIf { it.isNotBlank() }
            ?.let { me.lakko.app.l10n.UiText("chat.reentry.second.person", texts = mapOf("name" to it)) }
        ReentryHelper.OWNER_OR_ADMIN -> me.lakko.app.l10n.UiText("chat.reentry.second.group")
    }
}

/** What the waiting note needs: since when this chat waits (for the
 *  30-second rule) and who can bring the device back, `null` when unknown. */
internal data class ReentryWaitingNote(val sinceMs: Long, val helper: ReentryHelper?)

/** The route a request takes and the helper the note names are one fact. */
internal fun helperOf(route: ReentryRouteKind): ReentryHelper = when (route) {
    ReentryRouteKind.OWN_DEVICE -> ReentryHelper.OWN_DEVICE
    ReentryRouteKind.MAILBOX -> ReentryHelper.OTHER_PERSON
    ReentryRouteKind.KNOCK -> ReentryHelper.OWNER_OR_ADMIN
}

// ---------------------------------------------------------------------------
// The history of the gap (section 7)
// ---------------------------------------------------------------------------

/**
 * The exporter label both ends derive the gap bundle's secret under, at the
 * epoch the replacement commit creates (`epoch_left + 1`), with the returning
 * leaf's 32-byte signature key as the context: a device that re-requested
 * holds two KeyPackages and cannot know which one was used, but its leaf key
 * is the same in both. The carrier is a client convention shared by web, iOS
 * and Android (the core names none).
 */
internal const val REENTRY_HISTORY_EXPORTER_LABEL: String = "lakko/conversation-reentry/history/v1"

/** The label the rendezvous id is hashed under (no separator follows it). */
internal const val REENTRY_HISTORY_RENDEZVOUS_LABEL: String = "lakko/conversation-reentry/history-rendezvous/v1"

/** The rendezvous id the gap bundle is posted to and read from:
 *  `SHA-256(label || secret)`, so only the members at the join epoch can
 *  name it. */
internal fun reentryHistoryRendezvous(secret: ByteArray): ByteArray {
    val digest = java.security.MessageDigest.getInstance("SHA-256")
    digest.update(REENTRY_HISTORY_RENDEZVOUS_LABEL.toByteArray(Charsets.US_ASCII))
    digest.update(secret)
    return digest.digest()
}

/** A gap bundle the re-admitter still owes: sent once its group reaches
 *  [epochLeft] + 1, the epoch the returning device joins at. [leafSignatureKey]
 *  is the returning leaf's key (the exporter context). In memory. */
internal data class OwedGapBundle(
    val groupIdHex: String,
    val leafSignatureKey: ByteArray,
    val epochLeft: ULong,
    val gapFromMs: Long,
    val owedSinceMs: Long,
)

/** What to do with an owed bundle now that the group is at [epoch]. */
internal enum class OwedBundleStep { WAIT, SEND, DROP }

/** Give up on an owed bundle after a day. Chosen, not measured. */
internal const val OWED_GAP_BUNDLE_TTL_MS: Long = 24L * 60L * 60L * 1_000L

internal fun owedBundleStep(owed: OwedGapBundle, epoch: ULong?, nowMs: Long): OwedBundleStep = when {
    nowMs - owed.owedSinceMs > OWED_GAP_BUNDLE_TTL_MS -> OwedBundleStep.DROP
    epoch == null -> OwedBundleStep.WAIT
    owed.epochLeft == ULong.MAX_VALUE -> OwedBundleStep.DROP
    epoch == owed.epochLeft + 1uL -> OwedBundleStep.SEND
    // Moved past the join epoch without this device seeing it, or the
    // replacement lost to another commit: the key can no longer be derived.
    epoch > owed.epochLeft + 1uL -> OwedBundleStep.DROP
    else -> OwedBundleStep.WAIT
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

/** The rows of [rows] a gap bundle carries: from [fromMs] on, at most [cap]. */
internal fun gapRows(rows: List<PersistedMessage>, fromMs: Long, cap: Int, timeOf: (PersistedMessage) -> Long?): Pair<List<PersistedMessage>, Boolean> {
    val inGap = rows.filter { (timeOf(it) ?: 0L) >= fromMs }
    val kept = inGap.takeLast(cap.coerceAtLeast(1))
    return kept to (kept.size < inGap.size)
}
