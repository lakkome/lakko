// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.Base64

/*
 * ADR-0052 decision 2 + `docs/ux/message-actions.md` sections 5 and 5.1 —
 * the pure rules of a forward.
 *
 * A forward is a NEW message from the forwarder that carries the original's
 * words and attachments, the original author's display name as a label, and
 * a reference to the original message for the one purpose of jumping to it.
 * The name is a snapshot and a courtesy, never a person: no username, no key,
 * no route (section 5). A forwarded forward keeps the ORIGINAL author's name
 * and origin, never the forwarder's, so a chain does not rewrite its own
 * history.
 *
 * Nothing here touches Compose, the repository, the network or the native
 * core, so every rule runs on the host JVM.
 */

/** What the bubble's attribution line says when no name was carried. */
public const val FORWARDED_LABEL: String = "Forwarded"

/** The attribution line: "Forwarded from <name>", or [FORWARDED_LABEL]. */
public fun forwardLineLabel(forward: ForwardRef): String {
    val name = forward.author?.trim().orEmpty()
    return if (name.isEmpty()) FORWARDED_LABEL else "Forwarded from $name"
}

/**
 * The text and attribution to send when [message] is forwarded out of the
 * conversation [originGroupIdHex], as section 5.1 specifies them.
 *
 * - The text is the message's own text verbatim — which is the EDITED text
 *   when it was edited, because an accepted edit replaces [ChatMessage.text]
 *   in place. The reply quote is never carried; reactions are never carried.
 * - The author is [authorLabel], whatever the timeline already calls that
 *   person ("You" for an own message), decided by the caller because the
 *   timeline is where that resolution lives. A blank label is no label.
 * - When [message] is itself a forward, the ORIGINAL author's name and the
 *   ORIGINAL origin are carried onward, as the source carried them — even
 *   when the source carried none, because falling back to the forwarder's
 *   name would be exactly the history rewrite section 5 forbids.
 * - Otherwise the origin is this conversation and this message's DS id. A row
 *   with no id (never acked) has no origin to name; the menu does not offer
 *   Forward on one, so this is defence in depth.
 * - ADR-0053 section 6 — when the SOURCE conversation is a one-to-one
 *   ([sourceIsOneToOne]) no origin is carried at all, neither this
 *   conversation's nor one a forwarded forward brought along. The author's
 *   name is kept.
 *
 * The attachments are not here: they are re-encrypted and re-uploaded by the
 * sender (ADR-0052 OQ1), which is I/O, not a rule.
 */
public fun forwardPayloadOf(
    message: ChatMessage,
    authorLabel: String?,
    originGroupIdHex: String,
    sourceIsOneToOne: Boolean = false,
): Pair<String, ForwardRef> {
    val source = message.forward
    val ref = if (source != null) {
        ForwardRef(
            author = source.author?.trim()?.takeIf { it.isNotEmpty() },
            originGroupIdHex = source.originGroupIdHex,
            originMessageId = source.originMessageId,
        )
    } else {
        val hasId = message.messageId != 0L
        ForwardRef(
            author = authorLabel?.trim()?.takeIf { it.isNotEmpty() },
            originGroupIdHex = if (hasId) originGroupIdHex else null,
            originMessageId = if (hasId) message.messageId else null,
        )
    }
    if (sourceIsOneToOne) {
        return message.text to ref.copy(originGroupIdHex = null, originMessageId = null)
    }
    return message.text to ref
}

/**
 * The parsed envelope's three fields folded into a [ForwardRef], or `null`
 * for an ordinary message.
 *
 * The origin is both-or-neither: the core already hands it that way, and a
 * group id that does not decode drops the pair as a whole rather than leaving
 * a message id pointing nowhere. A name with no origin, or an origin with no
 * name, are both real forwards.
 */
public fun forwardRefFrom(
    forwardedFrom: String?,
    originGroupB64: String?,
    originMessageId: Long?,
): ForwardRef? {
    val author = forwardedFrom?.trim()?.takeIf { it.isNotEmpty() }
    val originHex = originGroupB64?.let(::forwardOriginGroupIdHexFrom)
    val origin = if (originHex != null && originMessageId != null) {
        originHex to originMessageId
    } else {
        null
    }
    if (author == null && origin == null) return null
    return ForwardRef(
        author = author,
        originGroupIdHex = origin?.first,
        originMessageId = origin?.second,
    )
}

/**
 * The origin group id as the app keys groups: lowercase hex of the raw id the
 * envelope carries as STANDARD base64. `null` when it does not decode or is
 * empty.
 */
public fun forwardOriginGroupIdHexFrom(b64: String): String? {
    val bytes = try {
        Base64.getDecoder().decode(b64.trim())
    } catch (_: IllegalArgumentException) {
        return null
    }
    if (bytes.isEmpty()) return null
    val sb = StringBuilder(bytes.size * 2)
    for (b in bytes) {
        val v = b.toInt() and 0xFF
        sb.append(HEX_DIGITS[v ushr 4])
        sb.append(HEX_DIGITS[v and 0x0F])
    }
    return sb.toString()
}

/**
 * The inverse of [forwardOriginGroupIdHexFrom], for the send side: the
 * envelope's standard base64 of the group id the app holds as hex. `null` for
 * a string that is not hex, so a malformed id is dropped rather than sent.
 */
public fun forwardOriginGroupIdB64From(hex: String): String? {
    if (hex.isEmpty() || hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        val hi = Character.digit(hex[i * 2], 16)
        val lo = Character.digit(hex[i * 2 + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return Base64.getEncoder().encodeToString(out)
}

/**
 * Section 5.1 "Tap to jump" — where a tapped attribution takes the person, or
 * `null` when it takes them nowhere.
 *
 * Somewhere only when this device holds the origin conversation AND that
 * conversation's timeline holds the message. Either missing is `null`, and
 * the caller does nothing and says nothing: an explanation would confirm the
 * conversation exists (section 5). The threads are the repository's current
 * snapshot, so "holds" means what this device could show right now.
 */
public fun forwardJumpTarget(
    forward: ForwardRef,
    threads: List<ChatThread>,
): Pair<String, Long>? {
    val groupIdHex = forward.originGroupIdHex ?: return null
    val messageId = forward.originMessageId ?: return null
    if (messageId == 0L) return null
    val thread = threads.firstOrNull { it.groupIdHex == groupIdHex } ?: return null
    val held = thread.messages.any { it.messageId == messageId && it.direction != Direction.System }
    return if (held) groupIdHex to messageId else null
}

private const val HEX_DIGITS: String = "0123456789abcdef"
