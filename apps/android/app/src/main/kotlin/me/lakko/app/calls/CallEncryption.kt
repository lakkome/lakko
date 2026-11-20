// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.joinList

/**
 * How a call says it is encrypted — `docs/ux/calls.md` sections 4 and 5.
 *
 * ## The defect this closes
 *
 * The call screen printed the raw safety number across its top: sixteen digits
 * in a monospace pill, permanently, on the most valuable line of the screen.
 * Section 5 is blunt about why that is wrong — it is unreadable, it means
 * nothing to anyone who has not been told what it is, and it occupies the line
 * the call's own state should be on.
 *
 * The new rule is one sentence: **the code is never shown unless the user asks
 * for it.** On the call there is a lock and the word `Encrypted`; the digits
 * live in a sheet behind it.
 *
 * ## What the chip's presence means
 *
 * The chip is shown when, and only when, the call's own encryption is armed —
 * that is, when the per-call SFrame key derived from the MLS exporter is in
 * place. It is not decoration and it is not a claim about the transport: an
 * un-armed call shows nothing at all rather than a greyed-out lock, because a
 * lock the user has to interpret is worse than no lock. A call that cannot arm
 * its key does not continue quietly without one — it refuses (security
 * invariant #4), which is a call state and not a chip.
 *
 * The previous pill had a second failure of the same kind: it rendered a yellow
 * lock with an ellipsis while keying was pending or had produced no comparable
 * number, which is a security signal that says nothing and cannot be acted on.
 *
 * Pure and Android-free, so the copy and the grouping are unit-tested on the
 * host JVM. The words are catalog messages (ADR-0061), all `security_critical`:
 * they render in English until a translation is reviewed.
 */

/** The one word beside the lock on the call screen. */
public val CALL_ENCRYPTION_CHIP_LABEL: UiText = UiText("call.encryption.chip")

/** The encryption sheet's title. */
public val CALL_ENCRYPTION_SHEET_TITLE: UiText = UiText("call.encryption.title")

/**
 * Whether the encryption sheet speaks to two people (`docs/ux/calls.md`
 * section 5). A group call or a call from a link never does, whatever its size,
 * and neither does a call with more than one other person in it, whatever it
 * was placed as.
 *
 * @param groupCall `true` for a group call or a call from a link.
 * @param othersInCall the other people in the call now.
 */
public fun isOneToOneEncryptionSheet(groupCall: Boolean, othersInCall: Int): Boolean =
    !groupCall && othersInCall <= 1

/**
 * The line under the digits saying what they are for, without naming a single
 * protocol: two people read the number to each other; in a group or link call
 * everyone sees the same number. Never "phones": people join from computers too.
 */
public fun callEncryptionHint(groupCall: Boolean, othersInCall: Int): UiText =
    if (isOneToOneEncryptionSheet(groupCall, othersInCall)) {
        UiText("call.encryption.compare_hint_one_to_one")
    } else {
        UiText("call.encryption.compare_hint_group")
    }

/**
 * The sheet's plain sentence. Word for word what `encryptionSheetSentence` says
 * on web and `CallPeerName.encryptionSentence` on iOS.
 *
 * A one-to-one call names the other person when this device knows their name.
 * A group or link call never names one person: everyone in it compares the same
 * number, and naming one participant claims a two-person call (the defect of
 * 2026-11-07). Where there is no name the sentence stays true rather than
 * inventing "Someone", which would read as a claim about a specific person.
 */
public fun callEncryptionSentence(otherName: String?, groupCall: Boolean, othersInCall: Int): UiText {
    val name = otherName?.trim()?.takeIf { it.isNotEmpty() }
    return if (name == null || !isOneToOneEncryptionSheet(groupCall, othersInCall)) {
        UiText("call.encryption.sentence_group")
    } else {
        UiText("call.encryption.sentence_named", mapOf("name" to name))
    }
}

/**
 * The safety number split into its groups of four, for reading aloud.
 *
 * The core (`lakko_calls::safety_number`) already renders four space-separated
 * groups of four digits and pins that shape with a golden vector, so this is
 * normally a split. It re-groups anyway rather than trusting the caller,
 * because the display depends on the grouping being right and a value that
 * arrives unspaced would otherwise render as one unreadable run — the exact
 * failure this whole change exists to remove. Returns an empty list for a blank
 * value, so the sheet can show nothing rather than an empty box.
 */
public fun safetyNumberGroups(value: String?): List<String> {
    val digits = value?.filterNot { it.isWhitespace() }.orEmpty()
    if (digits.isEmpty()) return emptyList()
    return digits.chunked(SAFETY_NUMBER_GROUP_SIZE)
}

/** Digits per spoken / displayed group. */
private const val SAFETY_NUMBER_GROUP_SIZE: Int = 4

/**
 * What TalkBack reads for the safety number (section 8: "read in groups of
 * four"). Commas make the reader pause between groups instead of running
 * sixteen digits together.
 */
public fun safetyNumberSpokenLabel(value: String?, strings: Strings = Localization.strings.value): String {
    val groups = safetyNumberGroups(value)
    if (groups.isEmpty()) return strings["call.encryption.number_unavailable"]
    return strings.format("call.encryption.number_a11y", mapOf("groups" to joinList(groups, strings)))
}

/**
 * Whether the call screen shows the lock chip at all.
 *
 * Both halves are required and neither is redundant: [armed] is the security
 * fact, and a [safetyNumber] is what the sheet behind the chip exists to show —
 * a chip that opened an empty sheet would be a dead end. In practice they move
 * together (the number is derived from the same seed in the same step), so this
 * is a guard rather than a third state.
 */
public fun showsCallEncryptionChip(armed: Boolean, safetyNumber: String?): Boolean =
    armed && safetyNumberGroups(safetyNumber).isNotEmpty()
