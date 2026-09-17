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
            ClassifiedPost.Meta(SharedChannelMeta(m.name, m.description, m.updatedAtMs))
        }
    }
    FfiChannelPostKind.MALFORMED -> ClassifiedPost.Malformed
}

/** The FFI classification of a decrypted post plaintext. Never fails. */
public fun classifyChannelPlaintext(plaintext: ByteArray): ClassifiedPost =
    classifiedPostOf(classifyChannelPostPlaintext(plaintext))

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
    ) {
        val keys = state.keysByChannel.value[channelId]
            ?: throw IllegalStateException("no publisher keys for this channel on this device")
        val record = FfiChannelMeta(name = name.trim(), description = description.trim(), updatedAtMs = updatedAtMs)
        val plaintext = composeChannelMeta(record)
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
        client.publishPost(channelId, wire)
        state.adoptSharedMeta(channelId, SharedChannelMeta(record.name, record.description, record.updatedAtMs))
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
        return try {
            post(state, client, channelId, name, description, stamp)
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
    return SharedChannelMeta(name = sharedName, description = sharedDescription, updatedAtMs = stamp)
}
