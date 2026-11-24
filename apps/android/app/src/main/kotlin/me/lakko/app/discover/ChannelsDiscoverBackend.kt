// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import android.content.Context
import me.lakko.app.anon.isAnonTokenQuotaExhausted
import me.lakko.app.channels.ChannelDiscovery
import me.lakko.app.channels.ChannelsApiException
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.l10n.UiText

/**
 * The production [DiscoverBackend]: the directory routes of `services/channels`
 * ([ChannelsClient]), the OPRF handle lookup ([ChannelDiscovery]), the core's
 * listing codec and topic list, and the sealed [DiscoverStore].
 */
public class ChannelsDiscoverBackend(
    private val context: Context,
    private val client: ChannelsClient,
) : DiscoverBackend {
    override suspend fun fetchBucket(topic: Int, language: String): List<DirectoryItemWire> =
        client.fetchBucket(topic, language)

    override suspend fun fetchListing(channelIdHex: String): DirectoryItemWire? = client.fetchListing(channelIdHex)

    override suspend fun search(query: String): List<DirectoryItemWire> = client.searchDirectory(query)

    override suspend fun resolveHandle(handle: String): me.lakko.app.channels.PublicChannelFound? =
        ChannelDiscovery.resolvePublicChannel(handle)

    override fun decodeListing(listingB64: String): DecodedListing? = DiscoverListing.decodeListingB64(listingB64)

    override fun topics(): List<Int> =
        uniffi.lakko_ffi_uniffi.channelTopics().map { it.id.toInt() }

    override fun loadPrefs(accountIdB64: String): DiscoverPrefs = DiscoverStore.loadPrefs(context, accountIdB64)

    override fun savePrefs(accountIdB64: String, prefs: DiscoverPrefs) {
        DiscoverStore.savePrefs(context, accountIdB64, prefs)
    }

    override fun searchErrorText(error: Throwable): UiText = searchErrorCopy(error)

    override fun lookupErrorText(error: Throwable): UiText =
        if (isAnonTokenQuotaExhausted(error)) UiText("tokens.lookup_allowance_used") else DiscoverCopy.UNAVAILABLE
}

/** The line for a failed search (UX spec section 7): a used-up allowance or a
 *  `429` reads "Too many searches"; anything else, unavailable. */
public fun searchErrorCopy(error: Throwable): UiText = when {
    isAnonTokenQuotaExhausted(error) -> UiText("tokens.search_allowance_used")
    error is ChannelsApiException && error.status == 429 -> UiText("tokens.search_allowance_used")
    else -> DiscoverCopy.UNAVAILABLE
}
