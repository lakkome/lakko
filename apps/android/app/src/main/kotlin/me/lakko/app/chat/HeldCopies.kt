// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.builtins.MapSerializer
import kotlinx.serialization.builtins.serializer
import kotlinx.serialization.json.Json

/*
 * ADR-0079 section 5 / ADR-0050 section 2.9 (state bundle v3) - DISPLAY-ONLY
 * copies of the messages this account wrote, on ANOTHER of its devices, to a
 * person added through their invite link whose app has not answered yet.
 *
 * The device where the messages were typed keeps them in its own held-send
 * queue and sends them when the one-to-one exists. A device paired meanwhile
 * receives copies through the pairing state bundle so the person sees what
 * they already wrote. A copy is never put into this device's held-send queue
 * and never sent from here: two senders would deliver each message twice.
 *
 * A copy waits under the pending contact's queue key (its account id, 32 hex
 * characters) and moves to the one-to-one (its group id, 64 hex characters)
 * when that lands here; it goes when a message from this account's other
 * device arrives in that one-to-one with the same text and the same number of
 * attachments, or [HELD_COPY_TTL_MS] after it was held.
 *
 * Stored sealed under the device's Keystore key, one file per account, behind
 * the account store gate (ADR-0077); wiped with the account.
 */

/** One display-only copy of a held message. */
@Serializable
public data class HeldCopy(
    val text: String,
    /** How many attachments the original carries. Only the count is kept:
     *  the refs are in the sending client's own form, used for matching. */
    val attachmentCount: Int = 0,
    /** The message id the original replies to, or `null`. */
    val replyTo: Long? = null,
    /** When the original was held on the sending device (unix ms). */
    val heldAtMs: Long,
)

/** A copy still unmatched this long after it was held is dropped. */
public const val HELD_COPY_TTL_MS: Long = 7L * 24 * 60 * 60 * 1000

/** At most this many copies are kept per key (the bundle's own bound). */
public const val MAX_HELD_COPIES_PER_KEY: Int = 200

/** The pure rules of the display copies (one definition each, tested). */
public object HeldCopies {
    /** [copies] without the ones held [HELD_COPY_TTL_MS] or longer ago. */
    public fun live(copies: List<HeldCopy>, nowMs: Long): List<HeldCopy> =
        copies.filter { nowMs - it.heldAtMs < HELD_COPY_TTL_MS }

    /** The index of the FIRST copy (in order) a message with [text] and
     *  [attachmentCount] attachments replaces, or `-1`. */
    public fun matchIndex(copies: List<HeldCopy>, text: String, attachmentCount: Int): Int =
        copies.indexOfFirst { it.text == text && it.attachmentCount == attachmentCount }

    /** [copies] with the first copy a message replaces removed, and its
     *  index (`-1` when none matched). */
    public fun replacedBy(copies: List<HeldCopy>, text: String, attachmentCount: Int): Pair<List<HeldCopy>, Int> {
        val at = matchIndex(copies, text, attachmentCount)
        if (at < 0) return copies to -1
        return copies.toMutableList().also { it.removeAt(at) } to at
    }

    /** [held] followed by the [carried] copies it does not hold yet (the
     *  same text at the same time), in order, bounded. */
    public fun merged(held: List<HeldCopy>, carried: List<HeldCopy>): List<HeldCopy> {
        val out = held.toMutableList()
        for (c in carried) {
            if (out.none { it.text == c.text && it.heldAtMs == c.heldAtMs && it.attachmentCount == c.attachmentCount }) {
                out += c
            }
        }
        return out.sortedBy { it.heldAtMs }.take(MAX_HELD_COPIES_PER_KEY)
    }
}

/** Where the copies of an account live: key (queue key or group id hex) to
 *  copies, in order. */
public interface HeldCopyBackend {
    public fun read(accountIdB64: String): String?

    /** `false` when the copies could not be written. */
    public fun write(accountIdB64: String, text: String?): Boolean
}

/** In-memory backend (tests). */
public class InMemoryHeldCopyBackend : HeldCopyBackend {
    private val rows = HashMap<String, String>()

    override fun read(accountIdB64: String): String? = synchronized(rows) { rows[accountIdB64] }

    override fun write(accountIdB64: String, text: String?): Boolean {
        synchronized(rows) { if (text == null) rows.remove(accountIdB64) else rows[accountIdB64] = text }
        return true
    }
}

/** Production backend: the sealed local store ([me.lakko.app.feed.FeedSealedStore]),
 *  whose write is behind the account store gate. */
public class SealedHeldCopyBackend(context: Context) : HeldCopyBackend {
    private val appContext = context.applicationContext

    override fun read(accountIdB64: String): String? =
        try {
            me.lakko.app.feed.FeedSealedStore.read(appContext, DIR, accountIdB64)
        } catch (_: Throwable) {
            null
        }

