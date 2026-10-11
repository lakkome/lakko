// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.l10n.UiText

/**
 * The catalog keys of the remove-device sheet and the removed-device screen
 * (`docs/ux/identity-recovery.md` section 18.1, 18.2, 18.6). Nothing else
 * reads one of these keys directly.
 */
internal object RemovalCopy {
    /** Takes `{device}`. */
    const val SHEET_TITLE: String = "settings.devices.remove.title"
    const val SHEET_BODY: String = "settings.devices.remove.body"
    const val CONFIRM: String = "settings.devices.remove.confirm"
    const val WORKING: String = "settings.devices.remove.working"

    /** The row of a device signed out at auth whose removal is finishing. */
    const val ROW_PENDING: String = "devices.remove.pending"

    /** The row's button when the owed step waits on the person's passkey. */
    const val ROW_FINISH: String = "devices.remove.finish"

    /** Under the list once every step of a removal answered. */
    const val DONE: String = "devices.remove.done"
    const val ERROR_NOT_REVOKED: String = "devices.remove.error.not_revoked"
    const val ERROR_TRY_LATER: String = "devices.remove.error.try_later"

    /** UX 15.4: a removal is blocked while a reset's publish is owed. */
    const val BLOCKED_PUBLISH_OWED: String = "recovery_phrase.fresh.publish_owed"
    const val RATE_LIMITED: String = "common.error.too_many_attempts"

    /** The removed screen (18.2). The two bodies take `{name}`. */
    const val REMOVED_TITLE: String = "devices.removed.title"
    const val REMOVED_BODY_REMOVING: String = "devices.removed.body_removing"
    const val REMOVED_BODY_DONE: String = "devices.removed.body_done"

    /** The key-reset screen's, reused as 18.6 says. */
    const val REMOVED_PROGRESS: String = "devices.signed_out.removing"
    const val REMOVED_SIGN_IN: String = "devices.signed_out.sign_in"
}

/** What the sheet does after one run of the removal. */
internal sealed interface RemovalSheetOutcome {
    /** Close the sheet; the row is gone or shows `Removing...`. */
    data object Close : RemovalSheetOutcome

    /** The proof ran out: the block appears again, no error. */
    data object ShowProofBlock : RemovalSheetOutcome

    /** Stay open with this line; nothing was removed. */
    data class Line(val text: UiText) : RemovalSheetOutcome
}

/** The pure mapping of a removal's result onto the sheet (UX 18.1's table). */
internal fun removalSheetOutcome(result: RemovalResult): RemovalSheetOutcome = when (result) {
    RemovalResult.Done, is RemovalResult.Pending -> RemovalSheetOutcome.Close
    is RemovalResult.NotRevoked -> when (result.failure) {
        RevokeFailure.ProofRequired -> RemovalSheetOutcome.ShowProofBlock
        RevokeFailure.AccountKeyRequired -> RemovalSheetOutcome.Line(UiText(RemovalCopy.ERROR_TRY_LATER))
        is RevokeFailure.RateLimited -> RemovalSheetOutcome.Line(UiText(RemovalCopy.RATE_LIMITED))
        is RevokeFailure.Unconfirmed, is RevokeFailure.Refused ->
            RemovalSheetOutcome.Line(UiText(RemovalCopy.ERROR_NOT_REVOKED))
    }
}

/** The line a blocked removal shows instead of running; `null` when it may run. */
internal fun removalGateLine(gate: RemovalGate): UiText? = when (gate) {
    RemovalGate.Allowed -> null
    RemovalGate.KeyResetPublishOwed -> UiText(RemovalCopy.BLOCKED_PUBLISH_OWED)
    // A session on Android always holds the account's keys (a device
    // without them is parked on "Restore your keys" and never reaches
    // Settings), so this line is not expected to be drawn.
    RemovalGate.NoAccountKeys -> UiText(RemovalCopy.ERROR_TRY_LATER)
}

