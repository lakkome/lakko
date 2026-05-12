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
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.AuthRepository
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * State + dispatcher for the [RecoverScreen].
 *
 * Mirror of `apps/web/src/routes/Recover.tsx`'s local signals.
 * Single-stage form (mnemonic + new password + confirm). On
 * submit drives [AuthRepository.recover], which rotates the
 * password envelope on the server, re-derives the device root
 * key from the new OPAQUE export key, and flips the auth state.
 *
 * The [cachedUsername] constructor arg drives the "Account on
 * this device: @alice" line in the UI. When it's `null` the
 * screen renders a "no cached account" stub instead (the
 * recovery flow can't run without an `accountId` on disk; that's
 * a 1.15e/discovery sprint problem).
 */
public class RecoverViewModel(
    private val repo: AuthRepository,
    public val cachedUsername: String?,
) : ViewModel() {

    private val mutableState: MutableStateFlow<RecoverUiState> =
        MutableStateFlow(RecoverUiState())

    public val state: StateFlow<RecoverUiState> = mutableState.asStateFlow()

    public fun onPhraseChange(value: String) {
        mutableState.update { it.copy(phrase = value) }
    }

    public fun onPasswordChange(value: String) {
        mutableState.update { it.copy(password = value) }
    }

    public fun onConfirmChange(value: String) {
        mutableState.update { it.copy(confirm = value) }
    }

    /**
     * Submit the recover form.
     *
     * Validates locally (new password ≥ 8, confirm matches),
     * drives [AuthRepository.recover] in `viewModelScope`,
     * surfaces success as an [AuthState.Session] transition (the
     * top-level Activity router catches it via
     * `collectAsStateWithLifecycle`), surfaces failures as inline
     * form messages.
     *
     * The mnemonic-parse failure (bad checksum / unknown word)
     * collapses to one user-facing message — same posture as the
     * web side, preserves BIP39's no-probing property.
     */
    public fun onSubmit() {
        val snapshot = mutableState.value
        if (snapshot.busy) return  // ignore double-tap

        if (snapshot.password != snapshot.confirm) {
            mutableState.update {
                it.copy(error = "Passwords do not match.")
            }
            return
        }
        if (snapshot.password.length < MIN_PASSWORD_LEN) {
            mutableState.update {
                it.copy(
                    error = "Password must be at least $MIN_PASSWORD_LEN characters.",
                )
            }
            return
        }
        if (snapshot.phrase.isBlank()) {
            mutableState.update {
                it.copy(error = "Recovery phrase must be non-empty.")
            }
            return
        }

        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            val passwordBytes = snapshot.password.toByteArray(Charsets.UTF_8)
            try {
                repo.recover(snapshot.phrase, passwordBytes)
                // `repo.recover` already flipped AuthState to
                // Session. The screen will be replaced by the
                // home destination on the next recomposition;
                // wipe the textfield buffers so they don't hold
                // the phrase / password past the navigation.
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = null,
                        phrase = "",
                        password = "",
                        confirm = "",
                    )
                }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Invalid input.")
                }
            } catch (_: FfiException.Mnemonic) {
                // Unknown word + bad checksum both flow here via
                // `RecoveryMnemonic.fromPhrase`. Generic copy is
                // intentional — the BIP39 spec keeps these
                // indistinguishable.
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "Invalid recovery phrase. Check the spelling and order of all 24 words.",
                    )
                }
            } catch (err: AuthHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = userFacingHttpError(err))
                }
            } catch (_: AuthNetworkError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "Network error. Check your connection and try again.",
                    )
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Recovery failed.")
                }
            } finally {
                passwordBytes.fill(0)
            }
        }
    }

    private fun userFacingHttpError(err: AuthHttpError): String =
        when (err.status) {
            // The server's `/password/reset/{start,finish}` pair
            // returns 401 on a wrong mnemonic (the OPAQUE recovery
            // login fails at AKE finalisation). Surface generic
            // copy — wrong-mnemonic and wrong-account collapse
            // into the same response by design.
            401 -> "Invalid recovery phrase."
            429 -> "Too many attempts. Please wait a moment and retry."
            in 500..599 -> "Server error. Please try again shortly."
            else -> err.message ?: "Recovery failed (${err.status})."
        }

    public companion object {
        /** Mirror of [SignupViewModel.MIN_PASSWORD_LEN]. The
         *  recovery flow sets a **new** password so the same
         *  floor applies. */
        public const val MIN_PASSWORD_LEN: Int = 8
    }
}

/** Snapshot of every field the [RecoverScreen] renders. */
public data class RecoverUiState(
    val phrase: String = "",
    val password: String = "",
    val confirm: String = "",
    val busy: Boolean = false,
    val error: String? = null,
)
