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

/**
 * State + dispatcher for the [LoginScreen].
 *
 * Mirror of `apps/web/src/routes/Login.tsx`'s local signals.
 * Simpler than [SignupViewModel] — single-stage form, no
 * recovery-phrase post-step.
 *
 * The [cachedUsername] constructor arg pre-fills the username
 * field from [me.lakko.app.auth.AuthState.Anon.cachedUsername].
 * On a fresh device (no cache) it's `null` and the user types
 * the handle from scratch.
 */
public class LoginViewModel(
    private val repo: AuthRepository,
    cachedUsername: String? = null,
) : ViewModel() {

    private val mutableState: MutableStateFlow<LoginUiState> =
        MutableStateFlow(LoginUiState(username = cachedUsername ?: ""))

    public val state: StateFlow<LoginUiState> = mutableState.asStateFlow()

    public fun onUsernameChange(value: String) {
        mutableState.update { it.copy(username = value) }
    }

    public fun onPasswordChange(value: String) {
        mutableState.update { it.copy(password = value) }
    }

    /**
     * Submit the login form. Drives [AuthRepository.login] under
     * `viewModelScope`, surfaces success as an [AuthState.Session]
     * transition (which the top-level Activity router catches via
     * `collectAsStateWithLifecycle`), and surfaces failures as
     * inline form messages.
     *
     * Wrong-password / unknown-username collapse to the same
     * generic copy — by design (OPAQUE indistinguishable failure
     * + the same posture as the web side).
     */
    public fun onSubmit() {
        val snapshot = mutableState.value
        if (snapshot.busy) return  // ignore double-tap

        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            val passwordBytes = snapshot.password.toByteArray(Charsets.UTF_8)
            try {
                repo.login(snapshot.username, passwordBytes)
                // `repo.login` flipped the AuthState; the screen
                // will be replaced by the home destination on the
                // next recomposition. Clear the password field so
                // the textfield buffer doesn't hold it past the
                // navigation.
                mutableState.update {
                    it.copy(busy = false, error = null, password = "")
                }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Invalid input.")
                }
            } catch (err: AuthHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = userFacingHttpError(err))
                }
            } catch (_: AuthNetworkError) {
                // Same indistinguishable-network-failure copy as
                // signup — the user takes the same action either way
                // (check connection, retry).
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "Network error. Check your connection and try again.",
                    )
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Login failed.")
                }
            } finally {
                passwordBytes.fill(0)
            }
        }
    }

    /** Map server-side HTTP errors to user-facing copy. 401 stays
     *  generic on purpose — wrong password and unknown account
     *  are intentionally indistinguishable (OPAQUE's no-probing
     *  property + the username-resolution layer can't leak
     *  account existence). */
    private fun userFacingHttpError(err: AuthHttpError): String =
        when (err.status) {
            401 -> "Invalid credentials."
            429 -> "Too many attempts. Please wait a moment and retry."
            in 500..599 -> "Server error. Please try again shortly."
            else -> err.message ?: "Login failed (${err.status})."
        }
}

/** Snapshot of every field the [LoginScreen] renders. */
public data class LoginUiState(
    val username: String = "",
    val password: String = "",
    val busy: Boolean = false,
    val error: String? = null,
)
