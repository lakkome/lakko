// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.storage

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json

/*
 * ADR-0065 sections 3 and 6 — what this device does to keep the files that
 * matter alive and to remove the ones its account deleted:
 *
 *   * [BlobDeleteQueue] — the blobs of a message deleted for everyone, of an
 *     upload never sent and of a replaced profile photo, deleted at once and,
 *     when that fails, kept in a small durable list per account and retried.
 *   * [BlobKeepSchedule] — about once a month per "bucket" (one conversation's
 *     pinned messages, the saved list, the own profile photo), spread over
 *     time, a `POST /blob/keep` for the bucket's blob ids.
 *
 * Both are pure over a storage port; [BlobLifetime] is the production wiring.
 */

/** Durable rows, one string per (account, name). */
public interface BlobLifetimeStorage {
    public fun get(account: String, name: String): String?
    public fun set(account: String, name: String, value: String?)
}

/** In memory (host tests). */
public class InMemoryBlobLifetimeStorage : BlobLifetimeStorage {
    private val rows = HashMap<String, String>()

    @Synchronized
    override fun get(account: String, name: String): String? = rows["$account|$name"]

    @Synchronized
    override fun set(account: String, name: String, value: String?) {
        if (value == null) rows.remove("$account|$name") else rows["$account|$name"] = value
    }

    @Synchronized
    public fun isEmptyFor(account: String): Boolean = rows.keys.none { it.startsWith("$account|") }
}

private val lifetimeJson = Json { ignoreUnknownKeys = true }

/** The blob deletes still owed, per account. */
public class BlobDeleteQueue(private val storage: BlobLifetimeStorage) {
    private fun load(account: String): List<String> = try {
        storage.get(account, ROW)?.let { lifetimeJson.decodeFromString(ListSerializer(String.serializer()), it) }.orEmpty()
    } catch (_: Throwable) {
        emptyList()
    }

    private fun save(account: String, ids: List<String>) {
        storage.set(
            account,
            ROW,
            ids.takeIf { it.isNotEmpty() }?.let { lifetimeJson.encodeToString(ListSerializer(String.serializer()), it) },
        )
    }

    /** The owed blob ids (hex), oldest first. */
    @Synchronized
    public fun owed(account: String): List<String> = load(account)

    /** Add [blobIdsHex], each once; the oldest fall off past [MAX_OWED]. */
    @Synchronized
    public fun add(account: String, blobIdsHex: List<String>) {
        val next = (load(account) + blobIdsHex.filter(::isBlobHex)).distinct().takeLast(MAX_OWED)
        save(account, next)
    }

    /**
     * Try every owed delete with [delete] (`true` = done, `false` = refused
     * for good: not this account's or already gone; a throw = try later) and
     * keep only the ones that threw. Returns how many remain.
     */
    public suspend fun drain(account: String, delete: suspend (String) -> Boolean): Int {
        val owed = synchronized(this) { load(account) }
        val failed = ArrayList<String>()
        for (id in owed) {
            val done = try {
                delete(id)
                true
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                false
            }
            if (!done) failed += id
        }
        synchronized(this) {
            // Ids added while draining stay.
            val now = load(account)
            val added = now.filter { it !in owed }
            save(account, (failed + added).distinct().takeLast(MAX_OWED))
            return failed.size + added.size
        }
    }

    /** Forget device. */
    @Synchronized
    public fun clear(account: String) {
        save(account, emptyList())
    }

    public companion object {
        private const val ROW = "deletes"

        /** Chosen, not measured: past what one device owes. */
        public const val MAX_OWED: Int = 2000
    }
}

/** One keep bucket and the ids it names now. */
public data class KeepBucket(val key: String, val blobIdsHex: List<String>)

/**
 * When each keep bucket was last kept, per account. A bucket is due once
 * [KEEP_INTERVAL_MS] plus its own offset (spread over [KEEP_SPREAD_MS] by a
 * hash of its key, so a device's buckets do not all fall on one day) has
 * passed; a bucket never kept is due after its offset from the first time it
 * is seen, so a fresh device does not keep everything at once.
 */
