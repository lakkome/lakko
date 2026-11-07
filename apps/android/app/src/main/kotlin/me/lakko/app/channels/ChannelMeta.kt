// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.util.Log
import uniffi.lakko_ffi_uniffi.FfiChannelMeta
import uniffi.lakko_ffi_uniffi.FfiChannelPostContent
import uniffi.lakko_ffi_uniffi.FfiChannelPostKind
import uniffi.lakko_ffi_uniffi.channelMetaIsNewer
import uniffi.lakko_ffi_uniffi.classifyChannelPostPlaintext
import uniffi.lakko_ffi_uniffi.composeChannelMeta

/**
 * ADR-0051 §4 — the channel metadata record on Android.
 *
 * A channel's name and description travel as the PLAINTEXT of an ordinary
 * post: sealed under the epoch seed and signed by the publisher key like every
 * post, so a subscriber trusts the record for the same reason it trusts a
 * post (only a publisher can sign one). Readers classify every decrypted
 * plaintext BEFORE the text path — a record is never rendered as a post — and
 * keep the newest per channel by its stamp; the publisher re-issues the
 * current record after every key rotation so a late subscriber always finds
 * one it can read.
 *
 * The pure pieces ([newerChannelMeta], [ClassifiedPost]) are FFI-free so a JVM
 * test drives them; the two FFI touchpoints (`classifyChannelPostPlaintext`,
 * `composeChannelMeta`) are wrapped once here.
 */

/** A channel's shared name + description with the publisher's stamp. */
public data class SharedChannelMeta(
    val name: String,
    val description: String,
    val updatedAtMs: ULong,
    /** ADR-0059 metadata v2: the topic id (0 = Other). */
    val topic: Int = 0,
    /** ADR-0059 metadata v2: ISO 639 language, "" when not set. */
    val language: String = "",
    /** ADR-0059 metadata v2: the owner listed the channel in Discover.
     *  ADR-0087 section 16: since metadata v3 this is the PUBLIC flag. */
    val listed: Boolean = false,
    /** ADR-0087 section 18.2 — the record's own plaintext (base64), which a
     *  re-issue after a rotation re-posts verbatim with its @name, photo and
     *  reactions setting; "" when only the fields above are known. */
    val recordB64: String = "",
)

/** UX copy (docs/ux/group-and-channel-names.md section 5). */
public const val CHANNEL_NEEDS_A_NAME: String = "A channel needs a name"

/**
 * The reader's fold: `incoming` replaces `current` only when strictly newer
 * (the FFI `channelMetaIsNewer`, threaded in). Nothing held yet: the first
 * record wins. Returns the record to keep.
 */
public fun newerChannelMeta(
    current: SharedChannelMeta?,
    incoming: SharedChannelMeta,
    isNewer: (incomingMs: ULong, storedMs: ULong) -> Boolean,
): SharedChannelMeta {
    if (current == null) return incoming
    return if (isNewer(incoming.updatedAtMs, current.updatedAtMs)) incoming else current
}

/** What one decrypted plaintext is, after classification. */
public sealed interface ClassifiedPost {
    /** Ordinary text to render. */
    public data class Text(val text: String) : ClassifiedPost

    /** A metadata record: adopt if newer, never render. */
    public data class Meta(val meta: SharedChannelMeta) : ClassifiedPost

    /** A domain match that failed a bound: drop, never render as text. */
    public data object Malformed : ClassifiedPost

    /** ADR-0087 section 13a — a publisher-signed ban list: hides the comments
     *  of the keys it names, never rendered. */
    public data class BanList(val list: uniffi.lakko_ffi_uniffi.FfiBanList) : ClassifiedPost
}

