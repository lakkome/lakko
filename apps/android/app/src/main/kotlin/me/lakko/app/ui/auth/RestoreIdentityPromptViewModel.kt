// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.IdentityVerificationUnavailable
import me.lakko.app.auth.PhraseMismatch
import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * State + dispatcher for the [RestoreIdentityPrompt]: the person is signed
 * in on a device that does not hold the account's keys
 * ([me.lakko.app.auth.AuthState.NeedsRecoveryPhrase]).
 *
 * Mirror of `apps/web/src/auth/RestoreIdentityPrompt.tsx`. The keys come
 * from another device, from a passkey that carries them, or from the typed
 * 12-word phrase; every route ends in the same fail-closed check against
 * the account's published identity before anything is stored. The last
 * resort creates a new identity. A route that only signs in (three words,
 * an authenticator code) is never offered here: it cannot restore keys.
 *
 * The verification failure modes are surfaced distinctly:
 *   * [PhraseMismatch] — the phrase derives the wrong account key.
 *   * [IdentityVerificationUnavailable] — the directory could not be
 *     reached to verify (retryable).
 *   * a BIP39 checksum failure ([FfiException.Mnemonic]) — bad words.
 */
public class RestoreIdentityPromptViewModel(
    private val repo: AuthRepository,
) : ViewModel() {

    private val mutableState: MutableStateFlow<RestoreIdentityUiState> =
        MutableStateFlow(RestoreIdentityUiState())

    public val state: StateFlow<RestoreIdentityUiState> = mutableState.asStateFlow()

    init {
        // How many of the account's passkeys carry keys, asked with the
        // parked session. Unknown (`null`) keeps the passkey row.
        val token = repo.pendingSessionToken()
        if (token != null) {
            viewModelScope.launch {
                val withKeys = repo.passkeysWithKeys(token)
                mutableState.update { it.copy(passkeysWithKeys = withKeys?.size) }
            }
        }
    }

    /**
     * The passkey row: run the passkey prompt for the signed-in account and
     * ask the passkey for the keys. When it restores them the repository
     * lands the session; otherwise the prompt stays, [onNotRestored] returns
     * to the list, and the first line there says why.
     */
    public fun restoreWithPasskey(activityContext: android.content.Context, onNotRestored: () -> Unit) {
        if (mutableState.value.busy) return
        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                val outcome = repo.restoreKeysWithPasskey(activityContext)
                mutableState.update { it.copy(busy = false) }
                if (outcome != me.lakko.app.webauthn.PasskeyKeysOutcome.Restored) onNotRestored()
            } catch (_: me.lakko.app.auth.PasskeyCancelled) {
                mutableState.update { it.copy(busy = false) }
            } catch (err: IdentityVerificationUnavailable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = UiText(
                            if (err.missing) "restore_identity.error.not_published" else "restore_identity.error.unreachable",
                        ),
                    )
                }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, error = err.text ?: UiText("auth.passkey.error.generic"))
                }
            } catch (_: me.lakko.app.auth.AuthNetworkError) {
                mutableState.update { it.copy(busy = false, error = UiText("common.error.offline")) }
            } catch (err: Throwable) {
                if (err is kotlinx.coroutines.CancellationException) throw err
                mutableState.update { it.copy(busy = false, error = UiText("auth.passkey.error.generic")) }
            }
        }
    }

    public fun onPhraseChange(value: String) {
        mutableState.update { it.copy(phrase = value) }
    }

    /**
     * Submit the typed recovery phrase. Drives
     * [AuthRepository.provideRecoveryPhrase] in `viewModelScope`. On
     * success the repository flips the auth state to `Session` and the
     * top-level router replaces this screen. On any failure-to-verify
     * the seed is zeroized inside the repository and we surface an
     * inline error (retryable).
     */
    public fun onSubmit() {
        val snapshot = mutableState.value
        if (snapshot.busy) return
        if (snapshot.phrase.isBlank()) {
            mutableState.update {
                it.copy(error = UiText("recovery_phrase.error.incomplete"))
            }
            return
        }
        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                repo.provideRecoveryPhrase(snapshot.phrase)
                // Repository already flipped to Session — wipe the
                // textfield buffer so the phrase doesn't linger.
                mutableState.update {
                    it.copy(busy = false, error = null, phrase = "")
                }
            } catch (_: PhraseMismatch) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = UiText("restore_identity.error.phrase_mismatch"),
                    )
                }
            } catch (err: IdentityVerificationUnavailable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = UiText(
                            if (err.missing) {
                                "restore_identity.error.not_published"
                            } else {
                                "restore_identity.error.unreachable"
                            },
                        ),
                    )
                }
            } catch (_: FfiException.Mnemonic) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = UiText("restore_identity.error.invalid_phrase"),
                    )
                }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, error = err.text ?: UiText("restore_identity.error.generic"))
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = UiText("restore_identity.error.generic"),
                    )
                }
            }
        }
    }

    /**
     * Another device: complete the pending restore with the keys
     * received from ANOTHER DEVICE (the entropy of the recovery phrase)
     * instead of a typed phrase. Suspend passthrough to
     * [AuthRepository.completeFromPairing]; verification failures
     * (`PhraseMismatch` / `IdentityVerificationUnavailable`)
     * propagate to the calling panel, which surfaces them inline.
     * On success the repository flips the auth state to `Session`.
     */
    public suspend fun completeFromPairing(
        outcome: me.lakko.app.devices.PairingOrchestrator.JoinRecoveryOutcome,
    ) {
        repo.completeFromPairing(
            phraseEntropy = outcome.phraseEntropy,
            session = outcome.session,
        )
    }

    /**
     * ADR-0065 "Reset account keys" — the person has NO recovery phrase, NO
     * other device and NO passkey that restores keys, and typed the
     * confirmation. [AuthRepository.mintFreshIdentity] runs the reset; the
     * repository flips the auth state to `SaveFreshPhrase` and the router
     * shows the new phrase (or its "could not finish publishing" state). A
     * failure BEFORE anything changed (UX 15.1) leaves the panel open with
     * the sentence [resetFailureText] picks; a failure after the server step
     * is the phrase screen's, never this panel's.
     */
    public fun startFresh() {
        val snapshot = mutableState.value
        if (snapshot.busy) return
        val passwordBytes = if (snapshot.resetNeedsPassword) snapshot.resetPassword.toByteArray(Charsets.UTF_8) else null
        // The typed name goes along only with the password; the repository
        // uses it only when the device does not know the account's username.
        val typedUsername = snapshot.resetUsername.takeIf { snapshot.resetNeedsPassword && it.isNotBlank() }
        mutableState.update { it.copy(busy = true, error = null, resetPasswordError = null) }
        viewModelScope.launch {
            try {
                repo.mintFreshIdentity(passwordBytes, typedUsername)
                // Repository flipped to SaveFreshPhrase — clear local busy
                // and what was typed; the warning is over.
                resetOpen = false
                mutableState.update { it.copy(busy = false, error = null, resetPassword = "", resetUsername = "") }
            } catch (err: Throwable) {
                if (err is kotlinx.coroutines.CancellationException) throw err
                mutableState.update { afterResetFailure(it, resetFailureOf(err), repo.resetNeedsPassword()) }
            } finally {
                passwordBytes?.fill(0)
            }
        }
    }

    /** Whether the warning is open in THIS view model. The view model
     *  outlives a configuration change, so a rotation finds it `true` and
     *  [onResetOpened] keeps what was typed. */
    private var resetOpen: Boolean = false

    /** The warning screen opened: decide whether it asks for the password
     *  (UX 15.1a). The fields are never pre-filled. Called again when the
     *  screen is recreated by a configuration change (rotation): the warning
     *  is already open then, and the typed password and username stay. They
     *  live here, in memory, never in saved instance state. */
    public fun onResetOpened() {
        val needsPassword = repo.resetNeedsPassword()
        val reopened = resetOpen
        resetOpen = true
        mutableState.update { afterResetOpened(it, needsPassword, alreadyOpen = reopened) }
    }

    /** The warning screen closed (back, cancel, or the prompt left the
     *  screen): the typed password and username are not kept. */
    public fun onResetClosed() {
        resetOpen = false
        mutableState.update {
            it.copy(resetPassword = "", resetUsername = "", resetPasswordError = null, error = null)
        }
    }

    public fun onResetPasswordChange(value: String) {
        mutableState.update { it.copy(resetPassword = value, resetPasswordError = null) }
    }

    public fun onResetUsernameChange(value: String) {
        mutableState.update { it.copy(resetUsername = value, resetPasswordError = null) }
    }

    override fun onCleared() {
        resetOpen = false
        mutableState.update { it.copy(phrase = "", resetPassword = "", resetUsername = "") }
        super.onCleared()
    }
}