    override fun write(accountIdB64: String, text: String?): Boolean =
        try {
            if (text == null) {
                me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
                    me.lakko.app.feed.FeedSealedStore.delete(appContext, DIR, accountIdB64)
                    true
                } ?: false
            } else {
                me.lakko.app.feed.FeedSealedStore.write(appContext, DIR, accountIdB64, text)
            }
        } catch (_: Throwable) {
            false
        }

    public companion object {
        internal const val DIR: String = "held-copies"

        /** Forget the copies of the account (its data is wiped). */
        public fun deleteAccount(context: Context, accountIdB64: String) {
            try {
                me.lakko.app.feed.FeedSealedStore.delete(context.applicationContext, DIR, accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }
}

private val copiesJson = Json { ignoreUnknownKeys = true }
private val COPIES_MAP = MapSerializer(String.serializer(), ListSerializer(HeldCopy.serializer()))

internal fun encodeHeldCopies(map: Map<String, List<HeldCopy>>): String = copiesJson.encodeToString(COPIES_MAP, map)

internal fun decodeHeldCopies(raw: String?): Map<String, List<HeldCopy>> {
    if (raw.isNullOrEmpty()) return emptyMap()
    return try {
        copiesJson.decodeFromString(COPIES_MAP, raw)
    } catch (_: Exception) {
        emptyMap()
    }
}

/**
 * The per-account copy store. Every change is a read-modify-write under one
 * lock, so the pairing bundle's write and the chat repository's hand-over and
 * matching never lose each other's change. Expired copies are dropped on
 * every read.
 */
public class HeldCopyStore(
    private val backend: HeldCopyBackend,
    private val now: () -> Long = System::currentTimeMillis,
) {
    private val lock = Any()

    /** Every key's live copies. */
    public fun load(accountIdB64: String): Map<String, List<HeldCopy>> = synchronized(lock) {
        val t = now()
        decodeHeldCopies(backend.read(accountIdB64))
            .mapValues { (_, v) -> HeldCopies.live(v, t) }
            .filterValues { it.isNotEmpty() }
    }

    /** The live copies held under [key]. */
    public fun copies(accountIdB64: String, key: String): List<HeldCopy> = load(accountIdB64)[key].orEmpty()

    /** Apply [change] to the whole map and store it; returns the new map. */
    public fun update(
        accountIdB64: String,
        change: (Map<String, List<HeldCopy>>) -> Map<String, List<HeldCopy>>,
    ): Map<String, List<HeldCopy>> = synchronized(lock) {
        val t = now()
        val current = decodeHeldCopies(backend.read(accountIdB64))
            .mapValues { (_, v) -> HeldCopies.live(v, t) }
            .filterValues { it.isNotEmpty() }
        val next = change(current).filterValues { it.isNotEmpty() }
        if (next != current) backend.write(accountIdB64, if (next.isEmpty()) null else encodeHeldCopies(next))
        next
    }

    /** Add [carried] under [key], keeping what is held. */
    public fun addCarried(accountIdB64: String, key: String, carried: List<HeldCopy>) {
        if (carried.isEmpty()) return
        update(accountIdB64) { map -> map + (key to HeldCopies.merged(map[key].orEmpty(), carried)) }
    }

    /** Move the copies held under [fromKey] to [toKey] (a pending contact's
     *  one-to-one landed); returns what moved. */
    public fun move(accountIdB64: String, fromKey: String, toKey: String): List<HeldCopy> {
        var moved: List<HeldCopy> = emptyList()
        update(accountIdB64) { map ->
            moved = map[fromKey].orEmpty()
            if (moved.isEmpty()) {
                map
            } else {
                (map - fromKey) + (toKey to HeldCopies.merged(map[toKey].orEmpty(), moved))
            }
        }
        return moved
    }

    /** Remove the copy under [key] that a message with [text] and
     *  [attachmentCount] attachments replaces; returns it, or `null`. */
    public fun replace(accountIdB64: String, key: String, text: String, attachmentCount: Int): HeldCopy? {
        var removed: HeldCopy? = null
        update(accountIdB64) { map ->
            val list = map[key].orEmpty()
            val (rest, at) = HeldCopies.replacedBy(list, text, attachmentCount)
            if (at < 0) {
                map
            } else {
                removed = list[at]
                map + (key to rest)
            }
        }
        return removed
    }

    /** Drop every copy under [key]. */
    public fun clear(accountIdB64: String, key: String) {
        update(accountIdB64) { it - key }
    }

    public companion object {
        @Volatile
        private var instance: HeldCopyStore? = null

        /** The process-wide store over the sealed backend. */
        public fun onDevice(context: Context): HeldCopyStore {
            instance?.let { return it }
            return synchronized(this) {
                instance ?: HeldCopyStore(SealedHeldCopyBackend(context)).also { instance = it }
            }
        }
    }
}
