// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.FreshAuthOtherAccount
import me.lakko.app.auth.FreshAuthWrongPassword
import me.lakko.app.auth.PasskeyCancelled
import me.lakko.app.l10n.UiText

/*
 * The proof block of `docs/ux/identity-recovery.md` section 17.1, as pure
 * decisions: what is drawn, whether the surface's main button is enabled,
 * and what a failed proof leaves on screen. The Compose side ([ProofBlock])
 * renders these and decides nothing.
 */

/** The surfaces the block is drawn inside. */
public enum class ProofSurface {
    Reset,
    DeleteAccount,
    AddDeviceRequest,
    AddDeviceScan,
    AddPasskey,
    ChangePassword,

    /** ADR-0066 / UX 18.1 - remove another device (Settings > Devices). */
    RemoveDevice,

    /** ADR-0067 / UX 19.2 - add, replace or remove the recovery email. */
    RecoveryEmail,

    /** ADR-0067 / UX 19.5 - save a recovery phrase (a key rotation). */
    SavePhrase,

    /** ADR-0068 - change the username. */
    ChangeUsername,
}

/** The per-surface lines of the block (UX 17.2-17.4). */
public class ProofCopy(
    /** The helper under the field. */
    public val helper: String,
    /** A wrong password: says what did NOT happen. */
    public val wrongPassword: String,
    /** A cancelled passkey gesture: one line, no error styling. */
    public val cancelled: String,
    /** Any other refusal. */
    public val generic: String,
)

/** The lines of [surface]. Add a passkey and change the password have their
 *  own helper and cancelled line; their wrong-password and generic lines are
 *  the shared ones (the catalog has none per surface). */
public fun proofCopy(surface: ProofSurface): ProofCopy = when (surface) {
    ProofSurface.Reset -> ProofCopy(
        helper = "restore_identity.reset.password_help",
        wrongPassword = "restore_identity.reset.error.wrong_password",
        cancelled = "proof.reset.cancelled",
        generic = "restore_identity.reset.error.generic",
    )
    ProofSurface.DeleteAccount -> ProofCopy(
        helper = "proof.delete.help",
        wrongPassword = "proof.delete.wrong_password",
        cancelled = "settings.account.delete.error.cancelled",
        generic = "settings.account.delete.error.generic",
    )
    ProofSurface.AddDeviceRequest -> ProofCopy(
        helper = "proof.add_device.help",
        wrongPassword = "proof.add_device.request.wrong_password",
        cancelled = "proof.add_device.request.cancelled",
        generic = "device_link.request.error.generic",
    )
    ProofSurface.AddDeviceScan -> ProofCopy(
        helper = "proof.add_device.help",
        wrongPassword = "proof.add_device.scan.wrong_password",
        cancelled = "proof.add_device.scan.cancelled",
        generic = "device_link.request.error.generic",
    )
    ProofSurface.AddPasskey -> ProofCopy(
        helper = "proof.add_passkey.help",
        wrongPassword = "restore_identity.reset.error.wrong_password",
        cancelled = "proof.add_passkey.cancelled",
        generic = "settings.passkeys.error.generic",
    )
    ProofSurface.ChangePassword -> ProofCopy(
        helper = "proof.change_password.help",
        wrongPassword = "restore_identity.reset.error.wrong_password",
        cancelled = "proof.change_password.cancelled",
        generic = "recovery.new_password.error.generic",
    )
    // UX 19.2 / 19.5: the catalog has no cancelled line of their own; the
    // closest existing line is used.
    ProofSurface.RecoveryEmail -> ProofCopy(
        helper = "proof.recovery_email.help",
        wrongPassword = "restore_identity.reset.error.wrong_password",
        cancelled = "chat.record.cancelled",
        generic = "common.error.unavailable",
    )
    ProofSurface.SavePhrase -> ProofCopy(
        helper = "proof.save_phrase.help",
        wrongPassword = "restore_identity.reset.error.wrong_password",
        cancelled = "chat.record.cancelled",
        generic = "protect.phrase.save.failed",
    )
    ProofSurface.ChangeUsername -> ProofCopy(
        helper = "proof.username.help",
        wrongPassword = "restore_identity.reset.error.wrong_password",
        cancelled = "chat.record.cancelled",
        generic = "common.error.unavailable",
    )
    // UX 18.6.
    ProofSurface.RemoveDevice -> ProofCopy(
        helper = "proof.remove_device.help",
        wrongPassword = "proof.remove_device.wrong_password",
        cancelled = "proof.remove_device.cancelled",
        generic = "devices.remove.error.not_revoked",
    )
}