/**
 * The screen state when the warning opens. A first opening starts with
 * empty fields; a re-opening by a configuration change ([alreadyOpen]) keeps
 * what was typed and the sentence under the field, and keeps the password
 * field once it was shown (a field must not vanish under the person's hands).
 */
public fun afterResetOpened(
    state: RestoreIdentityUiState,
    needsPassword: Boolean,
    alreadyOpen: Boolean,
): RestoreIdentityUiState = if (alreadyOpen) {
    state.copy(resetNeedsPassword = state.resetNeedsPassword || needsPassword)
} else {
    state.copy(
        resetNeedsPassword = needsPassword,
        resetPassword = "",
        resetUsername = "",
        resetPasswordError = null,
        error = null,
    )
}

/** What a failed reset means for the warning screen (UX 15.1 + 15.1a).
 *  Nothing was reset in any of them. */
public sealed interface ResetFailure {
    /** The password must be proven now (no live grant, or `403
     *  password-required`): reveal the field with its helper, no error. */
    public data object PasswordRequired : ResetFailure

    /** The typed password is wrong: the sentence goes under the field. */
    public data object WrongPassword : ResetFailure

    /** Anything else: the form's error line ([resetFailureText]). */
    public class Other(public val text: UiText) : ResetFailure
}

/** Classify a failure of [AuthRepository.mintFreshIdentity]. A raw `403
 *  password-required` reads as [ResetFailure.PasswordRequired] too. */
