// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

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
 * host JVM.
 */

/** The one word beside the lock on the call screen. */
public const val CALL_ENCRYPTION_CHIP_LABEL: String = "Encrypted"

/** The encryption sheet's title. */
public const val CALL_ENCRYPTION_SHEET_TITLE: String = "This call is encrypted"

/**
 * The line under the digits saying what they are for — the only place in the
 * call surface that explains the safety number, and it does so without naming a
 * single protocol.
 */
public const val CALL_ENCRYPTION_SHEET_HINT: String =
    "Read these numbers aloud. If they match on both phones, no one is listening."

/**
 * The sheet's plain sentence, naming the other person when this device knows
 * their name.
 *
 * A name is what makes the sentence worth reading, so it is used whenever there
 * is one. Where there is not — a group call, or a contact this device has never
 * been able to name — the sentence stays true rather than inventing "Someone",
 * which would read as a claim about a specific person.
 */
public fun callEncryptionSentence(otherName: String?): String {
    val name = otherName?.trim()?.takeIf { it.isNotEmpty() }
    return if (name == null) {
        "Only the people on this call can hear it."
    } else {
        "Only you and $name can hear this call."
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
public fun safetyNumberSpokenLabel(value: String?): String {
    val groups = safetyNumberGroups(value)
    if (groups.isEmpty()) return "Safety number unavailable"
    return "Safety number: " + groups.joinToString(", ")
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