/** Whether [surface] opens on the passkey notice when the account has a
 *  usable passkey (UX 17.4: "No field. You will be asked for your passkey.")
 *  rather than on the password field with `Use a passkey instead`. */
public fun proofOpensOnPasskey(surface: ProofSurface): Boolean =
    surface == ProofSurface.AddDeviceRequest || surface == ProofSurface.AddDeviceScan

/** Which input the block offers. */
public enum class ProofMode {
    /** The password field (and `Use a passkey instead` when usable). */
    Password,

    /** `You will be asked for your passkey.` and `Use your password instead`. */
    PasskeyNotice,
}

/** What the block is doing. */
public enum class ProofPhase { Idle, Checking, PasskeySheet }

/** The one line under the block. */
public sealed interface ProofLine {
    /** In the danger colour, announced as an error. */
    public data class Error(val text: UiText) : ProofLine

    /** Plain: nothing went wrong (a cancelled gesture). */
    public data class Plain(val text: UiText) : ProofLine
}

/** The block's own state. Memory only; the password is never saved. */
public data class ProofBlockState(
    val mode: ProofMode = ProofMode.Password,
    val phase: ProofPhase = ProofPhase.Idle,
    val password: String = "",
    val username: String = "",
    val line: ProofLine? = null,
    /** A server limit: when it ends (unix ms). */
    val waitUntilMs: Long? = null,
) {
    /** Never print the typed password. */
    override fun toString(): String = "ProofBlockState(<redacted>)"
}

/** What the block draws and allows. */
public data class ProofBlockView(
    /** `false`: a proof is live, the surface looks as it did before. */
    val visible: Boolean,
    val showPasswordField: Boolean,
    val showUsernameField: Boolean,
    /** `Use a passkey instead`: absent, never disabled-for-ever. */
    val showPasskeyButton: Boolean,
    val showPasskeyNotice: Boolean,
    val showUsePasswordInstead: Boolean,
    val fieldsEnabled: Boolean,
    val passkeyButtonEnabled: Boolean,
    /** Whether the proof side allows the surface's main button. */
    val mainEnabled: Boolean,
    /** The main button shows its working word. */
    val working: Boolean,
    /** Milliseconds of a server limit left; 0 when none. */
    val waitLeftMs: Long,
)

/**
 * The block for [state] (UX 17.1, the shared states table).
 *
 * @param proofLive a fresh-auth proof is live: no block at all.
 * @param hasUsablePasskey the account has a passkey AND this device can use
 *   passkeys; otherwise the passkey controls are absent.
 * @param usernameKnown this device knows the account's username; when it
 *   does not, the password route needs the typed name too.
 */
public fun proofBlockView(
    proofLive: Boolean,
    hasUsablePasskey: Boolean,
    usernameKnown: Boolean,
    state: ProofBlockState,
    nowMs: Long,
): ProofBlockView {
    if (proofLive) {
        return ProofBlockView(
            visible = false,
            showPasswordField = false,
            showUsernameField = false,
            showPasskeyButton = false,
            showPasskeyNotice = false,
            showUsePasswordInstead = false,
            fieldsEnabled = false,
            passkeyButtonEnabled = false,
            mainEnabled = true,
            working = false,
            waitLeftMs = 0L,
        )
    }
    // A passkey mode without a usable passkey cannot be shown: the field is.
    val mode = if (state.mode == ProofMode.PasskeyNotice && !hasUsablePasskey) ProofMode.Password else state.mode
    val idle = state.phase == ProofPhase.Idle
    val waitLeft = waitLeftMs(state.waitUntilMs, nowMs)
    val limited = waitLeft > 0L
    val passwordMode = mode == ProofMode.Password
    val passwordReady = state.password.isNotEmpty() && (usernameKnown || state.username.isNotBlank())
    return ProofBlockView(
        visible = true,
        showPasswordField = passwordMode,
        showUsernameField = passwordMode && !usernameKnown,
        showPasskeyButton = passwordMode && hasUsablePasskey,
        showPasskeyNotice = !passwordMode,
        showUsePasswordInstead = !passwordMode,
        // A limit is on the password budget: the field waits, the passkey
        // route and every cancel control stay usable.
        fieldsEnabled = idle && !limited,
        passkeyButtonEnabled = idle,
        mainEnabled = idle && if (passwordMode) passwordReady && !limited else true,
        working = !idle,
        waitLeftMs = waitLeft,
    )
}