public fun resetFailureOf(err: Throwable): ResetFailure = when {
    err is me.lakko.app.auth.ResetPasswordRequired || me.lakko.app.auth.isResetPasswordRequired(err) ->
        ResetFailure.PasswordRequired
    err is me.lakko.app.auth.ResetWrongPassword -> ResetFailure.WrongPassword
    else -> ResetFailure.Other(resetFailureText(err))
}

/**
 * The screen state after a failed reset. [needsPasswordNow] is whether a
 * live grant is still missing (a password that was accepted leaves a grant
 * behind, so a later failure does not ask for it again). The typed password
 * is kept only where the person corrects or re-sends it.
 */
public fun afterResetFailure(
    state: RestoreIdentityUiState,
    failure: ResetFailure,
    needsPasswordNow: Boolean,
): RestoreIdentityUiState = when (failure) {
    ResetFailure.PasswordRequired -> state.copy(
        busy = false,
        error = null,
        resetNeedsPassword = true,
        resetPassword = "",
        resetPasswordError = null,
    )
    // The typed username stays in every case: it is not a secret, and the
    // person corrects it in place.
    ResetFailure.WrongPassword -> state.copy(
        busy = false,
        error = null,
        resetNeedsPassword = true,
        resetPasswordError = UiText("restore_identity.reset.error.wrong_password"),
    )
    is ResetFailure.Other -> state.copy(
        busy = false,
        error = failure.text,
        resetNeedsPassword = needsPasswordNow,
        resetPassword = if (needsPasswordNow) state.resetPassword else "",
        resetPasswordError = null,
    )
}

/** Whether the warning shows the username field above the password (UX
 *  15.1a): only together with the password field, and only when this device
 *  does not know the account's username (a passkey sign-in that named no
 *  account), since the password sign-in needs a name to derive its login
 *  handle from. */
public fun resetShowsUsername(needsPassword: Boolean, usernameKnown: Boolean): Boolean =
    needsPassword && !usernameKnown

/** Whether the reset button is enabled (UX 15.1a): the confirmation word,
 *  the password too when the screen asks for it, and the username too when
 *  the screen shows that field ([resetShowsUsername]). */
public fun resetButtonEnabled(
    typedWord: String,
    needsPassword: Boolean,
    password: String,
    usernameKnown: Boolean = true,
    username: String = "",
): Boolean =
    resetConfirmationMatches(typedWord) &&
        (!needsPassword || password.isNotEmpty()) &&
        (!resetShowsUsername(needsPassword, usernameKnown) || username.isNotBlank())

/**
 * The sentence for a reset that failed before anything changed (UX 15.1):
 * the shared `429` line with its countdown, "check your connection" when
 * offline, "try again later" for any other refusal.
 */
public fun resetFailureText(err: Throwable): UiText = when (err) {
    is me.lakko.app.auth.AuthNetworkError -> UiText("restore_identity.reset.error.offline")
    is me.lakko.app.auth.AuthHttpError ->
        me.lakko.app.auth.authLimitText(err) ?: UiText("restore_identity.reset.error.generic")
    is AuthClientArgError -> err.text ?: UiText("restore_identity.reset.error.generic")
    else -> UiText("restore_identity.reset.error.generic")
}

/** Snapshot of the [RestoreIdentityPrompt] field state. */
public data class RestoreIdentityUiState(
    val phrase: String = "",
    val busy: Boolean = false,
    /** The form's error sentence (ADR-0061: rendered from its key). */
    val error: UiText? = null,
    /** How many of the account's passkeys carry keys; `null` while it is
     *  not known. Zero hides the passkey row. */
    val passkeysWithKeys: Int? = null,
    /** Whether the reset warning shows the `Password` field (UX 15.1a): no
     *  live reset grant is held, or the server asked for the password. */
    val resetNeedsPassword: Boolean = false,
    /** The password typed on the reset warning; never pre-filled, cleared
     *  when the screen closes. Memory only (never saved instance state). */
    val resetPassword: String = "",
    /** The username typed on the reset warning, when the device does not
     *  know the account's. Same lifetime as [resetPassword]. */
    val resetUsername: String = "",
    /** The sentence under the password field (a wrong password). */
    val resetPasswordError: UiText? = null,
) {
    /** Never print the typed phrase or password. */
    override fun toString(): String = "RestoreIdentityUiState(<redacted>)"
}
