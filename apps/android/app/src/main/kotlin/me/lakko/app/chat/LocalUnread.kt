// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/**
 * A per-chat unread count built from what THIS device's polls apply.
 *
 * ## Why it exists
 *
 * The chat list's unread badge used to come only from the cross-device read
 * cursor (`DeviceGroupSyncRepository.readCursors`). A chat with no cursor —
 * never opened on any device, or on a device whose DeviceGroup broadcast
 * failed, which also skips the local cursor write — counted every message as
 * read, so a message arriving in a chat that was not open showed no badge at
 * all. The preview line already updated from the all-groups poll; the badge
 * did not.
 *
 * ## What it counts, honestly
 *
 * Incoming chat messages (not edits, reactions, typing, receipts or system
 * rows) applied while the chat was NOT on screen, since this process started or
 * since the chat was last on screen. It is in memory: a restart forgets it, and
 * the cursor-based count is what remains then. It knows nothing about what
 * another device read; [combinedUnread] folds the two.
 *
 * Thread-safe: polls of different groups apply concurrently, and the screen
 * flag is flipped from the main thread.
 */
internal class LocalUnreadCounter {
    private data class Entry(val count: Int, val onScreen: Boolean, val viewedThisSession: Boolean)

    private val lock = Any()
    private val entries = HashMap<String, Entry>()

    /** [incoming] chat messages were applied for [groupIdHex]. Ignored while it is on screen. */
    fun onIncomingApplied(groupIdHex: String, incoming: Int) {
        if (incoming <= 0) return
        synchronized(lock) {
            val current = entries[groupIdHex] ?: Entry(0, onScreen = false, viewedThisSession = false)
            if (current.onScreen) return
            entries[groupIdHex] = current.copy(count = current.count + incoming)
        }
    }

    /** The chat's screen became visible (clears the count) or stopped being visible. */
    fun setOnScreen(groupIdHex: String, onScreen: Boolean) {
        synchronized(lock) {
            val current = entries[groupIdHex] ?: Entry(0, onScreen = false, viewedThisSession = false)
            entries[groupIdHex] = if (onScreen) {
                Entry(count = 0, onScreen = true, viewedThisSession = true)
            } else {
                current.copy(onScreen = false)
            }
        }
    }

    /** The current count and whether this process has shown the chat. */
    fun snapshot(groupIdHex: String): LocalUnread = synchronized(lock) {
        val entry = entries[groupIdHex]
        LocalUnread(count = entry?.count ?: 0, viewedThisSession = entry?.viewedThisSession == true)
    }

    /** Logout / forget device. */
    fun clear() {
        synchronized(lock) { entries.clear() }
    }
}

/** What [LocalUnreadCounter] knows about one chat. */
internal data class LocalUnread(val count: Int, val viewedThisSession: Boolean)

/**
 * The badge count, from the cross-device cursor count and the local count.
 *
 *  - The chat was on screen in this process: this device knows exactly what
 *    arrived since, so the local count stands — capped by the cursor count
 *    when another device read further.
 *  - It was not: every locally counted message is also above any cursor, so the
 *    larger of the two is the better answer (the cursor count covers what
 *    arrived before this process started; the local count covers a chat with no
 *    cursor at all).
 *
 * @param cursorCount unread rows above the cross-device read cursor, or `null`
 *   when there is no cursor for the chat.
 */
internal fun combinedUnread(cursorCount: Int?, local: LocalUnread): Int =
    if (local.viewedThisSession) {
        cursorCount?.let { minOf(it, local.count) } ?: local.count
    } else {
        maxOf(cursorCount ?: 0, local.count)
    }
