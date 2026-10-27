// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.FolderConversationRef
import uniffi.lakko_ffi_uniffi.SavedMessageView

/*
 * docs/ux/saved-messages-and-file-lifetime.md sections 2, 3 — the pure parts
 * of Save: what a message's register carries, and how the Saved screen lists
 * them. The register itself lives in the synced state (`lakko-sync`, ADR-0065
 * section 4) and is written through `DeviceGroupSyncRepository`.
 */

/** The attachment kind token a saved snapshot carries; empty for none. */
public fun savedAttachmentKind(msg: ChatMessage): String {
    val ref = msg.attachments.firstOrNull() ?: return ""
    return when {
        ref.kind == me.lakko.app.media.KIND_VOICE -> "voice"
        ref.kind == me.lakko.app.media.KIND_VIDEO_NOTE -> "video_note"
        ref.kind == me.lakko.app.media.KIND_GIF -> "gif"
        ref.mime?.startsWith("image/") == true -> "photo"
        ref.mime?.startsWith("video/") == true -> "video"
        else -> "file"
    }
}

/** The excerpt a saved snapshot carries: the text, else a file's name. */
public fun savedExcerpt(msg: ChatMessage): String =
    msg.text.ifEmpty {
        msg.attachments.firstOrNull()?.takeIf { savedAttachmentKind(msg) == "file" }?.name.orEmpty()
    }

/** Every blob id of [msg]'s files and thumbnails, each once. */
public fun messageBlobIds(msg: ChatMessage): List<ByteArray> {
    val out = ArrayList<ByteArray>()
    for (ref in msg.attachments) {
        out += ref.blobId
        ref.thumb?.let { out += it.blobId }
    }
    return out.filter { it.size == 32 }.distinctBy { it.toList() }
}

/**
 * The maintainer's rule (2026-10-27): a saved message is named by the
 * SEGMENT that holds it, never by the root conversation — the MLS group id of
 * the group the message lives in (the current group, or an older segment of
 * the conversation, ADR-0073) and the DS id it has in that group. That pair
 * never changes when the conversation later moves to a successor group.
 */
public data class SavedKey(val segmentHex: String, val messageId: Long)

/** The key [msg] is saved under in a chat whose current group is
 *  [currentGroupHex]; `null` for a row with no DS id (not sent yet). */
public fun savedKeyOf(currentGroupHex: String, msg: ChatMessage): SavedKey? {
    val segment = msg.historyOf
    return if (segment == null) {
        msg.messageId.takeIf { it > 0 }?.let { SavedKey(currentGroupHex.lowercase(), it) }
    } else {
        msg.historyMessageId.takeIf { it > 0 }?.let { SavedKey(segment.lowercase(), it) }
    }
}

/** The row of the folded [thread] saved under [key] (a row of the thread's
 *  current group, or of one of its older segments), or `null`. */
public fun rowForSavedKey(thread: ChatThread, key: SavedKey): ChatMessage? =
    thread.messages.firstOrNull { savedKeyOf(thread.groupIdHex, it) == key }

/** One row of the Saved screen. */
public data class SavedRow(
    val conversation: FolderConversationRef,
    /** Hex of the SEGMENT group the message lives in (the register's id). */
    val conversationHex: String,
    val messageId: Long,
    val savedAtMs: Long,
    val author: String,
    val excerpt: String,
    val attachmentKind: String,
    /** The message's own time (ms), for the row's date: the snapshot's
     *  `sentAtMs`, else the held message's, else the save time. */
    val dateMs: Long,
    /** The message itself when this device holds it, else `null`: the row is
     *  then drawn from the snapshot alone (no thumbnail, does not open). */
    val held: ChatMessage?,
) {
    /** Section 3 — a row opens only once its message is here. */
    val opens: Boolean get() = held != null
}

/**
 * The Saved screen's rows: most recently saved first, a held message
 * attached where this device has it ([held] looks it up). Ties (two saves in
 * one millisecond, another device's clock) order by message id, newest
 * first, so the list never reshuffles on a redraw.
 */
public fun savedRows(
    saved: List<SavedMessageView>,
    held: (conversationHex: String, messageId: Long) -> ChatMessage?,
): List<SavedRow> =
    saved.map { v ->
        val hex = hexOf(v.conversation.id)
        val id = v.messageId.toLong()
        val message = held(hex, id)?.takeIf { !it.deleted }
        SavedRow(
            conversation = v.conversation,
            conversationHex = hex,
            messageId = id,
            savedAtMs = v.savedAtMs.toLong(),
            author = v.author,
            excerpt = v.excerpt,
            attachmentKind = v.attachmentKind,
            dateMs = savedRowDateMs(
                sentAtMs = v.sentAtMs.toLong(),
                heldAtMs = message?.createdAt?.let { me.lakko.app.ui.chat.parseMessageTimeMs(it) },
                savedAtMs = v.savedAtMs.toLong(),
            ),
            held = message,
        )
    }.sortedWith(compareByDescending<SavedRow> { it.savedAtMs }.thenByDescending { it.messageId })

/** UX section 3 — a Saved row shows the MESSAGE's date: the snapshot's,
 *  else the held message's, else (both unknown) the save time. */
public fun savedRowDateMs(sentAtMs: Long, heldAtMs: Long?, savedAtMs: Long): Long = when {
    sentAtMs > 0 -> sentAtMs
    heldAtMs != null && heldAtMs > 0 -> heldAtMs
    else -> savedAtMs
}

/** The second line of a row for media (`Photo`, `Voice message`,
 *  `File: name`), or `null` when the snapshot has no attachment kind. */
public fun savedKindText(kind: String, excerpt: String): UiText? = when (kind) {
    "photo" -> UiText("saved.kind.photo")
    "video" -> UiText("saved.kind.video")
    "voice" -> UiText("saved.kind.voice")
    "video_note" -> UiText("saved.kind.video_note")
    "gif" -> UiText("saved.kind.gif")
    "file" -> UiText("saved.kind.file", texts = mapOf("name" to excerpt))
    else -> null
}

/** Whether one more message may be saved (the client limit,
 *  `savedMaxClient()`; a removal is always allowed). */
public fun saveAllowed(currentCount: Int, adding: Int, max: Int): Boolean = currentCount + adding <= max

/** Whether [key] is in [saved] (matched on the segment group id and the DS
 *  id, whatever ref kind another device filed). */
public fun isSavedIn(saved: List<SavedMessageView>, key: SavedKey): Boolean =
    saved.any { it.messageId.toLong() == key.messageId && hexOf(it.conversation.id) == key.segmentHex }

/** The saved entries of [segmentHex] whose DS ids are in [deletedIds]
 *  (delete-for-everyone removes them from Saved). */
public fun savedDeletedIn(saved: List<SavedMessageView>, segmentHex: String, deletedIds: Set<Long>): List<SavedMessageView> =
    saved.filter { it.messageId.toLong() in deletedIds && hexOf(it.conversation.id) == segmentHex.lowercase() }

