// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import uniffi.lakko_ffi_uniffi.LinkRecord
import uniffi.lakko_ffi_uniffi.LinkRecordKind

/**
 * ADR-0087 section 15 — `Add publisher` names one person: the owner sends a
 * one-use publisher link in their one-to-one, and the owner's device approves
 * that person's request without asking, since it named them.
 *
 * A request is approved without asking only when the capability it carried
 * is the one inside a live publisher link of this channel in the account's
 * link register, whichever device of the account minted it, and only when
 * that link is one use ([isOneUse]): a shareable link must never admit
 * whoever holds it. A link the
 * register shows as revoked (or used, which revokes it) is not live, so its
 * request waits for the owner like any other; so does a request whose link
 * the register does not hold.
 */
public object PublisherLinkMatch {

    /**
     * The register record of the live publisher link [capabilityWire] came
     * by, or `null`. [open] opens a record's sealed capability under its link
     * secret and returns the capability wire, or `null` when it does not open
     * (the core's `GroupInviteCapability.openLink`, injected for tests).
     */
    public fun linkOf(
        records: List<LinkRecord>,
        channelId: ByteArray,
        capabilityWire: ByteArray,
        nowMs: Long,
        open: (sealed: ByteArray, linkSecret: ByteArray) -> ByteArray?,
    ): LinkRecord? {
        if (capabilityWire.isEmpty()) return null
        for (r in records) {
            if (r.kind != LinkRecordKind.PUBLISHER) continue
            if (!r.targetId.contentEquals(channelId)) continue
            if (r.revoked) continue
            if (!isOneUse(r)) continue
            if (r.expiryMs != 0uL && r.expiryMs.toLong() <= nowMs) continue
            if (r.capability.isEmpty()) continue
            val wire = try {
                open(r.capability, r.linkSecret)
            } catch (_: Throwable) {
                null
            } ?: continue
            if (wire.contentEquals(capabilityWire)) return r
        }
        return null
    }

    /** The register's use limit of a one-use link (as the ledger counts it,
     *  the same value web files). */
    public const val ONE_USE: UInt = 0u

    /** Whether [record] serves one request at most: the register reads a
     *  limit of 0 and of 1 as one use (`LinkRecordEntry::effective_limit`). */
    public fun isOneUse(record: LinkRecord): Boolean = record.maxUses == 0u || record.maxUses == 1u

    /** The core's opener: the capability wire of a sealed link, or `null`. */
    public val coreOpen: (ByteArray, ByteArray) -> ByteArray? = { sealed, secret ->
        try {
            uniffi.lakko_ffi_uniffi.GroupInviteCapability.openLink(sealed, secret).use { it.wire() }
        } catch (_: Throwable) {
            null
        }
    }
}
