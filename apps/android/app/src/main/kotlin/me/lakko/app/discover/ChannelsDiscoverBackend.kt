// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import android.content.Context
import me.lakko.app.anon.LOOKUP_ALLOWANCE_USED_COPY
import me.lakko.app.anon.SEARCH_ALLOWANCE_USED_COPY
import me.lakko.app.anon.isAnonTokenQuotaExhausted
import me.lakko.app.channels.ChannelDiscovery
import me.lakko.app.channels.ChannelsApiException
import me.lakko.app.channels.ChannelsClient

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

    override suspend fun resolveHandle(handle: String): String? = ChannelDiscovery.resolveChannelHandle(handle)

    override fun decodeListing(listingB64: String): DecodedListing? = DiscoverListing.decodeListingB64(listingB64)

    override fun topics(): List<Pair<Int, String>> =
        uniffi.lakko_ffi_uniffi.channelTopics().map { it.id.toInt() to it.name }

    override fun loadPrefs(accountIdB64: String): DiscoverPrefs = DiscoverStore.loadPrefs(context, accountIdB64)

    override fun savePrefs(accountIdB64: String, prefs: DiscoverPrefs) {
        DiscoverStore.savePrefs(context, accountIdB64, prefs)
    }

    override fun searchErrorText(error: Throwable): String = searchErrorCopy(error)

    override fun lookupErrorText(error: Throwable): String =
        if (isAnonTokenQuotaExhausted(error)) LOOKUP_ALLOWANCE_USED_COPY else DiscoverCopy.UNAVAILABLE
}

/** The line for a failed search (UX spec section 7): a used-up allowance or a
 *  `429` reads "Too many searches"; anything else, unavailable. */
public fun searchErrorCopy(error: Throwable): String = when {
    isAnonTokenQuotaExhausted(error) -> SEARCH_ALLOWANCE_USED_COPY
    error is ChannelsApiException && error.status == 429 -> SEARCH_ALLOWANCE_USED_COPY
    else -> DiscoverCopy.UNAVAILABLE
}
