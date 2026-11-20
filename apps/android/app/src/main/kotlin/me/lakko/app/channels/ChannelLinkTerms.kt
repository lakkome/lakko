// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.LinkRecord
import uniffi.lakko_ffi_uniffi.LinkRecordKind

/**
 * `docs/ux/group-and-channel-privacy.md` section 14.2 — a channel's invite
 * links: the terms a new link is made with (the web `ChannelInviteLinkScreen`
 * and iOS `ChannelInviteLinkScreen` choices: expiry never / 1 / 7 / 30 days,
 * uses unlimited / one) and the list of the channel's links in the account's
 * link register, with the line each shows. Pure.
 */
public object ChannelLinkTerms {
    public const val DAY_MS: Long = 24L * 60 * 60 * 1000

    /** When a new link stops working. */
    public enum class Expiry(public val days: Int) { NEVER(0), ONE_DAY(1), SEVEN_DAYS(7), THIRTY_DAYS(30) }

    /** How many people a new link lets in. */
    public enum class Uses { UNLIMITED, ONE }

    /** The wire expiry (unix ms) of [choice] from [nowMs], `0` never. */
    public fun expiryMs(choice: Expiry, nowMs: Long): Long = if (choice == Expiry.NEVER) 0L else nowMs + choice.days * DAY_MS

    /** The wire use limit: `0` one use, `0xFFFFFFFF` none (the service's and
     *  the ledger's sentinels). */
    public fun maxUses(choice: Uses): Long = if (choice == Uses.ONE) 0L else ChannelInviteLinks.UNLIMITED_USES

    /** The channel's live links in the register, newest first. */
    public fun linksOf(records: List<LinkRecord>, channelIdHex: String): List<LinkRecord> =
        records
            .filter { it.kind == LinkRecordKind.CHANNEL && !it.revoked }
            .filter { ChannelsComposer.bytesToHex(it.targetId).equals(channelIdHex, ignoreCase = true) }
            .sortedByDescending { it.mintedAtMs }

    /**
     * The terms line of a link (web `termsOf`): expired or never expires,
     * then unlimited, used up or the uses left. [used] is what the service
     * counted for the link's invite key, `0` when unknown.
     */
    public fun termsOf(expiryMs: Long, maxUses: Long, used: Long, nowMs: Long): List<UiText> {
        val parts = ArrayList<UiText>()
        if (expiryMs != 0L && expiryMs < nowMs) {
            parts += UiText("invite.links.expired")
        } else if (expiryMs == 0L) {
            parts += UiText("invite.links.never_expires")
        }
        if (maxUses == ChannelInviteLinks.UNLIMITED_USES) {
            parts += UiText("invite.links.unlimited_uses")
        } else {
            val left = maxOf(0L, (if (maxUses == 0L) 1L else maxUses) - used)
            parts += if (left == 0L) {
                UiText("invite.links.used_up")
            } else {
                UiText("invite.links.uses_left", counts = mapOf("count" to left.toULong()))
            }
        }
        return parts
    }
}