/** Milliseconds of [waitUntilMs] left at [nowMs]; 0 when none or over. */
public fun waitLeftMs(waitUntilMs: Long?, nowMs: Long): Long =
    if (waitUntilMs == null) 0L else (waitUntilMs - nowMs).coerceAtLeast(0L)

/** What a failed proof means for the block. Nothing was done in any of them. */
public sealed interface ProofFailure {
    public data object WrongPassword : ProofFailure

    /** The person dismissed the passkey sheet. */
    public data object Cancelled : ProofFailure

    /** The passkey belongs to a different account. */
    public data object OtherAccount : ProofFailure

    /** The passkey check failed otherwise. */
    public data object PasskeyFailed : ProofFailure

    public data object Offline : ProofFailure

    /** A server limit, with the stated wait when there is one. */
    public data class RateLimited(val retryAfterSecs: Long?) : ProofFailure

    public data object Generic : ProofFailure
}

/** Classify a failure of the proof ([viaPasskey]: the gesture, else the
 *  password sign-in). */
public fun proofFailureOf(err: Throwable, viaPasskey: Boolean): ProofFailure = when {
    err is FreshAuthWrongPassword -> ProofFailure.WrongPassword
    err is PasskeyCancelled -> ProofFailure.Cancelled
    err is FreshAuthOtherAccount -> ProofFailure.OtherAccount
    err is AuthNetworkError || err is java.io.IOException -> ProofFailure.Offline
    err is AuthHttpError && err.status == HTTP_TOO_MANY_REQUESTS -> ProofFailure.RateLimited(err.retryAfterSecs)
    viaPasskey && err is AuthClientArgError -> ProofFailure.PasskeyFailed
    viaPasskey -> ProofFailure.PasskeyFailed
    else -> ProofFailure.Generic
}

/**
 * The block after a failed proof (UX 17.1): back to idle with one line. The
 * typed password is kept so the person corrects it in place; a cancelled
 * gesture carries no error styling.
 */
public fun afterProofFailure(
    state: ProofBlockState,
    failure: ProofFailure,
    surface: ProofSurface,
    nowMs: Long,
): ProofBlockState {
    val copy = proofCopy(surface)
    val idle = state.copy(phase = ProofPhase.Idle, line = null)
    return when (failure) {
        ProofFailure.WrongPassword -> idle.copy(line = ProofLine.Error(UiText(copy.wrongPassword)))
        ProofFailure.Cancelled -> idle.copy(line = ProofLine.Plain(UiText(copy.cancelled)))
        ProofFailure.OtherAccount -> idle.copy(line = ProofLine.Error(UiText("proof.passkey.other_account")))
        ProofFailure.PasskeyFailed -> idle.copy(line = ProofLine.Error(UiText("proof.passkey.failed")))
        ProofFailure.Offline -> idle.copy(line = ProofLine.Error(UiText("common.error.offline")))
        is ProofFailure.RateLimited -> {
            val secs = failure.retryAfterSecs
            if (secs != null && secs > 0L) {
                // The countdown line is drawn from [ProofBlockView.waitLeftMs].
                idle.copy(waitUntilMs = nowMs + secs * MILLIS_PER_SECOND)
            } else {
                idle.copy(line = ProofLine.Error(UiText("common.error.too_many_attempts")))
            }
        }
        ProofFailure.Generic -> idle.copy(line = ProofLine.Error(UiText(copy.generic)))
    }
}

/**
 * The block when its surface found the proof gone (`403
 * fresh-auth-required`, or the ten minutes ran out while the screen was
 * open): idle, no line, no error styling. Nothing went wrong.
 */
public fun afterProofRequired(state: ProofBlockState): ProofBlockState =
    state.copy(phase = ProofPhase.Idle, line = null, password = "")

// ---------------------------------------------------------------------------
// Per-surface decisions
// ---------------------------------------------------------------------------

/** What the reset warning shows under its bullets (UX 17.2). */
public enum class ResetPanel {
    /** 15.1 as it was: the typed word and the button, no block. */
    NoBlock,

    /** The block, then the word and the button. */
    Block,

    /** The "cannot reset" panel, in place of the block, the word and the
     *  button. */
    Cannot,
}

/**
 * Which panel the reset warning opens on (UX 17.2).
 *
 * @param authenticatorLanding this session was landed with the
 *   authenticator code.
 * @param knowsPassword the person tapped `I know my password`.
 * @param saidNoPassword the person tapped `I do not know my password`.
 */
