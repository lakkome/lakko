// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull

/*
 * ADR-0089 F2 — a mark needs a moderator's signature, not only the snapshot.
 *
 * A snapshot bucket that says "suspended" (or "removed", for a channel) is
 * acted on only once the moderation log entry behind it is fetched
 * (`GET /moderation/log/entry/{logIndex}?size={answer root's log_size}`) and
 * the core proves it is a statement a moderator of the root-signed list
 * signed for exactly that target (`moderationProveAccountStanding` /
 * `moderationProveChannelStanding`). No pinned root key, no list, no entry,
 * or a refused proof: nothing is proven, and the standing stays UNKNOWN.
 * Nothing here logs a key, an index or a prefix.
 */

/**
 * The device's moderator-list floor: the highest list version it has seen
 * (the report sheet keeps it, [ModeratorListVersionStore]). A list below it
 * is refused, so a server cannot hand back an older list that still names a
 * since-revoked moderator.
 */
public interface ModeratorListFloor {
    public fun highest(): ULong?
    public fun record(version: ULong)
}

/** The moderator list as last fetched and verified under the pinned root. */
public class ModeratorListRead(public val bytes: ByteArray, public val fetchedNow: Boolean)

/**
 * The root-signed moderator list, kept in memory: the report sheet's fetch
 * offers what it fetched ([offer]), the proofs read it ([get]). A list is
 * kept only when it verifies under the pinned root and is not older than the
 * device's floor ([ModeratorListFloor], raised by a newer one); no floor
 * store, no list.
 */
public class ModeratorListCache(
    private val rootKey: () -> ByteArray?,
    private val fetch: suspend () -> ModeratorListFetch,
    /** The list's version under the root; throws when it does not verify. */
    private val versionOf: (list: ByteArray, root: ByteArray) -> ULong,
    /** The device's floor; `null` (not installed yet) keeps no list. */
    private val floor: () -> ModeratorListFloor?,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    private class Held(val bytes: ByteArray, val version: ULong, val keptAtMs: Long)

    private val mutex = Mutex()
    private val lock = Any()
    private var held: Held? = null

    /** The kept list while younger than [MAX_AGE_MS], else a fetched one. */
    public suspend fun get(): ModeratorListRead? = mutex.withLock {
        val kept = synchronized(lock) { held }
        val floorStore = floor() ?: return@withLock null
        val usable = kept != null && nowMs() - kept.keptAtMs < MAX_AGE_MS &&
            !belowFloor(kept.version, floorStore)
        if (usable) return@withLock ModeratorListRead(kept!!.bytes, false)
        fetchLocked()?.let { ModeratorListRead(it, true) }
    }

    /** A list fetched now, whatever is kept. */
    public suspend fun fresh(): ModeratorListRead? = mutex.withLock { fetchLocked()?.let { ModeratorListRead(it, true) } }

    /**
     * A list fetched now for sealing (the suspension appeal): [ModeratorListFetch.Ok]
     * only with a list that verifies under the pinned root and is not older
     * than the device's floor (a newer one raises it), so nothing is sealed
     * to a list that still names a removed moderator. No root, no floor, a
     * list that does not verify or is older: [ModeratorListFetch.Unavailable];
     * no answer: [ModeratorListFetch.Network].
     */
    public suspend fun fetchVerified(): ModeratorListFetch = mutex.withLock {
        val fetched = try {
            fetch()
        } catch (cause: CancellationException) {
            throw cause
        } catch (_: Throwable) {
            return@withLock ModeratorListFetch.Network
        }
        when (fetched) {
            is ModeratorListFetch.Ok -> keep(fetched.bytes)?.let { ModeratorListFetch.Ok(it) }
                ?: ModeratorListFetch.Unavailable
            else -> fetched
        }
    }

    /** Keep [bytes] fetched elsewhere when it verifies and is not older. */
    public fun offer(bytes: ByteArray) {
        keep(bytes)
    }

    /** Forget the kept list. */
    public fun clear() {
        synchronized(lock) { held = null }
    }

    private suspend fun fetchLocked(): ByteArray? {
        val fetched = try {
            fetch()
        } catch (cause: CancellationException) {
            throw cause
        } catch (_: Throwable) {
            return null
        }
        val bytes = (fetched as? ModeratorListFetch.Ok)?.bytes ?: return null
        return keep(bytes)
    }

    /** The list now kept after offering [bytes]; `null` when [bytes] does not verify. */
    private fun keep(bytes: ByteArray): ByteArray? {
        val root = rootKey() ?: return null
        val floorStore = floor() ?: return null
        val version = try {
            versionOf(bytes, root)
        } catch (_: Throwable) {
            return null
        }
        // Older than a list this device has seen: refused (unknown).
        if (belowFloor(version, floorStore)) return null
        try {
            floorStore.record(version)
        } catch (_: Throwable) {
            return null
        }
        return synchronized(lock) {
            val kept = held
            if (kept != null && version < kept.version) {
                kept.bytes
            } else {
                held = Held(bytes.copyOf(), version, nowMs())
                bytes
            }
        }
    }

    private fun belowFloor(version: ULong, floorStore: ModeratorListFloor): Boolean {
        val highest = try {
            floorStore.highest()
        } catch (_: Throwable) {
            return true
        }
        return highest != null && version < highest
    }

    public companion object {
        /** A kept list is fetched again after an hour (chosen). */
        public const val MAX_AGE_MS: Long = 60L * 60L * 1000L

        @Volatile
        private var installedFloor: ModeratorListFloor? = null

        /** Bind the device's floor (the report sheet's store); at app start. */
        public fun installFloor(floor: ModeratorListFloor) {
            installedFloor = floor
        }

        /** The process-wide list. */
        public val shared: ModeratorListCache by lazy {
            ModeratorListCache(
                rootKey = { pinnedModerationRootKey() },
                fetch = { ModerationClient().use { it.moderatorList() } },
                versionOf = { list, root -> uniffi.lakko_ffi_uniffi.moderatorListVersion(list, root) },
                floor = { installedFloor },
            )
        }
    }
}

