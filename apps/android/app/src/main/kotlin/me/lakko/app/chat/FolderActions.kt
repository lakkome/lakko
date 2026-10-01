// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.util.Log
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.devices.sync.Hlc
import uniffi.lakko_ffi_uniffi.FolderChangeSet
import uniffi.lakko_ffi_uniffi.mintFolderId

/**
 * Every folder edit the screens make, turned into ONE change set and handed to
 * the sync runtime, which applies it here at once and sends it to the
 * account's other devices (ADR-0058; UX spec section 6: an edit that cannot be
 * sent still applies here and travels later).
 *
 * The change sets themselves are built by the pure functions in
 * `ChatFolders.kt`; this only stamps and delivers them.
 */
public class FolderActions(
    private val sync: DeviceGroupSyncRepository,
    private val chats: ChatRepository,
) {
    private val stamp: StampSource = { Hlc.mint() }

    private suspend fun deliver(changes: FolderChangeSet?): Boolean {
        if (changes == null || changes.isEmpty()) return true
        return try {
            sync.applyFolderChanges(changes)
            true
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(TAG, "folder change refused (${cause.javaClass.simpleName})")
            false
        }
    }

    /** Save the editor: create when [original] is `null`. */
    public suspend fun save(original: ChatFolder?, draft: FolderDraft): Boolean {
        val id = original?.id ?: mintFolderId()
        return deliver(saveChanges(id, original, draft, listedFolderIds(), stamp))
    }

    public suspend fun delete(folder: ChatFolder): Boolean = deliver(deleteChanges(folder.id, stamp))

    public suspend fun reorder(folders: List<ChatFolder>): Boolean =
        deliver(orderChanges(folders.map { it.id }, stamp))

    /** "Add to folder" / "Remove from folder" on one row (UX spec 5). */
    public suspend fun toggleMembership(folder: ChatFolder, row: ConversationListRow, inFolder: Boolean): Boolean {
        val ref = refOf(row) ?: return false
        val state = membershipToggle(folder, row, inFolder) ?: return true
        if (membershipToggleBlocked(folder, row, state)) return false
        return deliver(memberChange(folder.id, ref, state, stamp))
    }

    /** Pin / Unpin inside a folder: the folder's own pins (UX spec 2.3). */
    public suspend fun togglePin(folder: ChatFolder, row: ConversationListRow): Boolean {
        val ref = refOf(row) ?: return false
        val pinned = folder.pinned.any { refNames(it, row) }
        return deliver(pinChanges(folder, ref, pin = !pinned, stamp = stamp))
    }

    public suspend fun addSuggestion(s: FolderSuggestion): Boolean =
        deliver(suggestionChanges(mintFolderId(), s, listedFolderIds(), stamp))

    /** The folders as the fold lists them now: what a creation appends to. */
    private fun listedFolderIds(): List<ByteArray> = sync.folders.value.map { it.folderId }

    /**
     * Mark all as read (UX spec 2.2): for each unread conversation, the path
     * opening it takes - the local count cleared as if it had been on screen,
     * and the read cursor moved to its newest message for the other devices -
     * plus clearing a manual "mark as unread". Channels have no unread state.
     */
    public suspend fun markAllRead(rows: List<ConversationListRow>) {
        val threads = chats.state.value.threads.associateBy { it.groupIdHex }
        for (row in rows) {
            if (row.kind == ConversationRowKind.CHANNEL || !row.hasUnread) continue
            val groupId = bytesOfHex(row.id) ?: continue
            chats.setThreadOnScreen(row.id, true)
            chats.setThreadOnScreen(row.id, false)
            val highest = threads[row.id]?.messages?.maxOfOrNull { it.messageId } ?: -1L
            try {
                if (highest >= 0) {
                    sync.setReadCursor(
                        groupId = groupId,
                        upToMessageId = beI64(highest),
                        readAtMs = System.currentTimeMillis().toULong(),
                    )
                }
                if (row.unreadOverride) {
                    val flags = sync.flagsFor(groupId)
                    sync.setFlags(
                        groupId = groupId,
                        pinned = flags?.pinned ?: false,
                        muted = flags?.muted ?: false,
                        archived = flags?.archived ?: false,
                        unreadOverride = false,
                    )
                }
            } catch (cancellation: kotlinx.coroutines.CancellationException) {
                throw cancellation
            } catch (cause: Throwable) {
                Log.w(TAG, "mark read failed (${cause.javaClass.simpleName})")
            }
        }
    }

    private companion object {
        const val TAG = "ChatFolders"

        /** 8 big-endian bytes, the read cursor's wire shape. */
        fun beI64(value: Long): ByteArray {
            val out = ByteArray(8)
            var v = value
            for (i in 7 downTo 0) {
                out[i] = (v and 0xff).toByte()
                v = v ushr 8
            }
            return out
        }
    }
}