/** Map the FFI classification onto [ClassifiedPost]; pure over the record. */
public fun classifiedPostOf(content: FfiChannelPostContent): ClassifiedPost = when (content.kind) {
    FfiChannelPostKind.TEXT ->
        ClassifiedPost.Text((content.text ?: ByteArray(0)).toString(Charsets.UTF_8))
    FfiChannelPostKind.META -> {
        val m = content.meta
        if (m == null) {
            ClassifiedPost.Malformed
        } else {
            ClassifiedPost.Meta(
                SharedChannelMeta(
                    name = m.name,
                    description = m.description,
                    updatedAtMs = m.updatedAtMs,
                    topic = m.topic.toInt(),
                    language = m.language,
                    listed = m.listed,
                ),
            )
        }
    }
    FfiChannelPostKind.MALFORMED -> ClassifiedPost.Malformed
    // ADR-0087 section 13a — moderation state for comments, never a post.
    FfiChannelPostKind.BAN_LIST -> content.banList?.let { ClassifiedPost.BanList(it) } ?: ClassifiedPost.Malformed
}

/** The FFI classification of a decrypted post plaintext. Never fails. A
 *  metadata record keeps its own plaintext ([SharedChannelMeta.recordB64]). */
public fun classifyChannelPlaintext(plaintext: ByteArray): ClassifiedPost =
    when (val c = classifiedPostOf(classifyChannelPostPlaintext(plaintext))) {
        is ClassifiedPost.Meta -> ClassifiedPost.Meta(
            c.meta.copy(recordB64 = java.util.Base64.getEncoder().encodeToString(plaintext)),
        )
        else -> c
    }

/** ADR-0087 section 18.2 — the full v3 record a [SharedChannelMeta] carries,
 *  decoded again from its stored plaintext; `null` when none is stored or it
 *  no longer decodes. */
public fun SharedChannelMeta.fullRecord(): FfiChannelMeta? {
    if (recordB64.isEmpty()) return null
    val bytes = try {
        java.util.Base64.getDecoder().decode(recordB64)
    } catch (_: IllegalArgumentException) {
        return null
    }
    return try {
        val c = classifyChannelPostPlaintext(bytes)
        if (c.kind == FfiChannelPostKind.META) c.meta else null
    } catch (_: Throwable) {
        null
    }
}

/** The canonical order, defined once in Rust. */
public fun channelMetaNewer(incomingMs: ULong, storedMs: ULong): Boolean =
    channelMetaIsNewer(incomingMs, storedMs)

/**
 * The publisher side: compose the record and post it through the normal
 * compose path (it consumes a counter and seals under the current key), then
 * file the publisher's own copy in the channel log.
 */
public object ChannelMetaPublisher {
    private const val TAG = "ChannelMetaPublisher"

    /**
     * Post `{ name, description }` stamped [updatedAtMs] into [channelId].
     * ADR-0059 metadata v2: [topic], [language] and [listed] default to what
     * this device holds for the channel, so a rename keeps a listed channel
     * listed; a channel never listed is topic Other, language unset, not listed.
     * Throws [uniffi.lakko_ffi_uniffi.FfiException.ChannelMeta] when the
     * record violates a bound (the message starts with the frozen reason
     * code), and whatever the compose / publish path throws on failure.
     */
    public suspend fun post(
        state: ChannelsUiState,
        client: ChannelsClient,
        channelId: String,
        name: String,
        description: String,
        updatedAtMs: ULong,
        topic: Int? = null,
        language: String? = null,
        listed: Boolean? = null,
        /** ADR-0087 section 3 — the signed @name; `null` keeps the held one. */
        handle: uniffi.lakko_ffi_uniffi.FfiConversationHandle? = null,
        /** Section 18.2 — the photo; `null` keeps the held one. */
        avatar: uniffi.lakko_ffi_uniffi.FfiAvatar? = null,
        clearAvatar: Boolean = false,
        /** Section 13a — the allowed reactions; `null` keeps the held one. */
        reactions: uniffi.lakko_ffi_uniffi.FfiAllowedReactions? = null,
    ) {
        val held = state.channels.value.firstOrNull { it.id == channelId }
        val heldRecord = held?.sharedMeta()?.fullRecord()
        val record = FfiChannelMeta(
            name = name.trim(),
            description = description.trim(),
            updatedAtMs = updatedAtMs,
            topic = (topic ?: held?.sharedTopic ?: 0).coerceIn(0, 255).toUByte(),
            language = language ?: held?.sharedLanguage ?: "",
            listed = listed ?: held?.sharedListed ?: false,
            handle = handle ?: heldRecord?.handle,
            avatar = if (clearAvatar) null else avatar ?: heldRecord?.avatar,
            reactions = when (reactions) {
                null -> heldRecord?.reactions
                is uniffi.lakko_ffi_uniffi.FfiAllowedReactions.All -> null
                else -> reactions
            },
        )
        postRecord(state, client, channelId, composeChannelMeta(record))
    }

