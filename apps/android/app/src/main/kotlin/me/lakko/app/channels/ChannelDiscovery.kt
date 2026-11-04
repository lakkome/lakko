// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import me.lakko.app.discovery.DiscoveryClient

/**
 * `@channel` discovery — Android client wiring for G4-5 / ADR-0010.
 *
 * A channel's on-the-wire identity is a 16-byte `channel_id`
 * pseudonym (hex-encoded on every `services/channels` REST route).
 * That is unfriendly to type or share, so — exactly like
 * `@username` — a channel may claim a human-readable `@handle`. The
 * mapping lives in the zero-trust discovery service behind the SAME
 * OPRF mechanism as usernames
 * (`services/discovery/src/channel_names.rs`): the server stores
 * only `OPRF(K, normalize(@handle)) -> channel_id`, never the
 * plaintext handle.
 *
 * Mirror of `apps/web/src/channels/channel-discovery.ts`. This is a
 * thin channel-facing wrapper over
 * [DiscoveryClient.registerChannelName] / [DiscoveryClient.lookupChannel]
 * (which own the multi-round-trip PoW → blind → evaluate → finalize
 * → PoW → register/lookup dance and reuse the UniFFI `OprfBlinder`
 * blind/finalize primitives — no OPRF is reimplemented here). It
 * only bridges the hex `channel_id` the channels UI speaks to the
 * raw-16-byte form the discovery client speaks.
 */
public object ChannelDiscovery {

    /** Raw 16-byte channel_id ⇒ 32 lowercase hex chars. */
    private const val CHANNEL_ID_HEX_LEN: Int = 32

    /**
     * `true` when [input] is a bare 32-char hex `channel_id` (16
     * bytes). Lets the subscribe entry point accept EITHER a pasted
     * raw id OR a typed `@handle` from one text field without a mode
     * switch — a hex id is taken verbatim, anything else is resolved
     * as a handle.
     */
    public fun looksLikeChannelIdHex(input: String): Boolean {
        val trimmed = input.trim()
        return trimmed.length == CHANNEL_ID_HEX_LEN &&
            trimmed.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }
    }

    /**
     * Register the `@handle → channel_id` mapping at channel-create
     * time. Anonymous: it spends a `discovery-lookup` token (the OPRF)
     * and a `discovery-register` token (ADR-0054); nothing binds the
     * handle to the account, ownership lives at the PublisherGroup
     * layer.
     *
     * @param handle       User-typed handle, with or without a
     *                     leading `@` — normalised inside
     *                     `OprfBlinder.blind`.
     * @param channelIdHex The freshly-minted 32-char hex channel id.
     * @param sessionToken Kept for the caller's shape; the register
     *                     itself goes out without a session.
     * @return `true` once the handle is bound.
     * @throws me.lakko.app.discovery.DiscoveryHttpError with
     *   `status == 409` when the handle is already taken
     *   (first-come-first-served).
     */
    public suspend fun registerChannelHandle(
        handle: String,
        channelIdHex: String,
        sessionToken: String,
    ): Boolean {
        val channelId = ChannelsComposer.channelIdFromHex(channelIdHex)
        return DiscoveryClient().use { client ->
            client.setSessionToken(sessionToken)
            client.registerChannelName(channelId, handle)
        }
    }

    /**
     * Resolve a typed `@handle` to its channel id hex, or `null`
     * when no channel has claimed that handle. Unauthenticated (the
     * lookup path is open); anonymous-safe.
     *
     * @param handle User-typed handle, with or without a leading `@`.
     * @return 32-char hex `channel_id`, or `null` on an unknown
     *   handle (the server's 404 not-found path).
     */
    /**
     * ADR-0087 section 1 — claim a channel's `@handle` on the handle
     * mechanism (the separate channel-name routes are gone): the write key
     * from the channel's PublisherGroup journal root, one `discovery-lookup`
     * and one `discovery-register` token. Throws
     * [me.lakko.app.discovery.HandleWriteError] (`name-taken`) or a network
     * error.
     */
    public suspend fun claimChannelHandle(
        identity: uniffi.lakko_ffi_uniffi.Identity,
        handle: String,
        channelIdHex: String,
        name: String,
        description: String,
    ) {
        val channelId = ChannelsComposer.channelIdFromHex(channelIdHex)
        val journalId = CoPublisher.openPublisherGroup(identity, channelIdHex).use { it.groupId() }
        DiscoveryClient().use { client ->
            me.lakko.app.chat.claimChannelHandle(
                discovery = client,
                accountSeed = identity.ed25519Seed(),
                journalId = journalId,
                channelId = channelId,
                handle = handle,
                name = name,
                description = description,
            )
        }
    }

    /** group-and-channel-privacy.md 4.5.4 — republish the channel's public
     *  record (its name and description) under its @name. The owner only. */
    public suspend fun updateChannelHandle(
        identity: uniffi.lakko_ffi_uniffi.Identity,
        handle: String,
        channelIdHex: String,
        name: String,
        description: String,
    ) {
        val channelId = ChannelsComposer.channelIdFromHex(channelIdHex)
        val journalId = CoPublisher.openPublisherGroup(identity, channelIdHex).use { it.groupId() }
        DiscoveryClient().use { client ->
            me.lakko.app.chat.updateChannelHandle(
                discovery = client,
                accountSeed = identity.ed25519Seed(),
                journalId = journalId,
                channelId = channelId,
                handle = handle,
                name = name,
                description = description,
            )
        }
    }

    /**
     * 4.5.4 — the owed republish of [channelIdHex]'s record, when one is
     * recorded: sent, and cleared once it lands. A failure keeps it owed for
     * the next try (the channel's heartbeat, the next edit). Returns whether
     * nothing is owed any more.
     */
    public suspend fun sendOwedRecordIfAny(
        context: android.content.Context,
        publisher: ChannelPublisherIdentity,
        accountIdB64: String,
        channel: ChannelHandle,
    ): Boolean {
        val store = me.lakko.app.chat.GroupSettingsStore
        if (!store.owedChannelRecord(context, accountIdB64, channel.id)) return true
        val handle = channel.handle.trim().removePrefix("@")
        if (handle.isEmpty()) {
            store.saveOwedChannelRecord(context, accountIdB64, channel.id, false)
            return true
        }
        return try {
            publisher.withIdentity { identity ->
                updateChannelHandle(
                    identity = identity,
                    handle = handle,
                    channelIdHex = channel.id,
                    name = channel.sharedName.ifEmpty { channel.localName },
                    description = channel.sharedDescription.ifEmpty { channel.localDescription },
                )
            }
            store.saveOwedChannelRecord(context, accountIdB64, channel.id, false)
            true
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            android.util.Log.i("ChannelDiscovery", "ADR-0087 4.5.4 channel record republish not sent yet")
            false
        }
    }

    public suspend fun resolveChannelHandle(handle: String): String? =
        DiscoveryClient().use { it.lookupChannel(handle) }

    /**
     * Convenience for the subscribe entry point: accept EITHER a
     * pasted 32-char hex `channel_id` OR a typed `@handle`, and
     * return the resolved channel id hex (or `null` when a handle
     * does not resolve). A bare hex id is returned verbatim
     * (lowercased) with no network round-trip; anything else is
     * OPRF-resolved.
     */
    public suspend fun resolveChannelIdOrHandle(input: String): String? {
        val trimmed = input.trim()
        if (trimmed.isEmpty()) return null
        if (looksLikeChannelIdHex(trimmed)) {
            return trimmed.lowercase()
        }
        return resolveChannelHandle(trimmed)
    }
}
