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
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableState.update { it.copy(busy = false) }
                }
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
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableState.update { it.copy(busy = false) }
                }
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
            flowWipes = outcome.wipes,
            predecessors = outcome.predecessors,
        )
    }

    /**
     * ADR-0073 "Reset account keys" - the person has NO recovery phrase, NO
     * other device and NO passkey that restores keys, confirmed a second
     * factor and typed the confirmation. [AuthRepository.mintFreshIdentity]
     * runs the reset; the repository flips the auth state and the router
     * shows what follows (15.4). A failure BEFORE anything changed (UX 15.1)
     * leaves the panel open with the sentence [resetFailureText] picks; a
     * failure after the server step is the next screen's, never this
     * panel's.
     *
     * The password and the second factor (UX 15.1b) are confirmed by the
     * panel's factor section BEFORE this is called. When the confirmation
     * ran out in between, or the server refused it (`403
     * reset-factor-required`, `403 fresh-auth-required`), nothing was reset
     * and [onFactorRequired] opens the section again; that is not an error.
     */
    public fun startFresh(onFactorRequired: () -> Unit = {}) {
        val snapshot = mutableState.value
        if (snapshot.busy) return
        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                repo.mintFreshIdentity()
                // The repository moved on - the warning is over.
                mutableState.update { it.copy(busy = false, error = null) }
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableState.update { it.copy(busy = false) }
                }
                val failure = resetFailureOf(err)
                if (failure == ResetFailure.FactorRequired) onFactorRequired()
                mutableState.update { afterResetFailure(it, failure) }
            }
        }
    }

    /** The warning screen closed (back, cancel, or the prompt left the
     *  screen): its error line is not kept. */
    public fun onResetClosed() {
        mutableState.update { it.copy(error = null) }
    }

    /** The repository, for the reset warning's factor section. */
    public val repository: AuthRepository get() = repo

    override fun onCleared() {
        mutableState.update { it.copy(phrase = "") }
        super.onCleared()
    }
}

/** What a failed reset means for the warning screen (UX 15.1 + 15.1b).
 *  Nothing was reset in any of them. */
public sealed interface ResetFailure {
    /** The password and a second factor must be confirmed now (none is
     *  live, or the server refused what was sent): the factor section opens
     *  again, no error. */
    public data object FactorRequired : ResetFailure

    /** Anything else: the form's error line ([resetFailureText]). */
    public class Other(public val text: UiText) : ResetFailure
}

/** Classify a failure of [AuthRepository.mintFreshIdentity]. A raw `403
 *  reset-factor-required` or `403 fresh-auth-required` reads as
 *  [ResetFailure.FactorRequired] too. */
public fun resetFailureOf(err: Throwable): ResetFailure =
    if (me.lakko.app.auth.isResetFactorRequired(err) || me.lakko.app.auth.isFreshAuthRequired(err)) {
        ResetFailure.FactorRequired
    } else {
        ResetFailure.Other(resetFailureText(err))
    }

/** The screen state after a failed reset: idle, with the error line only
 *  when something actually went wrong. */
public fun afterResetFailure(state: RestoreIdentityUiState, failure: ResetFailure): RestoreIdentityUiState =
    when (failure) {
        ResetFailure.FactorRequired -> state.copy(busy = false, error = null)
        is ResetFailure.Other -> state.copy(busy = false, error = failure.text)
    }

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
) {
    /** Never print the typed phrase. */
    override fun toString(): String = "RestoreIdentityUiState(<redacted>)"
}
