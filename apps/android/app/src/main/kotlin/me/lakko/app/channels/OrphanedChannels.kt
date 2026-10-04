// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/**
 * ADR-0065 phase 2 / `docs/ux/identity-recovery.md` 16.6 — the channels a
 * reset orphaned, on the device that reset the keys.
 *
 * A channel's owner key derives from the account seed (ADR-0059,
 * `lakko/channels/owner/v1`), so a reset orphans every channel the person
 * owned: no device can post to it, rotate it or close it any more, including
 * this one. The server would say so with a `401` on the next post; the UI
 * needs the state BEFORE the person tries, and the only place that knows is
 * this device at the moment of the reset — which is when [recordAtReset]
 * runs, reading the channels this account holds a publisher signing seed for.
 *
 * What the client cannot know, and does not invent: a subscriber has no
 * signal at all (16.6), and a channel whose seed was never on this device is
 * not listed here. The set is per account, plain preferences (channel ids
 * only), cleared with the account or channel by channel through `Remove from
 * this device`.
 */
public class OrphanedChannelsStore(private val prefs: SharedPreferences) {

    public fun load(accountIdB64: String): Set<String> =
        try {
            prefs.getStringSet(key(accountIdB64), emptySet()).orEmpty().toSet()
        } catch (_: Throwable) {
            emptySet()
        }

    public fun save(accountIdB64: String, channelIds: Set<String>) {
        prefs.edit().putStringSet(key(accountIdB64), channelIds).apply()
    }

    /** Add [channelIds] to the account's set. */
    public fun add(accountIdB64: String, channelIds: Collection<String>) {
        if (channelIds.isEmpty()) return
        save(accountIdB64, load(accountIdB64) + channelIds)
    }

    /** `Remove from this device` — the channel leaves the set. */
    public fun remove(accountIdB64: String, channelId: String) {
        val current = load(accountIdB64)
        if (channelId !in current) return
        save(accountIdB64, current - channelId)
    }

    public fun deleteAccount(accountIdB64: String) {
        prefs.edit().remove(key(accountIdB64)).apply()
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-orphaned-channels"
        private const val KEY_PREFIX = "orphaned-v1:"

        private fun key(accountIdB64: String): String =
            KEY_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

        public fun onDevice(context: Context): OrphanedChannelsStore =
            OrphanedChannelsStore(
                context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE),
            )

        /**
         * At the point of no return of a key reset: the channels this account
         * holds a publisher signing seed for are the ones it owned under the
         * old keys. Best-effort; a read failure records nothing (the server's
         * `401` on the next post is then the only signal, as before).
         */
        public fun recordAtReset(context: Context, accountIdB64: String) {
            val owned = try {
                publisherChannelIds(ChannelPublisherKeyStore.encryptedPrefs(context), accountIdB64)
            } catch (cause: Throwable) {
                android.util.Log.w("OrphanedChannels", "owned channels not read at reset", cause)
                return
            }
            onDevice(context).add(accountIdB64, owned)
        }

        /** The channel ids with a persisted publisher signing seed for the
         *  account, from the row [ChannelsUiState] writes. Visible for tests. */
        internal fun publisherChannelIds(publisherPrefs: SharedPreferences, accountIdB64: String): Set<String> {
            val raw = publisherPrefs.getString(
                ChannelsUiState.accountKey(ChannelsUiState.PUBLISHER_SIGNING_SEEDS_PREF_KEY, accountIdB64),
                null,
            ) ?: return emptySet()
            return publisherChannelIdsOf(raw)
        }

        /** The ids in the persisted `[{channelId, signingSeedHex}]` row.
         *  Parsed with kotlinx.serialization (the `org.json` classes are stubs
         *  on the host JVM, where this is tested). */
        internal fun publisherChannelIdsOf(raw: String): Set<String> {
            val root = runCatching { Json.parseToJsonElement(raw) }.getOrNull() as? JsonArray ?: return emptySet()
            val out = LinkedHashSet<String>()
            for (element in root) {
                val row = element as? JsonObject ?: continue
                val id = (row["channelId"] as? JsonPrimitive)?.takeIf { it.isString }?.content ?: continue
                if (id.isNotEmpty()) out += id
            }
            return out
        }
    }
}

/**
 * 16.6 — the owner's view of an orphaned channel: no composer, no rotation, no
 * subscriber management; one notice and one control. Pure, for the test.
 */
public fun channelIsOrphaned(channelId: String, orphaned: Set<String>): Boolean = channelId in orphaned

/** 16.6 — the publisher keys of an orphaned channel are never re-opened: the
 *  publish path would only reach the server's refusal. */
public fun channelsToReopenExcludingOrphaned(
    candidates: List<ChannelHandle>,
    orphaned: Set<String>,
): List<ChannelHandle> = candidates.filter { it.id !in orphaned }
