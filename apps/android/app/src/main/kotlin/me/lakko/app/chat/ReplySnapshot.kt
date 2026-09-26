// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.ui.chat.PreviewAttachment
import me.lakko.app.ui.chat.mediaPreviewLabel

/*
 * ADR-0052 decision 1 + `docs/ux/message-actions.md` section 3.1 — the pure
 * rules of a reply's quote.
 *
 * The quote is a SNAPSHOT: both strings are taken when Reply is chosen and
 * travel in the envelope as they were, so a later edit or deletion of the
 * original changes nothing, and a recipient who never held the original
 * reads the same quote as everyone else. What is snapshotted is the chosen
 * message's OWN text, one level deep — never the text it quoted.
 *
 * Nothing here touches Compose, the repository or the native core, so every
 * rule runs on the host JVM.
 */

/**
 * A reply the composer has armed: the target's id, and the quote taken at
 * that moment. [armedSeq] distinguishes two arms of the same message, so the
 * composer refocuses on each; it never leaves the device.
 */
public data class ReplyArmed(
    val messageId: Long,
    val snapshot: ReplyRef,
    val armedSeq: Long = 0,
)

/** What a reply's quote says when the envelope carried no excerpt. */
public const val REPLY_FALLBACK_EXCERPT: String = "Message"

/**
 * The quote to carry for [message], as section 3.1 specifies it.
 *
 * The name is [authorLabel] — whatever the timeline already calls that person
 * ("You" for an own message), decided by the caller because the timeline is
 * where that resolution lives. The excerpt is the text trimmed to one line; a
 * message with no text names the kind of its first attachment, through the
 * same word the conversation row uses for it ([mediaPreviewLabel]) so the two
 * cannot disagree. A message with neither carries no excerpt, and the receiver
 * then draws [REPLY_FALLBACK_EXCERPT].
 */
public fun replySnapshotFor(message: ChatMessage, authorLabel: String): ReplyRef {
    val oneLine = message.text.trim().replace(Regex("\\s+"), " ")
    val excerpt = when {
        oneLine.isNotEmpty() -> oneLine
        message.attachments.isNotEmpty() -> {
            val first = message.attachments.first()
            mediaPreviewLabel(PreviewAttachment(kind = first.kind, mime = first.mime))
        }
        else -> null
    }
    return ReplyRef(
        target = message.messageId,
        author = authorLabel.trim().takeIf { it.isNotEmpty() },
        excerpt = excerpt,
    )
}

/**
 * The two lines of a quote block, name then excerpt. A reply that arrived
 * with no snapshot at all draws the name line empty and the excerpt
 * [REPLY_FALLBACK_EXCERPT] (section 3.1); a blank string counts as absent.
 */
public fun replyQuoteLines(reply: ReplyRef): Pair<String, String> {
    val name = reply.author?.trim().orEmpty()
    val excerpt = reply.excerpt?.trim()?.takeIf { it.isNotEmpty() } ?: REPLY_FALLBACK_EXCERPT
    return name to excerpt
}