public class BlobKeepSchedule(
    private val storage: BlobLifetimeStorage,
    private val now: () -> Long = System::currentTimeMillis,
) {
    private val mapSerializer = MapSerializer(String.serializer(), Long.serializer())

    private fun load(account: String): Map<String, Long> = try {
        storage.get(account, ROW)?.let { lifetimeJson.decodeFromString(mapSerializer, it) }.orEmpty()
    } catch (_: Throwable) {
        emptyMap()
    }

    private fun save(account: String, map: Map<String, Long>) {
        storage.set(account, ROW, if (map.isEmpty()) null else lifetimeJson.encodeToString(mapSerializer, map))
    }

    /**
     * The buckets of [buckets] due now, at most [maxPerTick], the longest
     * overdue first. A bucket with no ids is never due. A bucket seen for the
     * first time is recorded as kept "now minus the interval plus its offset".
     */
    @Synchronized
    public fun due(account: String, buckets: List<KeepBucket>, maxPerTick: Int = MAX_PER_TICK): List<KeepBucket> {
        val t = now()
        val kept = load(account).toMutableMap()
        var changed = false
        for (b in buckets) {
            if (b.key !in kept) {
                // First sight: due after its own offset, not at once.
                // Never due at once: at least the next tick goes by.
                kept[b.key] = (t - KEEP_INTERVAL_MS + offsetOf(b.key)).coerceAtLeast(t - KEEP_INTERVAL_MS + 1)
                changed = true
            }
        }
        // Buckets that no longer exist are forgotten.
        val live = buckets.map { it.key }.toSet()
        if (kept.keys.retainAll(live)) changed = true
        if (changed) save(account, kept)
        return buckets
            .filter { it.blobIdsHex.isNotEmpty() && t - (kept[it.key] ?: t) >= KEEP_INTERVAL_MS }
            .sortedBy { kept[it.key] ?: t }
            .take(maxPerTick)
    }

    /** [key] was kept at the current time. */
    @Synchronized
    public fun markKept(account: String, key: String) {
        save(account, load(account) + (key to now()))
    }

    @Synchronized
    public fun clear(account: String) {
        save(account, emptyMap())
    }

    public companion object {
        private const val ROW = "keeps"

        /** ADR-0065 section 3: "about once a month". */
        public const val KEEP_INTERVAL_MS: Long = 30L * 24 * 60 * 60 * 1000

        /** A new set first comes due at a point spread over its first 30
         *  days by a hash of its key (the same rule on every client). */
        public const val KEEP_SPREAD_MS: Long = KEEP_INTERVAL_MS

        /** At most this many due sets per tick (the same on every client). */
        public const val MAX_PER_TICK: Int = 4

        /** The first-due offset of [key]: FNV-1a 32-bit over the key's UTF-16
         *  code units, modulo 30 days (web `spreadOffsetMs`, iOS
         *  `KeepSchedule.spreadOffsetMs`: one function on every client). */
        internal fun offsetOf(key: String): Long {
            var h = 2166136261L
            for (c in key) {
                h = h xor c.code.toLong()
                h = (h * 16777619L) and 0xffffffffL
            }
            return h % KEEP_SPREAD_MS
        }
    }
}

/** The 64-hex blob id form, lowercase. */
public fun isBlobHex(s: String): Boolean = s.length == 64 && s.all { it in '0'..'9' || it in 'a'..'f' }

/** [ids] as hex, each once. */
public fun blobHexes(ids: List<ByteArray>): List<String> =
    ids.filter { it.size == StorageClient.BLOB_ID_LEN }.map { StorageClient.blobIdHex(it) }.distinct()

/** The keep calls for a bucket: its ids in requests of at most
 *  [StorageClient.KEEP_MAX_IDS]. */
public fun keepBatches(ids: List<String>): List<List<String>> = ids.chunked(StorageClient.KEEP_MAX_IDS)

