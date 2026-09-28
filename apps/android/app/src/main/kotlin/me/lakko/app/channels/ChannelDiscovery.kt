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
