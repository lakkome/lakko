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
     * ADR-0087 section 6 — the 32-byte read key of a channel this account
     * owns, from the LINEAGE (conversation) identity [identity]. Public by
     * the owner's choice: the public record and the subscriber links carry it.
     */
    public fun readKey(identity: uniffi.lakko_ffi_uniffi.Identity, channelIdHex: String): ByteArray =
        uniffi.lakko_ffi_uniffi.channelReadKey(identity, ChannelsComposer.channelIdFromHex(channelIdHex))

    /**
     * ADR-0087 sections 1 to 4 — write a channel's @name on the handle
     * mechanism. One rule on all three clients (section 3 + ADR-0074): the
     * journal id is the PublisherGroup id the LINEAGE identity [identity]
     * derives for the channel; the handle key is derived from the journal root
     * of the CURRENT account seed over that id. [route] CLAIM spends one
     * `discovery-register` token, REPLACE ([oldHandle] set) one more, UPDATE
     * none. Throws [me.lakko.app.discovery.HandleWriteError] (`name-taken`)
     * or a network error.
     */
    public suspend fun writeHandle(
        identity: uniffi.lakko_ffi_uniffi.Identity,
        channelIdHex: String,
        handle: String,
        public: Boolean,
        name: String,
        description: String,
        route: me.lakko.app.discovery.HandleRoute,
        oldHandle: String? = null,
        sizeBand: Int = 0,
        avatar: uniffi.lakko_ffi_uniffi.FfiAvatar? = null,
    ): me.lakko.app.chat.SignedGroupHandle {
        val channelId = ChannelsComposer.channelIdFromHex(channelIdHex)
        val journalId = uniffi.lakko_ffi_uniffi.publisherGroupIdFor(identity, channelId)
        val currentSeed = ChannelOwnerSuccession.withOwnerIdentity(identity) { it.ed25519Seed() }
        val readKey = if (public) readKey(identity, channelIdHex) else ByteArray(32)
        val facts = me.lakko.app.chat.ChannelHandleFacts(public, name, description, readKey, sizeBand, avatar)
        try {
            return DiscoveryClient().use { client ->
                when (route) {
                    me.lakko.app.discovery.HandleRoute.REPLACE -> me.lakko.app.chat.replaceChannelHandle(
                        client, currentSeed, journalId, channelId, oldHandle ?: handle, handle, facts,
                    )
                    me.lakko.app.discovery.HandleRoute.UPDATE -> me.lakko.app.chat.updateChannelHandle(
                        client, currentSeed, journalId, channelId, handle, facts,
                    )
                    else -> me.lakko.app.chat.claimChannelHandle(
                        client, currentSeed, journalId, channelId, handle, facts,
                    )
                }
            }
        } finally {
            currentSeed.fill(0)
        }
    }

    /** The in-record form of a signed channel @name (metadata v3). */
    public fun conversationHandleOf(signed: me.lakko.app.chat.SignedGroupHandle): uniffi.lakko_ffi_uniffi.FfiConversationHandle =
        uniffi.lakko_ffi_uniffi.FfiConversationHandle(
            updatedAtMs = signed.stored.updatedAtMs,
            public = signed.stored.public,
            handle = signed.stored.handle,
            certificate = signed.stored.certificate,
            signature = signed.stored.signature,
        )

    /**
     * Section 3 + ADR-0074 — the owed `rekey` after this account's key
     * rotation, signed with the OLD handle key (kept at rotation by
     * `KeyRotationChannels`), moving the name to the current seed's key; then
     * the channel record is re-posted with the newly signed @name. `true` when
     * nothing is owed any more.
     */
    public suspend fun sendOwedRekeyIfAny(
        context: android.content.Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        accountIdB64: String,
        channel: ChannelHandle,
    ): Boolean {
        val store = me.lakko.app.chat.GroupSettingsStore
        val oldSeed = store.owedRekeySeed(context, accountIdB64, channel.id) ?: return true
        try {
            val held = channel.sharedMeta()?.fullRecord()?.handle
            if (held == null) {
                store.saveOwedRekeySeed(context, accountIdB64, channel.id, null)
                return true
            }
            val publisher = state.publisher ?: return false
            val signed = publisher.withIdentity(persist = false) { identity ->
                val channelId = ChannelsComposer.channelIdFromHex(channel.id)
                val journalId = uniffi.lakko_ffi_uniffi.publisherGroupIdFor(identity, channelId)
                ChannelOwnerSuccession.withOwnerIdentity(identity) { it.ed25519Seed() }
                    .let { cur -> journalId to cur }
            }.let { (journalId, currentSeed) ->
                try {
                    DiscoveryClient().use { d ->
                        me.lakko.app.chat.rekeyGroupHandle(d, oldSeed, currentSeed, journalId, held.handle, held.public)
                    }
                } finally {
                    currentSeed.fill(0)
                }
            }
            val shared = channel.sharedMeta()
            ChannelMetaPublisher.post(
                state = state,
                client = client,
                channelId = channel.id,
                name = shared?.name ?: channel.localName,
                description = shared?.description ?: channel.localDescription,
                updatedAtMs = System.currentTimeMillis().toULong(),
                handle = conversationHandleOf(signed),
            )
            store.saveOwedRekeySeed(context, accountIdB64, channel.id, null)
            return true
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            android.util.Log.i("ChannelDiscovery", "ADR-0087 owed channel handle rekey not sent yet")
            return false
        } finally {
            oldSeed.fill(0)
        }
    }

    /**
     * 4.5.4 — the owed republish of [channel]'s public record, when one is
     * recorded: sent, and cleared once it lands. A failure keeps it owed for
     * the next try (the publisher loop, the next edit). Returns whether
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
        val record = channel.sharedMeta()?.fullRecord()
        val handle = record?.handle?.handle ?: channel.handle.trim().removePrefix("@")
        if (handle.isEmpty()) {
            store.saveOwedChannelRecord(context, accountIdB64, channel.id, false)
            return true
        }
        return try {
            publisher.withIdentity(persist = false) { identity ->
                writeHandle(
                    identity = identity,
                    channelIdHex = channel.id,
                    handle = handle,
                    public = record?.listed ?: channel.sharedListed,
                    name = channel.sharedName.ifEmpty { channel.localName },
                    description = channel.sharedDescription.ifEmpty { channel.localDescription },
                    route = me.lakko.app.discovery.HandleRoute.UPDATE,
                    avatar = record?.avatar,
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

    /** The public channel [handle]'s record names, with the record's read key
     *  and name: what a Subscribe needs to read it at once ([subscribePublicChannel]). */
    public suspend fun resolvePublicChannel(handle: String): PublicChannelFound? =
        DiscoveryClient().use { it.lookupChannelRecord(handle) }?.asPublicChannel()

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
