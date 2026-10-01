// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import me.lakko.app.channels.ChannelMetaPublisher
import me.lakko.app.channels.ChannelPublisherIdentity
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.channels.sharedMeta
import uniffi.lakko_ffi_uniffi.FfiChannelListing
import uniffi.lakko_ffi_uniffi.composeChannelListing
import uniffi.lakko_ffi_uniffi.decodeChannelListing

/**
 * The owner's side of Discover (ADR-0059 sections 2-3, docs/ux/feed.md 6.4):
 * list a channel, withdraw it, and keep the listing in step with a rename.
 * Mirror of `apps/web/src/discover/listing.ts`.
 *
 * Listing writes two things, in this order:
 *
 *   1. the public listing (`PUT /v1/channels/{id}/listing`, owner-signed): if
 *      the server refuses it the switch goes back to off and nothing else
 *      happens;
 *   2. channel metadata v2 with `listed = true` and the topic and language,
 *      an ordinary encrypted post, so subscribers learn the channel is listed
 *      (what lets them share it in a group, section 6).
 *
 * Withdrawing deletes the listing and posts metadata with `listed = false`.
 * The owner key is derived from the account identity the shared
 * [ChannelPublisherIdentity] holds; a co-publisher's derivation is refused by
 * the server (`401`).
 */
public object DiscoverListing {

    /** The listing bytes of [b64] decoded by the core codec, or `null`. */
    public fun decodeListingB64(b64: String): DecodedListing? =
        try {
            val bytes = java.util.Base64.getUrlDecoder().decode(b64)
            val r = decodeChannelListing(bytes)
            DecodedListing(
                channelIdHex = r.channelId.joinToString("") { "%02x".format(it) },
                handle = r.handle,
                name = r.name,
                description = r.description,
                topic = r.topic.toInt(),
                language = r.language,
                updatedAtMs = r.updatedAtMs,
            )
        } catch (_: Throwable) {
            null
        }

    /** What the server currently lists for a channel: its stamp, or `null`
     *  when not listed. A listing this client cannot decode reads as 0. */
    public suspend fun listedStamp(client: ChannelsClient, channelIdHex: String): ULong? {
        val item = client.fetchListing(channelIdHex) ?: return null
        return decodeListingB64(item.listing_b64)?.updatedAtMs ?: 0uL
    }

    /** Whether this account holds the channel's OWNER key. `null` when the
     *  server could not answer. */
    public suspend fun probeOwner(
        client: ChannelsClient,
        publisher: ChannelPublisherIdentity,
        channelIdHex: String,
    ): Boolean? {
        val signer = publisher.withIdentity(persist = false) { ChannelsClient.ownerSigner(it, channelIdHex) }
        return signer.use { client.probeChannelOwner(channelIdHex, it) }
    }

    private data class Source(val handle: String, val name: String, val description: String)

    private fun source(state: ChannelsUiState, channelIdHex: String): Source {
        val c = state.channels.value.firstOrNull { it.id == channelIdHex }
        val meta = c?.sharedMeta()
        return Source(
            handle = c?.handle.orEmpty(),
            name = (meta?.name ?: c?.localName.orEmpty()).trim(),
            description = (meta?.description ?: c?.localDescription.orEmpty()).trim(),
        )
    }

    private suspend fun putCurrent(
        state: ChannelsUiState,
        client: ChannelsClient,
        publisher: ChannelPublisherIdentity,
        channelIdHex: String,
        topic: Int,
        language: String,
        stored: ULong?,
    ) {
        val src = source(state, channelIdHex)
        if (src.handle.isEmpty()) throw ListingNeedsHandle()
        val bytes = composeChannelListing(
            FfiChannelListing(
                channelId = ChannelsClient.channelIdBytes(channelIdHex),
                handle = src.handle,
                name = src.name,
                description = src.description,
                topic = topic.coerceIn(0, 255).toUByte(),
                language = language,
                updatedAtMs = nextListingStamp(System.currentTimeMillis(), stored),
            ),
        )
        val signer = publisher.withIdentity(persist = false) { ChannelsClient.ownerSigner(it, channelIdHex) }
        signer.use { client.putListing(channelIdHex, it, bytes) }
    }

    /** List the channel. Throws when the listing PUT fails ("Couldn't list")
     *  or when the metadata post fails after it (the listing is then live). */
    public suspend fun listChannel(
        state: ChannelsUiState,
        client: ChannelsClient,
        publisher: ChannelPublisherIdentity,
        channelIdHex: String,
        topic: Int,
        language: String,
    ) {
        val stored = try {
            listedStamp(client, channelIdHex)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }
        putCurrent(state, client, publisher, channelIdHex, topic, language, stored)
        val src = source(state, channelIdHex)
        ChannelMetaPublisher.post(
            state = state,
            client = client,
            channelId = channelIdHex,
            name = src.name,
            description = src.description,
            updatedAtMs = System.currentTimeMillis().toULong(),
            topic = topic,
            language = language,
            listed = true,
        )
    }

    /** Withdraw the listing and tell subscribers the channel is no longer listed. */
    public suspend fun unlistChannel(
        state: ChannelsUiState,
        client: ChannelsClient,
        publisher: ChannelPublisherIdentity,
        channelIdHex: String,
    ) {
        val signer = publisher.withIdentity(persist = false) { ChannelsClient.ownerSigner(it, channelIdHex) }
        signer.use { client.deleteListing(channelIdHex, it) }
        val src = source(state, channelIdHex)
        ChannelMetaPublisher.post(
            state = state,
            client = client,
            channelId = channelIdHex,
            name = src.name,
            description = src.description,
            updatedAtMs = System.currentTimeMillis().toULong(),
            listed = false,
        )
    }

    /** After a rename (ADR-0051 edit) of a listed channel: re-put the listing so
     *  it carries the new name and description. No-op when not listed. */
    public suspend fun refreshListingAfterEdit(
        state: ChannelsUiState,
        client: ChannelsClient,
        publisher: ChannelPublisherIdentity,
        channelIdHex: String,
    ) {
        val c = state.channels.value.firstOrNull { it.id == channelIdHex } ?: return
        if (!c.sharedListed || c.sharedLanguage.isEmpty()) return
        val stored = listedStamp(client, channelIdHex) ?: return
        putCurrent(state, client, publisher, channelIdHex, c.sharedTopic, c.sharedLanguage, stored)
    }
}

/** The channel has no `@handle`, so it cannot be listed (the listing needs one). */
public class ListingNeedsHandle : IllegalStateException("a channel needs a handle to be listed")
