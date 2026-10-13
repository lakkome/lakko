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
import me.lakko.app.l10n.UiText

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
    /** ADR-0048 §4 — the core's username rule (`validateUsername`). */
    private val validateUsername: (String) -> String =
        { uniffi.lakko_ffi_uniffi.validateUsername(it) },
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
        // ADR-0048 §4 — a username failing the rule cannot exist, so no
        // OPAQUE round-trip is spent on it. ADR-0054 S5: no lookup either; the
        // login goes by the login handle of the name.
        val username = when (val decision = me.lakko.app.profile.searchDecision(snapshot.username, validateUsername)) {
            is me.lakko.app.profile.SearchDecision.Refuse -> {
                mutableState.update { it.copy(error = decision.text) }
                return
            }
            is me.lakko.app.profile.SearchDecision.Lookup -> decision.normalized
        }

        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            val passwordBytes = snapshot.password.toByteArray(Charsets.UTF_8)
            try {
                repo.login(username, passwordBytes)
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
                    it.copy(busy = false, error = err.text ?: WRONG_CREDENTIALS)
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
                        error = OFFLINE,
                    )
                }
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableState.update { it.copy(busy = false) }
                }
                mutableState.update {
                    it.copy(busy = false, error = WRONG_CREDENTIALS)
                }
            } finally {
                passwordBytes.fill(0)
            }
        }
    }

    /**
     * Phase 2a2c2-android — drive a passkey-only login. Mirror
     * of [onSubmit] minus the password path: tap → Credential
     * Manager sheet → server /webauthn/auth round-trip → state
     * flip. Cache miss / cross-account failures surface as
     * inline form messages.
     */
    public fun onPasskeyLogin() {
        val snapshot = mutableState.value
        if (snapshot.busy) return

        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            try {
                repo.loginWithPasskey()
                mutableState.update { it.copy(busy = false, error = null) }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, error = err.text ?: PASSKEY_GENERIC)
                }
            } catch (err: AuthHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = userFacingHttpError(err))
                }
            } catch (_: AuthNetworkError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = OFFLINE,
                    )
                }
            } catch (_: me.lakko.app.anon.AnonTokenException) {
                // ADR-0054 - a username lookup spends an anonymous token; one
                // that could not be drawn reads as the network failure.
                mutableState.update {
                    it.copy(busy = false, error = OFFLINE)
                }
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableState.update { it.copy(busy = false) }
                }
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = PASSKEY_GENERIC,
                    )
                }
            }
        }
    }

    /** Map server-side HTTP errors to user-facing copy. 401 stays
     *  generic on purpose — wrong password and unknown account
     *  are intentionally indistinguishable (OPAQUE's no-probing
     *  property + the username-resolution layer can't leak
     *  account existence). */
    private fun userFacingHttpError(err: AuthHttpError): UiText =
        me.lakko.app.auth.authLimitText(err) ?: when (err.status) {
            429 -> UiText("common.error.too_many_attempts")
            503 -> UiText("common.error.unavailable")
            // 401 / 404 / 400 and anything else: the one sentence, so a wrong
            // username and a wrong password stay indistinguishable (web's
            // `loginErrorMessage`).
            else -> WRONG_CREDENTIALS
        }

    private companion object {
        val WRONG_CREDENTIALS = UiText("auth.error.wrong_credentials")
        val OFFLINE = UiText("common.error.offline")
        val PASSKEY_GENERIC = UiText("auth.passkey.error.generic")
    }
}

/** Snapshot of every field the [LoginScreen] renders. */
public data class LoginUiState(
    val username: String = "",
    val password: String = "",
    val busy: Boolean = false,
    /** The form's error sentence (ADR-0061: rendered from its key). */
    val error: UiText? = null,
)
