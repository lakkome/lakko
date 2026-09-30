// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import me.lakko.app.channels.ChannelsApiException
import me.lakko.app.channels.LegacyChannelException
import me.lakko.app.chat.AllowanceKind
import me.lakko.app.chat.anonTokenFailureText
import me.lakko.app.l10n.UiText

/*
 * ADR-0058 section 6 — what the channel screens say about a failed action.
 *
 * The channels service's `detail` prose is never shown (it goes to the log):
 * a known error code maps to `channels.error.*`, any other to
 * `channels.error.refused`. With a [prefixKey] the reason follows a fixed first
 * sentence through `common.sentence_pair` ("Could not publish. This post is too
 * long to publish."); without one a known code reads alone and anything else
 * is the action's [genericKey]. Mirror of the web `channelErrorText`
 * (`channels/api.ts`).
 */

/** The catalog key of the sentence for a channels service error code. */
internal fun channelsErrorCodeKey(code: String): String = when (code) {
    "channels-unreachable" -> "channels.error.unreachable"
    "unknown-channel" -> "channels.error.unknown_channel"
    "post-too-large" -> "channels.error.post_too_large"
    "empty-post" -> "channels.error.empty_post"
    "envelope-not-ready" -> "channels.error.not_let_in"
    else -> "channels.error.refused"
}

/** The sentence for a failed channel action (see the file comment). */
internal fun channelsErrorText(error: Throwable, genericKey: String, prefixKey: String? = null): UiText {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    var reasonKey: String? = null
    while (cause != null && seen.add(cause)) {
        if (cause is LegacyChannelException) return cause.text
        if (cause is ChannelsApiException) {
            reasonKey = channelsErrorCodeKey(cause.code)
            break
        }
        if (cause is java.io.IOException) {
            reasonKey = "channels.error.unreachable"
            break
        }
        cause = cause.cause
    }
    anonTokenFailureText(error, AllowanceKind.ACTION)?.let { return it }
    val reason = reasonKey ?: return UiText(genericKey)
    if (prefixKey == null) {
        return if (reason == "channels.error.refused") UiText(genericKey) else UiText(reason)
    }
    return UiText(
        "common.sentence_pair",
        parts = mapOf("first" to UiText(prefixKey), "second" to UiText(reason)),
    )
}

/**
 * The sentence for the core's refusal of a channel name or description
 * (`FfiException.ChannelMeta`, whose message opens with the frozen reason
 * code). Mirror of the web `channelMetaErrorText`.
 */
internal fun channelMetaErrorText(message: String?): UiText {
    val code = message?.substringBefore(": ")?.trim().orEmpty()
    return UiText(
        when (code) {
            "name.empty" -> "channels.meta.error.name_empty"
            "name.tooLong", "name.tooManyBytes" -> "channels.meta.error.name_too_long"
            "name.lineBreak" -> "channels.meta.error.name_line_break"
            "name.controlOrFormatCharacter" -> "channels.meta.error.name_bad_character"
            "description.tooLong", "description.tooManyBytes" -> "channels.meta.error.description_too_long"
            "description.controlOrFormatCharacter" -> "channels.meta.error.description_bad_character"
            else -> "channels.meta.error.generic"
        },
    )
}