private fun unhex(s: String): ByteArray = ByteArray(s.length / 2) { i -> s.substring(i * 2, i * 2 + 2).toInt(16).toByte() }

/** [BlobLifetimeStorage] over SharedPreferences, each row sealed under the
 *  device's Keystore key (blob ids are metadata worth keeping off disk). */
internal class SealedPrefsBlobLifetimeStorage(context: Context) : BlobLifetimeStorage {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    override fun get(account: String, name: String): String? {
        val raw = prefs.getString(key(account, name), null) ?: return null
        return try {
            me.lakko.app.avatar.KeystoreAvatarSeal.open(java.util.Base64.getDecoder().decode(raw))
                ?.toString(Charsets.UTF_8)
        } catch (_: Throwable) {
            null
        }
    }

    override fun set(account: String, name: String, value: String?) {
        if (value == null) {
            prefs.edit().remove(key(account, name)).apply()
            return
        }
        me.lakko.app.account.AccountStoreGate.write(account) {
            val sealed = me.lakko.app.avatar.KeystoreAvatarSeal.seal(value.toByteArray(Charsets.UTF_8)) ?: return@write
            prefs.edit().putString(key(account, name), java.util.Base64.getEncoder().encodeToString(sealed)).apply()
        }
    }

    fun clearAccount(account: String) {
        prefs.edit().apply {
            for (k in prefs.all.keys) if (k.startsWith("$account|")) remove(k)
        }.apply()
    }

    private fun key(account: String, name: String) = "$account|$name"

    companion object {
        const val PREFS = "lakko-blob-lifetime"
    }
}

/** The production wiring: one queue and one schedule per process, over the
 *  signed-in account. */
public object BlobLifetime {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val drainLock = Mutex()

    @Volatile
    private var storageRef: SealedPrefsBlobLifetimeStorage? = null

    private fun storage(context: Context): SealedPrefsBlobLifetimeStorage =
        storageRef ?: synchronized(this) {
            storageRef ?: SealedPrefsBlobLifetimeStorage(context).also { storageRef = it }
        }

    public fun queue(context: Context): BlobDeleteQueue = BlobDeleteQueue(storage(context))

    public fun schedule(context: Context): BlobKeepSchedule = BlobKeepSchedule(storage(context))

    private fun account(): String? = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64

    /** Delete [blobIds] now, keeping any that fail for a retry. */
    public fun delete(context: Context, blobIds: List<ByteArray>) {
        val account = account() ?: return
        val hexes = blobHexes(blobIds)
        if (hexes.isEmpty()) return
        queue(context).add(account, hexes)
        scope.launch { drain(context) }
    }

    /** Try the owed deletes once. */
    public suspend fun drain(context: Context) {
        val account = account() ?: return
        drainLock.withLock {
            StorageClient().use { client ->
                queue(context).drain(account) { hex -> client.deleteBlob(unhex(hex)) }
            }
        }
    }

    /**
     * One keep tick: drain the owed deletes, then keep at most one due bucket
     * of [buckets] (the caller lists them: pinned messages per conversation,
     * the saved list, the own profile photo).
     */
    public suspend fun tick(context: Context, buckets: List<KeepBucket>) {
        drain(context)
        val account = account() ?: return
        val schedule = schedule(context)
        val due = schedule.due(account, buckets)
        if (due.isEmpty()) return
        StorageClient().use { client ->
            for (bucket in due) {
                try {
                    for (batch in keepBatches(bucket.blobIdsHex)) client.keep(batch.map(::unhex))
                    schedule.markKept(account, bucket.key)
                } catch (cancel: kotlinx.coroutines.CancellationException) {
                    throw cancel
                } catch (cause: Throwable) {
                    android.util.Log.w("BlobLifetime", "keep failed: ${cause.javaClass.simpleName}")
                }
            }
        }
    }

    /** Forget device: this account's owed deletes and keep stamps. */
    public fun forgetAccount(context: Context, accountIdB64: String) {
        storage(context).clearAccount(me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64))
    }
}
