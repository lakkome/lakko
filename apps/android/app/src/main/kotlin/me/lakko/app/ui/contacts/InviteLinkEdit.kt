// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

/*
 * The Invite links sheet after the 2026-10-26 change (UX
 * `docs/ux/contact-invite-link.md` section 9, ADR-0079 amendment of the same
 * day), as pure rules: the edit form prefilled from a link's terms, and what
 * a row's synced record allows (the pencil, a confirmation before Delete).
 */

private const val MINUTE_MS: Long = 60_000L
private const val HOUR_MS: Long = 60 * MINUTE_MS
private const val DAY_MS: Long = 24 * HOUR_MS

/** The terms Create link mints at once: no label, 7 days, one person. */
public val INVITE_DEFAULT_PARAMS: InviteCreateParams = InviteCreateForm().toParams()!!

/**
 * What the account's synced link record says of a listed link, joined to the
 * row by its ledger id. A row with no record (the register not loaded yet,
 * or a link only this device's store knows) has none: it is not editable
 * and Delete asks first.
 */
public data class InviteLinkSync(
    /** The record's 16-byte link id: what Copy marks shared. */
    val linkId: ByteArray,
    /** The core's rule (`LinkRecordEntry::editable`): not revoked, never
     *  copied, never answered. */
    val editable: Boolean,
    /** Copied on one of the account's devices. */
    val shared: Boolean,
    /** Somebody was accepted through it. */
    val used: Boolean,
) {
    override fun equals(other: Any?): Boolean =
        other is InviteLinkSync && linkId.contentEquals(other.linkId) &&
            editable == other.editable && shared == other.shared && used == other.used

    override fun hashCode(): Int =
        ((linkId.contentHashCode() * 31 + editable.hashCode()) * 31 + shared.hashCode()) * 31 + used.hashCode()
}

/** The facts of one synced [record] the sheet needs. */
public fun inviteLinkSync(record: uniffi.lakko_ffi_uniffi.LinkRecord): InviteLinkSync = InviteLinkSync(
    linkId = record.linkId,
    editable = record.editable,
    shared = record.shared,
    used = record.uses > 0u || record.answered.isNotEmpty(),
)

/** The synced facts of each contact link by its slug (base64url of the
 *  ledger id), the key the sheet's rows carry. */
public fun inviteLinkSyncBySlug(records: List<uniffi.lakko_ffi_uniffi.LinkRecord>): Map<String, InviteLinkSync> =
    records
        .filter { it.kind == uniffi.lakko_ffi_uniffi.LinkRecordKind.CONTACT && it.ledgerId.isNotEmpty() }
        .associate { me.lakko.app.auth.base64UrlEncodeNoPad(it.ledgerId) to inviteLinkSync(it) }

/** The pencil shows only when the row's record says so; no record, no pencil. */
public fun inviteRowEditable(sync: InviteLinkSync?): Boolean = sync?.editable == true

/** Delete asks first unless the link was never copied and never used; a row
 *  with no record asks, since neither is known. */
public fun inviteDeleteNeedsConfirmation(sync: InviteLinkSync?): Boolean =
    sync == null || sync.shared || sync.used

/** Within a minute counts as the preset: the record's mint time is taken a
 *  network round trip after the expiry was computed. */
private fun near(a: Long, b: Long): Boolean = kotlin.math.abs(a - b) <= MINUTE_MS

/**
 * The edit form prefilled from a link's terms. Expiry: `0` is Never; a
 * period within a minute of 1, 7 or 30 days from [mintedAtMs] is that
 * preset; any other period is Custom, in days when it is whole days (within
 * a minute), else in hours (rounded, at least one). Uses: the single-use
 * sentinel (or 1) is One, 10 Up to 10, `UInt.MAX_VALUE` Unlimited, anything
 * else Custom with that count. With no mint time a dated expiry is Custom
 * from [nowMs] (rounded the same way).
 */
public fun inviteFormFromTerms(
    label: String,
    expiryMs: Long,
    mintedAtMs: Long?,
    maxUses: Long,
    nowMs: Long = System.currentTimeMillis(),
): InviteCreateForm {
    var form = InviteCreateForm(label = clampInviteLabel(label))
    form = if (expiryMs == 0L) {
        form.copy(expiry = InviteExpiryChoice.Never)
    } else {
        val period = expiryMs - (mintedAtMs ?: nowMs)
        val preset = when {
            mintedAtMs == null -> null
            near(period, DAY_MS) -> InviteExpiryChoice.OneDay
            near(period, 7 * DAY_MS) -> InviteExpiryChoice.SevenDays
            near(period, 30 * DAY_MS) -> InviteExpiryChoice.ThirtyDays
            else -> null
        }
        if (preset != null) {
            form.copy(expiry = preset)
        } else {
            val days = Math.round(period.toDouble() / DAY_MS)
            if (days >= 1 && near(period, days * DAY_MS)) {
                form.copy(
                    expiry = InviteExpiryChoice.Custom,
                    customExpiryAmount = days.toString(),
                    customExpiryUnit = InviteExpiryUnit.Days,
                )
            } else {
                val hours = Math.round(period.toDouble() / HOUR_MS).coerceAtLeast(1)
                form.copy(
                    expiry = InviteExpiryChoice.Custom,
                    customExpiryAmount = hours.toString(),
                    customExpiryUnit = InviteExpiryUnit.Hours,
                )
            }
        }
    }
    form = when (maxUses) {
        0L, 1L -> form.copy(uses = InviteUsesChoice.One)
        10L -> form.copy(uses = InviteUsesChoice.UpToTen)
        INVITE_UNLIMITED_USES -> form.copy(uses = InviteUsesChoice.Unlimited)
        else -> form.copy(uses = InviteUsesChoice.Custom, customUses = maxUses.toString())
    }
    return form
}
