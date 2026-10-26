// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

/*
 * The "Create link" form of the Invite links sheet, as pure data: the
 * presets, the custom-field validation and the mapping to what
 * `ContactsRepository.generateInviteLink` takes. The same choices and bounds
 * as the web `invite.ts` (`expiryPresetToMs`, `customExpiryMs`,
 * `customMaxUses`) and iOS.
 */

/** Hard cap on the label, in UTF-8 bytes. Mirrors
 *  `lakko_contacts::invite::MAX_LABEL_LEN` (and the web
 *  `INVITE_LABEL_MAX_LEN`); a sprint that bumps the Rust constant bumps
 *  this one. */
public const val INVITE_LABEL_MAX_BYTES: Int = 64

private const val HOUR_MS: Long = 3_600_000L
private const val DAY_MS: Long = 24 * HOUR_MS

/** The longest custom period a link may be given: 365 days. Chosen, not
 *  measured; the same bound on web and iOS. */
public const val INVITE_CUSTOM_EXPIRY_MAX_MS: Long = 365 * DAY_MS

/** The largest custom number of people a link may admit. Chosen, not
 *  measured; the same bound on web and iOS. */
public const val INVITE_CUSTOM_MAX_USES: Long = 10_000L

/** Wire `max_uses` sentinel for one person (`SINGLE_USE`). */
public val INVITE_SINGLE_USE_WIRE: UInt = 0u

/** Wire `max_uses` for unlimited ([INVITE_UNLIMITED_USES]). */
public val INVITE_UNLIMITED_USES_WIRE: UInt = UInt.MAX_VALUE

public enum class InviteExpiryChoice { OneDay, SevenDays, ThirtyDays, Never, Custom }

public enum class InviteExpiryUnit { Hours, Days }

public enum class InviteUsesChoice { One, UpToTen, Unlimited, Custom }

/** What the form holds. The defaults are the form as it opens. */
public data class InviteCreateForm(
    val label: String = "",
    val expiry: InviteExpiryChoice = InviteExpiryChoice.SevenDays,
    val customExpiryAmount: String = "",
    val customExpiryUnit: InviteExpiryUnit = InviteExpiryUnit.Days,
    val uses: InviteUsesChoice = InviteUsesChoice.One,
    val customUses: String = "",
)

/** What a valid form creates: the expiry as a period from now (`null` =
 *  never expires, the wire `NEVER_EXPIRES` sentinel `0`) and the wire
 *  `max_uses`. */
public data class InviteCreateParams(
    val label: String,
    val expiryMillisFromNow: Long?,
    val maxUses: UInt,
)

/** The custom period in milliseconds, or `null` when [amount] is not a whole
 *  number or the period is outside 1 hour .. 365 days. */
public fun customInviteExpiryMs(amount: String, unit: InviteExpiryUnit): Long? {
    val n = parseWholeNumber(amount) ?: return null
    val ms = n * if (unit == InviteExpiryUnit.Hours) HOUR_MS else DAY_MS
    return ms.takeIf { it in HOUR_MS..INVITE_CUSTOM_EXPIRY_MAX_MS }
}

/** The wire `max_uses` for a custom number of people, or `null` when [count]
 *  is not a whole number in 1 .. [INVITE_CUSTOM_MAX_USES]. One person is the
 *  single-use sentinel `0`, as the preset writes it. */
public fun customInviteMaxUses(count: String): UInt? {
    val n = parseWholeNumber(count) ?: return null
    if (n < 1 || n > INVITE_CUSTOM_MAX_USES) return null
    return if (n == 1L) INVITE_SINGLE_USE_WIRE else n.toUInt()
}

/** Show the custom-expiry error: Custom is chosen and the field holds text
 *  that is not a valid period (an empty field only disables Create). */
public fun InviteCreateForm.showsCustomExpiryError(): Boolean =
    expiry == InviteExpiryChoice.Custom &&
        customExpiryAmount.isNotBlank() &&
        customInviteExpiryMs(customExpiryAmount, customExpiryUnit) == null

/** Show the custom-uses error, by the same rule. */
public fun InviteCreateForm.showsCustomUsesError(): Boolean =
    uses == InviteUsesChoice.Custom &&
        customUses.isNotBlank() &&
        customInviteMaxUses(customUses) == null

/** The parameters the form creates, or `null` while it is not valid (Create
 *  is disabled). */
public fun InviteCreateForm.toParams(): InviteCreateParams? {
    val expiryFromNow: Long? = when (expiry) {
        InviteExpiryChoice.OneDay -> 1 * DAY_MS
        InviteExpiryChoice.SevenDays -> 7 * DAY_MS
        InviteExpiryChoice.ThirtyDays -> 30 * DAY_MS
        InviteExpiryChoice.Never -> null
        InviteExpiryChoice.Custom -> customInviteExpiryMs(customExpiryAmount, customExpiryUnit) ?: return null
    }
    val maxUses: UInt = when (uses) {
        InviteUsesChoice.One -> INVITE_SINGLE_USE_WIRE
        InviteUsesChoice.UpToTen -> 10u
        InviteUsesChoice.Unlimited -> INVITE_UNLIMITED_USES_WIRE
        InviteUsesChoice.Custom -> customInviteMaxUses(customUses) ?: return null
    }
    val trimmed = label.trim()
    if (trimmed.toByteArray(Charsets.UTF_8).size > INVITE_LABEL_MAX_BYTES) return null
    return InviteCreateParams(label = trimmed, expiryMillisFromNow = expiryFromNow, maxUses = maxUses)
}

/** [text] cut, at a code-point boundary, to [INVITE_LABEL_MAX_BYTES] UTF-8
 *  bytes: the label field never holds more than the core accepts. */
public fun clampInviteLabel(text: String): String {
    if (text.toByteArray(Charsets.UTF_8).size <= INVITE_LABEL_MAX_BYTES) return text
    val out = StringBuilder()
    var bytes = 0
    var i = 0
    while (i < text.length) {
        val cp = text.codePointAt(i)
        val len = String(Character.toChars(cp)).toByteArray(Charsets.UTF_8).size
        if (bytes + len > INVITE_LABEL_MAX_BYTES) break
        out.appendCodePoint(cp)
        bytes += len
        i += Character.charCount(cp)
    }
    return out.toString()
}

private fun parseWholeNumber(text: String): Long? {
    val trimmed = text.trim()
    if (trimmed.isEmpty() || trimmed.length > 9 || !trimmed.all { it in '0'..'9' }) return null
    return trimmed.toLong()
}