public fun resetPanel(
    proofLive: Boolean,
    hasUsablePasskey: Boolean,
    authenticatorLanding: Boolean,
    knowsPassword: Boolean,
    saidNoPassword: Boolean,
): ResetPanel = when {
    proofLive -> ResetPanel.NoBlock
    hasUsablePasskey -> ResetPanel.Block
    saidNoPassword -> ResetPanel.Cannot
    authenticatorLanding && !knowsPassword -> ResetPanel.Cannot
    else -> ResetPanel.Block
}

/** Whether the reset panel shows the `I do not know my password` link: only
 *  under a block that has no passkey route. */
public fun resetShowsNoPasswordLink(panel: ResetPanel, hasUsablePasskey: Boolean): Boolean =
    panel == ResetPanel.Block && !hasUsablePasskey

/** What the delete dialog shows between its body and its actions (UX 17.3). */
public enum class DeletePanel {
    /** The passkey notice; `Delete account` opens ONE passkey sheet, the
     *  gesture bound to this deletion (ADR-0014), which is itself the proof:
     *  no passkey sign-in runs first. A live proof does not skip it. */
    PasskeyNotice,

    /** Body and buttons, as before. */
    NoBlock,

    /** The block (no passkey button) and `I do not know my password`. */
    Block,

    /** `proof.delete.cannot` and `I know my password`; no field, no
     *  `Delete account`. */
    Cannot,
}

/** Which panel the delete dialog shows (UX 17.3). */
public fun deletePanel(hasPasskey: Boolean, proofLive: Boolean, saidNoPassword: Boolean): DeletePanel = when {
    hasPasskey -> DeletePanel.PasskeyNotice
    proofLive -> DeletePanel.NoBlock
    saidNoPassword -> DeletePanel.Cannot
    else -> DeletePanel.Block
}

/** Whether `Delete account` is shown and enabled for [panel]. */
public fun deleteButtonEnabled(panel: DeletePanel, view: ProofBlockView, busy: Boolean): Boolean = when (panel) {
    DeletePanel.Cannot -> false
    DeletePanel.PasskeyNotice, DeletePanel.NoBlock -> !busy
    DeletePanel.Block -> !busy && view.mainEnabled
}

/**
 * Whether the deletion must stop and show the proof block before anything is
 * sent (UX 17.3). A passkey account never does: its ADR-0014 gesture is the
 * proof and the server asks for no `x-lakko-fresh-grant` header (it is sent
 * only when a proof happens to be live). An account without a passkey needs
 * a live proof.
 */
public fun deleteNeedsProofFirst(hasPasskey: Boolean, proofLive: Boolean): Boolean = !hasPasskey && !proofLive

/** The label of the dialog's `Delete account` button: its working word while
 *  the account is being deleted, the checking word while the typed password
 *  is being checked. */
public fun deleteButtonLabel(deleting: Boolean, checking: Boolean): String = when {
    deleting -> "settings.account.delete.working"
    checking -> "common.checking"
    else -> "settings.account.delete.confirm"
}

/** What a failed step of a gated action means for its surface. */
public enum class GatedFailure {
    /** `403 fresh-auth-required` (or no live proof): show the block again,
     *  keep everything else, no error. */
    ProofRequired,

    /** Anything else: the surface's own error handling. */
    Other,
}

/** The 403 mapping every gated surface shares. */
public fun gatedFailureOf(err: Throwable): GatedFailure =
    if (me.lakko.app.auth.isFreshAuthRequired(err)) GatedFailure.ProofRequired else GatedFailure.Other

/** What the password-change form shows first (brief item 7). */
public enum class PasswordChangePanel {
    /** The two new-password fields. */
    Form,

    /** The proof block (current password, or a passkey), with the way to the
     *  three-word check ([PASSWORD_CHANGE_USE_WORDS]) when this device holds
     *  the phrase. */
    Proof,

    /** The local three-word check. */
    Words,
}

/** Which panel the password change shows. */
public fun passwordChangePanel(unlocked: Boolean, choseWords: Boolean, holdsPhrase: Boolean): PasswordChangePanel = when {
    unlocked -> PasswordChangePanel.Form
    choseWords && holdsPhrase -> PasswordChangePanel.Words
    else -> PasswordChangePanel.Proof
}

/** The link from the password-change gate to the local three-word check. */
public const val PASSWORD_CHANGE_USE_WORDS: String = "proof.change_password.use_words"

private const val HTTP_TOO_MANY_REQUESTS: Int = 429
private const val MILLIS_PER_SECOND: Long = 1_000L
