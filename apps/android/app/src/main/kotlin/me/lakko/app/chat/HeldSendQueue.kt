// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json

/*
 * ADR-0065 R.7 case 1 - the DURABLE half of the held send.
 *
 * A message written into a one-to-one whose other person's reset this device
 * accepted is held and goes out when the conversation's successor exists
 * here (`HeldSend.kt`). That can be minutes away and the process can die in
 * between, so the held messages are stored: sealed under the device's
 * Keystore key (they are message plaintext), one file per account, rewritten
 * whole on every change. A held message survives a process death and is
 * flushed by the repository's own continuity pass, with or without a chat
 * screen open.
 *
 * What is stored is what the envelope is composed from: the text, the
 * attachment references (the blobs are already uploaded), the quote snapshot
 * with the group it was composed in (its jump target is dropped when it goes
 * into another segment), and the forward attribution. Mirror of the web
 * `chat/held-send-queue.ts`.
 */

/** One stored held message. */
@Serializable
public data class PersistedHeldSend(
    /** Hex of the conversation id (the lineage's first group). */
    val conversationHex: String,
    /** Hex of the group the message was composed in (the dead one-to-one). */
    val groupIdHex: String,
    /** When it was held, UNIX milliseconds. Reset when read back: the wait
     *  for the successor starts again with the process. */
    val heldAtMs: Long,
    val text: String = "",
    val attachments: List<PersistedAttachment> = emptyList(),
    val reply: PersistedReply? = null,
    val forward: PersistedForward? = null,
)

/** At most this many held messages are stored per account: the queue is
 *  bounded like every other local store. Chosen, not measured. */
public const val MAX_STORED_HELD_SENDS: Int = 500

private val heldJson = Json { ignoreUnknownKeys = true }

private val HEX_ID = Regex("^[0-9a-f]+$")

/** Whether a stored row is one this build can send again. */
internal fun heldRowUsable(row: PersistedHeldSend): Boolean =
    HEX_ID.matches(row.conversationHex) &&
        HEX_ID.matches(row.groupIdHex) &&
        row.heldAtMs >= 0 &&
        (row.text.isNotEmpty() || row.attachments.isNotEmpty())

/** The queue as stored text. Visible for the codec test. */
internal fun encodeHeldSends(rows: List<PersistedHeldSend>): String =
    heldJson.encodeToString(ListSerializer(PersistedHeldSend.serializer()), rows.takeLast(MAX_STORED_HELD_SENDS))

/** The inverse; a text that is not this schema is an empty queue, and a row
 *  that cannot be sent again is left out. Order is kept. */
internal fun decodeHeldSends(raw: String?): List<PersistedHeldSend> {
    if (raw.isNullOrEmpty()) return emptyList()
    return try {
        heldJson.decodeFromString(ListSerializer(PersistedHeldSend.serializer()), raw)
            .filter(::heldRowUsable)
            .takeLast(MAX_STORED_HELD_SENDS)
    } catch (_: Exception) {
        emptyList()
    }
}

/**
 * The rows read back at start, with the wait starting again: a message held
 * before the process died is not failed for the time the app was not running.
 */
public fun restoredHeldSends(stored: List<PersistedHeldSend>, nowMs: Long): List<PersistedHeldSend> =
    stored.map { it.copy(heldAtMs = nowMs) }

/** Where the held messages live between sessions. */
public interface HeldSendBackend {
    public fun load(accountIdB64: String): List<PersistedHeldSend>

    /** `false` when the queue could not be written. */
    public fun save(accountIdB64: String, rows: List<PersistedHeldSend>): Boolean
}

/** In-memory backend (tests). */
public class InMemoryHeldSendBackend : HeldSendBackend {
    private val rows = HashMap<String, String>()

    override fun load(accountIdB64: String): List<PersistedHeldSend> =
        synchronized(rows) { decodeHeldSends(rows[accountIdB64]) }

    override fun save(accountIdB64: String, rows: List<PersistedHeldSend>): Boolean {
        synchronized(this.rows) { this.rows[accountIdB64] = encodeHeldSends(rows) }
        return true
    }
}

/**
 * Production backend: one AES-GCM sealed file per account under the app's
 * private storage, the key in the Android Keystore
 * ([me.lakko.app.feed.FeedSealedStore], the same root and key as the other
 * sealed local stores).
 */
public class SealedHeldSendBackend(context: Context) : HeldSendBackend {
    private val appContext = context.applicationContext

    override fun load(accountIdB64: String): List<PersistedHeldSend> =
        try {
            decodeHeldSends(me.lakko.app.feed.FeedSealedStore.read(appContext, DIR, accountIdB64))
        } catch (_: Throwable) {
            emptyList()
        }

    override fun save(accountIdB64: String, rows: List<PersistedHeldSend>): Boolean =
        try {
            if (rows.isEmpty()) {
                me.lakko.app.feed.FeedSealedStore.delete(appContext, DIR, accountIdB64)
                true
            } else {
                me.lakko.app.feed.FeedSealedStore.write(appContext, DIR, accountIdB64, encodeHeldSends(rows))
            }
        } catch (_: Throwable) {
            false
        }

    public companion object {
        private const val DIR = "held-sends"

        /** Forget the held messages of the account (its data is wiped). */
        public fun deleteAccount(context: Context, accountIdB64: String) {
            try {
                me.lakko.app.feed.FeedSealedStore.delete(context.applicationContext, DIR, accountIdB64)
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }
}