/** The catalog keys of "It was lost or stolen" (UX 18.1, 18.3, 18.6). */
internal object RotationCopy {
    const val REASON_UNUSED: String = "settings.devices.remove.reason_unused"
    const val REASON_STOLEN: String = "settings.devices.remove.reason_stolen"
    const val TITLE: String = "devices.rotate.title"
    const val LEDE: String = "devices.rotate.lede"
    val EFFECTS: List<String> = listOf(
        "devices.rotate.effect_devices",
        "devices.rotate.effect_phrase",
        "devices.rotate.effect_passkeys",
        "devices.rotate.effect_contacts",
        // ADR-0066 section 3.5a rule 5: what the old key granted is spent.
        "devices.rotate.effect_links",
    )
    const val CONFIRM: String = "devices.rotate.confirm"
    const val STEP_REMOVE: String = "devices.rotate.step_remove"
    const val STEP_KEYS: String = "devices.rotate.step_keys"
    const val STEP_DEVICES: String = "devices.rotate.step_devices"
    const val DONE: String = "devices.rotate.done"
    const val NOT_STARTED: String = "devices.rotate.error.not_started"
    const val RETRY: String = "devices.rotate.retry"
    const val NOT_NOW: String = "devices.rotate.not_now"

    /** The kept device (18.3). */
    const val ROTATED_NOTICE: String = "devices.rotated.notice"
    const val ROTATED_SHOW_PHRASE: String = "devices.rotated.show_phrase"
    const val ROTATED_UPDATING: String = "devices.rotated.updating"

    /** The new-phrase screen after a rotation (section 9.1's screen). */
    const val PHRASE_TITLE: String = "recovery_phrase.fresh.title"

    /** 15.4's copy while the publish is owed. */
    const val PUBLISH_OWED: String = "recovery_phrase.fresh.publish_owed"
}

/** Why the person is removing the device (the sheet's radio pair). */
internal enum class RemovalReason { Unused, Stolen }

/** The step list's three lines, in order. */
internal val ROTATION_STEP_LINES: List<Pair<RotationUiStep, String>> = listOf(
    RotationUiStep.RemovingDevice to RotationCopy.STEP_REMOVE,
    RotationUiStep.ChangingKeys to RotationCopy.STEP_KEYS,
    RotationUiStep.UpdatingDevices to RotationCopy.STEP_DEVICES,
)

/** What the sheet does after one run of "remove and change keys". */
internal sealed interface RotationSheetOutcome {
    /** The keys were changed: close the sheet and show the new phrase;
     *  [withheld] while the publish is still owed (15.4's state). */
    data class ShowPhrase(val withheld: Boolean) : RotationSheetOutcome

    /** The proof ran out: the block appears again, no error. */
    data object ShowProofBlock : RotationSheetOutcome

    /** Nothing was removed and nothing changed: stay open with this line. */
    data class NothingRemoved(val text: UiText) : RotationSheetOutcome

    /** The device WAS removed; the keys were not changed. `Try again` and
     *  `Not now`, with an extra line when there is a wait. */
    data class NotStarted(val extra: UiText?) : RotationSheetOutcome
}

/** The pure mapping of a rotation's result onto the sheet (UX 18.1's table).
 *  [phraseWithheld] is [rotationWithholdsPhrase] of the owed record. */
internal fun rotationSheetOutcome(result: RotationResult, phraseWithheld: Boolean): RotationSheetOutcome =
    when (result) {
        RotationResult.Done -> RotationSheetOutcome.ShowPhrase(withheld = false)
        is RotationResult.Pending -> RotationSheetOutcome.ShowPhrase(withheld = phraseWithheld)
        is RotationResult.NotRemoved -> when (val mapped = removalSheetOutcome(RemovalResult.NotRevoked(result.failure))) {
            RemovalSheetOutcome.ShowProofBlock -> RotationSheetOutcome.ShowProofBlock
            is RemovalSheetOutcome.Line -> RotationSheetOutcome.NothingRemoved(mapped.text)
            RemovalSheetOutcome.Close -> RotationSheetOutcome.NothingRemoved(UiText(RemovalCopy.ERROR_NOT_REVOKED))
        }
        is RotationResult.NotStarted -> when (val reason = result.reason) {
            NotStartedReason.ProofRequired -> RotationSheetOutcome.ShowProofBlock
            is NotStartedReason.RateLimited -> RotationSheetOutcome.NotStarted(UiText(RemovalCopy.RATE_LIMITED))
            NotStartedReason.AccountKeyRequired -> RotationSheetOutcome.NotStarted(UiText(RemovalCopy.ERROR_TRY_LATER))
            NotStartedReason.RemovalUnfinished, is NotStartedReason.Failed -> {
                @Suppress("UNUSED_EXPRESSION")
                reason
                RotationSheetOutcome.NotStarted(null)
            }
        }
    }