    /** Seal and publish one composed record plaintext, then file it. */
    internal suspend fun postRecord(
        state: ChannelsUiState,
        client: ChannelsClient,
        channelId: String,
        plaintext: ByteArray,
    ) {
        val keys = state.keysByChannel.value[channelId]
            ?: throw IllegalStateException("no publisher keys for this channel on this device")
        val epoch = state.epochByChannel.value[channelId]
            ?: client.fetchEpoch(channelId).also { state.setEpoch(channelId, it) }
        val counter = state.nextCounter(channelId)
        val wire = ChannelsComposer.compose(
            keys = keys,
            channelIdHex = channelId,
            epochId = epoch,
            messageCounter = counter,
            plaintext = plaintext,
        )
        val publisher = state.publisher
            ?: throw IllegalStateException("channel publisher identity not available")
        // ADR-0059 section 9: the record is a post, signed with this
        // account's publisher role for the channel.
        ChannelPublisherSigners.derive(publisher, channelId).use { client.publishPost(channelId, wire, it) }
        when (val c = classifyChannelPlaintext(plaintext)) {
            is ClassifiedPost.Meta -> state.adoptSharedMeta(channelId, c.meta)
            else -> Unit
        }
    }

    /**
     * Re-issue the record this device holds for [channelId] — after a rotation
     * (ADR-0051 §4) — with the STORED stamp verbatim, never a fresh clock read.
     * A channel created before records existed re-issues its local name and
     * description under a fresh stamp instead. Best-effort: logs and returns
     * `false` on failure.
     */
    public suspend fun reissue(state: ChannelsUiState, client: ChannelsClient, channelId: String): Boolean {
        val channel = state.channels.value.firstOrNull { it.id == channelId } ?: return false
        val shared = channel.sharedMeta()
        val name = shared?.name ?: channel.localName.trim()
        if (name.isEmpty()) return false
        val description = shared?.description ?: channel.localDescription.trim()
        val stamp = shared?.updatedAtMs ?: System.currentTimeMillis().toULong()
        val stored = shared?.recordB64?.takeIf { it.isNotEmpty() }?.let {
            try {
                java.util.Base64.getDecoder().decode(it)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        return try {
            // ADR-0087 section 18.2 — the stored record verbatim, photo included.
            if (stored != null) {
                postRecord(state, client, channelId, stored)
            } else {
                post(state, client, channelId, name, description, stamp)
            }
            true
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            Log.w(TAG, "channel record not re-issued", cause)
            false
        }
    }
}

/** The shared record a [ChannelHandle] holds, or `null` when none has landed. */
public fun ChannelHandle.sharedMeta(): SharedChannelMeta? {
    val stamp = sharedUpdatedAtMs ?: return null
    if (sharedName.isBlank()) return null
    return SharedChannelMeta(
        name = sharedName,
        description = sharedDescription,
        updatedAtMs = stamp,
        topic = sharedTopic,
        language = sharedLanguage,
        listed = sharedListed,
        recordB64 = sharedRecordB64,
    )
}
