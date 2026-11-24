// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

/**
 * A public channel as its `@name` record names it (ADR-0087 sections 4 and
 * 6): what a Subscribe needs to read the channel at once. The record carries
 * the channel's read key, so its public seeds open with no publisher
 * involved (`docs/ux/group-and-channel-privacy.md` 7.2: a public channel has
 * no waiting state).
 */
public class PublicChannelFound(
    /** The 32-char lower-case hex channel id. */
    public val idHex: String,
    /** The canonical `@name`, without the `@`. */
    public val handle: String,
    /** The channel's name from the record; may be empty. */
    public val name: String,
    /** The channel's read key from the record (32 bytes). */
    public val readKey: ByteArray,
)

/** A search result that is a channel, as [PublicChannelFound]. */
public fun me.lakko.app.chat.FoundConversation.asPublicChannel(): PublicChannelFound =
    PublicChannelFound(
        idHex = id.joinToString("") { "%02x".format(it) },
        handle = handle,
        name = name,
        readKey = knockOrReadKey,
    )

/**
 * The one Subscribe of a public channel found by its record, from a search
 * result or a Discover card alike: keep the record's read key for the bound
 * account ([keepReadKey], `ChannelReadKeys.put`), then open the channel
 * subscribing under the record's name ([open]). A device with no bound account
 * keeps nothing and still opens; the channel then reads as unknown.
 */
public fun subscribePublicChannel(
    found: PublicChannelFound,
    accountIdB64: String?,
    keepReadKey: (accountIdB64: String, channelIdHex: String, readKey: ByteArray) -> Unit,
    open: (channelIdHex: String, handle: String, name: String) -> Unit,
) {
    accountIdB64?.let { keepReadKey(it, found.idHex, found.readKey) }
    open(found.idHex, found.handle, found.name)
}