/**
 * The proof of one `suspended` standing. [prove] fetches the log entry at the
 * answer's own log size and hands it, the moderator list and the pinned root
 * key to `check` (the core's prove call, which throws on any refusal). A
 * refusal with a kept list is tried once more with a list fetched now (a
 * moderator added since the list was kept).
 */
public class StandingProofs(
    private val rootKey: () -> ByteArray?,
    private val fetchEntry: suspend (index: ULong, size: ULong) -> String?,
    private val moderatorList: ModeratorListCache,
) {
    public suspend fun prove(
        answerJson: String,
        logIndex: ULong,
        check: (entryJson: String, moderatorList: ByteArray, rootKey: ByteArray) -> Unit,
    ): Boolean {
        val root = rootKey() ?: return false
        val size = answerLogSize(answerJson) ?: return false
        val entry = try {
            fetchEntry(logIndex, size)
        } catch (cause: CancellationException) {
            throw cause
        } catch (_: Throwable) {
            null
        } ?: return false
        val list = moderatorList.get() ?: return false
        if (passes(check, entry, list.bytes, root)) return true
        if (list.fetchedNow) return false
        val fresh = moderatorList.fresh() ?: return false
        return passes(check, entry, fresh.bytes, root)
    }

    private fun passes(
        check: (String, ByteArray, ByteArray) -> Unit,
        entry: String,
        list: ByteArray,
        root: ByteArray,
    ): Boolean = try {
        check(entry, list, root)
        true
    } catch (_: Throwable) {
        false
    }
}

/** The `root.log_size` of a bucket answer, or `null`. */
public fun answerLogSize(answerJson: String): ULong? = try {
    val root = (Json.parseToJsonElement(answerJson) as? JsonObject)?.get("root") as? JsonObject
    (root?.get("log_size") as? JsonPrimitive)?.takeIf { !it.isString }?.contentOrNull?.toULongOrNull()
} catch (_: Throwable) {
    null
}
