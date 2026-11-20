// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.lakko_ffi_uniffi.LinkRecord
import uniffi.lakko_ffi_uniffi.LinkRecordKind

/**
 * Section 14.2 — Android's channel Invite link page had only a create
 * button: no expiry, no use limit, no list, no Revoke (web and iOS have all
 * four). The choices, the list of the channel's links and each link's terms
 * line, as web `ChannelInviteLinkScreen` draws them.
 */
class ChannelLinkTermsTest {
    private val now = 1_800_000_000_000L
    private val channel = "11".repeat(16)

    private fun record(
        target: String = channel,
        kind: LinkRecordKind = LinkRecordKind.CHANNEL,
        revoked: Boolean = false,
        mintedAt: Long = 1,
        label: String = "",
    ) = LinkRecord(
        hlc = 0uL,
        linkId = ByteArray(16) { mintedAt.toByte() },
        linkSecret = ByteArray(12),
        kind = kind,
        ledgerId = ByteArray(6),
        revocationSeed = ByteArray(32),
        expiryMs = 0uL,
        maxUses = 0u,
        uses = 0u,
        revoked = revoked,
        mintedAtMs = mintedAt.toULong(),
        label = label,
        answered = emptyList(),
        shared = false,
        editable = false,
        targetId = ByteArray(target.length / 2) { i -> target.substring(i * 2, i * 2 + 2).toInt(16).toByte() },
        inviteSeed = ByteArray(0),
        admissionVersion = 0u,
        capability = ByteArray(0),
    )

    @Test
    fun theChoicesMapToTheWireTerms() {
        assertEquals(0L, ChannelLinkTerms.expiryMs(ChannelLinkTerms.Expiry.NEVER, now))
        assertEquals(now + 7 * ChannelLinkTerms.DAY_MS, ChannelLinkTerms.expiryMs(ChannelLinkTerms.Expiry.SEVEN_DAYS, now))
        assertEquals(0L, ChannelLinkTerms.maxUses(ChannelLinkTerms.Uses.ONE))
        assertEquals(0xFFFFFFFFL, ChannelLinkTerms.maxUses(ChannelLinkTerms.Uses.UNLIMITED))
    }

    @Test
    fun theListIsThisChannelsLiveLinksNewestFirst() {
        val links = ChannelLinkTerms.linksOf(
            listOf(
                record(mintedAt = 1, label = "old"),
                record(mintedAt = 3, label = "new"),
                record(mintedAt = 2, revoked = true),
                record(mintedAt = 4, target = "22".repeat(16)),
                record(mintedAt = 5, kind = LinkRecordKind.PUBLISHER),
            ),
            channel.uppercase(),
        )
        assertEquals(listOf("new", "old"), links.map { it.label })
    }

    @Test
    fun theTermsLineSaysExpiryAndUsesAsWebDoes() {
        assertEquals(
            listOf(UiText("invite.links.never_expires"), UiText("invite.links.unlimited_uses")),
            ChannelLinkTerms.termsOf(0L, 0xFFFFFFFFL, 3, now),
        )
        assertEquals(
            listOf(UiText("invite.links.expired"), UiText("invite.links.used_up")),
            ChannelLinkTerms.termsOf(now - 1, 0L, 1, now),
        )
        assertEquals(
            listOf(UiText("invite.links.uses_left", counts = mapOf("count" to 7uL))),
            ChannelLinkTerms.termsOf(now + 1, 10L, 3, now),
        )
    }
}
