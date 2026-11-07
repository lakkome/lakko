// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test
import uniffi.lakko_ffi_uniffi.LinkRecord
import uniffi.lakko_ffi_uniffi.LinkRecordKind

/**
 * ADR-0087 section 15 — the owner's device approves without asking only a
 * request whose capability is inside a live publisher link of this channel in
 * the account's register (minted on any device); a revoked link or a link the
 * register does not hold leaves the request for the owner.
 */
class PublisherLinkMatchTest {

    private val channel = ByteArray(16) { 7 }
    private val otherChannel = ByteArray(16) { 8 }

    private fun record(
        tag: Byte,
        kind: LinkRecordKind = LinkRecordKind.PUBLISHER,
        target: ByteArray = channel,
        revoked: Boolean = false,
        expiryMs: ULong = 0uL,
        maxUses: UInt = PublisherLinkMatch.ONE_USE,
    ) = LinkRecord(
        hlc = 1uL,
        linkId = ByteArray(16) { tag },
        linkSecret = ByteArray(12) { tag },
        kind = kind,
        ledgerId = ByteArray(6) { tag },
        revocationSeed = ByteArray(32),
        expiryMs = expiryMs,
        maxUses = maxUses,
        uses = 0u,
        revoked = revoked,
        mintedAtMs = 1uL,
        label = "",
        answered = emptyList(),
        shared = false,
        editable = false,
        targetId = target,
        inviteSeed = ByteArray(0),
        capability = byteArrayOf(0x53, tag),
    )

    /** A fake opener: a sealed blob `S, tag` opens under secret `tag` to the
     *  capability wire `C, tag`. */
    private val open: (ByteArray, ByteArray) -> ByteArray? = { sealed, secret ->
        if (sealed.size == 2 && sealed[0] == 0x53.toByte() && secret.all { it == sealed[1] }) {
            byteArrayOf(0x43, sealed[1])
        } else {
            null
        }
    }

    private fun wire(tag: Byte) = byteArrayOf(0x43, tag)

    @Test
    fun `a request by a live publisher link of this channel matches its record`() {
        val mine = record(2)
        val records = listOf(record(1), mine, record(3))
        assertSame(mine, PublisherLinkMatch.linkOf(records, channel, wire(2), 1_000L, open))
    }

    @Test
    fun `a link the register shows as revoked is not approved without asking`() {
        val records = listOf(record(2, revoked = true))
        assertNull(PublisherLinkMatch.linkOf(records, channel, wire(2), 1_000L, open))
    }

    @Test
    fun `a request by a link the register does not hold waits for the owner`() {
        val records = listOf(record(1), record(3))
        assertNull(PublisherLinkMatch.linkOf(records, channel, wire(2), 1_000L, open))
        assertNull(PublisherLinkMatch.linkOf(emptyList(), channel, wire(2), 1_000L, open))
        assertNull(PublisherLinkMatch.linkOf(records, channel, ByteArray(0), 1_000L, open))
    }

    @Test
    fun `a shareable publisher link never admits a request without asking`() {
        // A link with no use limit (or any limit but one use) could be passed
        // on; its request waits for the owner's Approve.
        assertNull(PublisherLinkMatch.linkOf(listOf(record(2, maxUses = 0xFFFFFFFFu)), channel, wire(2), 1_000L, open))
        assertNull(PublisherLinkMatch.linkOf(listOf(record(2, maxUses = 5u)), channel, wire(2), 1_000L, open))
        // The register reads a limit of 1 as one use, like 0.
        val one = record(2, maxUses = 1u)
        assertSame(one, PublisherLinkMatch.linkOf(listOf(one), channel, wire(2), 1_000L, open))
    }

    @Test
    fun `only a live publisher link of this channel counts`() {
        assertNull(PublisherLinkMatch.linkOf(listOf(record(2, kind = LinkRecordKind.CHANNEL)), channel, wire(2), 1_000L, open))
        assertNull(PublisherLinkMatch.linkOf(listOf(record(2, kind = LinkRecordKind.GROUP)), channel, wire(2), 1_000L, open))
        assertNull(PublisherLinkMatch.linkOf(listOf(record(2, target = otherChannel)), channel, wire(2), 1_000L, open))
        assertNull(PublisherLinkMatch.linkOf(listOf(record(2, expiryMs = 500uL)), channel, wire(2), 1_000L, open))
        // A record whose capability does not open is not the request's link.
        assertNull(PublisherLinkMatch.linkOf(listOf(record(2)), channel, wire(2), 1_000L) { _, _ -> null })
    }
}
