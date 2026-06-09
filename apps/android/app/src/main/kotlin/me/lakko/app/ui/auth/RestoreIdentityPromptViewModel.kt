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
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * State + dispatcher for the [RestoreIdentityPrompt] (ADR-0017
 * fresh-device fail-safe completion).
 *
 * Mirror of `apps/web/src/auth/RestoreIdentityPrompt.tsx`. The user's
 * auth factor (password / passkey) already succeeded, but this device
 * has no cached AccountIdentity (phrase) seed, so the auth state is
 * [me.lakko.app.auth.AuthState.NeedsRecoveryPhrase]. The user types
 * their recovery phrase; on submit we drive
 * [AuthRepository.provideRecoveryPhrase], which HARD-BLOCK verifies the
 * phrase against the published identity bundle before caching it and
 * flipping to a full session.
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
                it.copy(error = "Recovery phrase must be non-empty.")
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
                        error = "That recovery phrase doesn't match this " +
                            "account's identity. Check the phrase, or pair " +
                            "an existing device instead.",
                    )
                }
            } catch (err: IdentityVerificationUnavailable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = err.message
                            ?: "Could not verify your recovery phrase. Try again.",
                    )
                }
            } catch (_: FfiException.Mnemonic) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "Invalid recovery phrase. Check the spelling " +
                            "and order of all 24 words.",
                    )
                }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Invalid input.")
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = err.message ?: "Could not restore your identity.",
                    )
                }
            }
        }
    }
}

/** Snapshot of the [RestoreIdentityPrompt] field state. */
public data class RestoreIdentityUiState(
    val phrase: String = "",
    val busy: Boolean = false,
    val error: String? = null,
)
