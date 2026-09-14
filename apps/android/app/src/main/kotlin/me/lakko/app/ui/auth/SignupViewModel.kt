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
import me.lakko.app.auth.SignupOutcome
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError
import me.lakko.app.profile.DisplayNameCheck
import me.lakko.app.profile.OwnProfile
import me.lakko.app.profile.UsernameCheck
import me.lakko.app.profile.checkDisplayName
import me.lakko.app.profile.checkUsername
import me.lakko.app.profile.usernameCanonicalHint
import me.lakko.app.profile.usernameFieldError

/**
 * State + dispatcher for the [SignupScreen].
 *
 * Mirror of `apps/web/src/routes/Signup.tsx`'s `createSignal`
 * collection — same fields, same transitions, same two-stage
 * shape (`Form` → `Phrase`).
 *
 * The two-stage local state machine is intentional. Splitting
 * signup from "save your recovery phrase" guarantees the user
 * sees the 24 words exactly once and explicitly acknowledges
 * having written them down before navigating into the
 * authenticated app. Re-display is impossible by construction —
 * the [Stage.Phrase] state holds the only in-memory reference
 * and is dropped on acknowledgement.
 */
public class SignupViewModel(
    private val repo: AuthRepository,
    /** ADR-0048 §4 — the core's username rule (`validateUsername`). */
    private val validateUsername: (String) -> String =
        { uniffi.lakko_ffi_uniffi.validateUsername(it) },
    /** ADR-0048 §1 — the core's profile-name rule (`normalizeDisplayName`). */
    private val normalizeDisplayName: (String) -> String? =
        { uniffi.lakko_ffi_uniffi.normalizeDisplayName(it) },
    /** Persist this account's own profile once signup has created it. No-op
     *  by default (tests); `MainActivity` writes the [OwnProfileStore]. */
    private val saveOwnProfile: (accountId: ByteArray, profile: OwnProfile) -> Unit = { _, _ -> },
) : ViewModel() {

    private val mutableState: MutableStateFlow<SignupUiState> =
        MutableStateFlow(SignupUiState())

    public val state: StateFlow<SignupUiState> = mutableState.asStateFlow()

    /** ADR-0048 §4 — validated as the user types; the reason shows at once. */
    public fun onUsernameChange(value: String) {
        val check = checkUsername(value, validateUsername)
        mutableState.update {
            it.copy(
                username = value,
                usernameError = usernameFieldError(check, value),
                usernameHint = usernameCanonicalHint(check, value),
            )
        }
    }

    /** ADR-0048 §4 — the optional "Your name" field. */
    public fun onDisplayNameChange(value: String) {
        val check = checkDisplayName(value, normalizeDisplayName)
        mutableState.update {
            it.copy(
                displayName = value,
                displayNameError = (check as? DisplayNameCheck.Invalid)?.problem?.copy,
            )
        }
    }

    public fun onPasswordChange(value: String) {
        mutableState.update { it.copy(password = value) }
    }

    public fun onConfirmChange(value: String) {
        mutableState.update { it.copy(confirm = value) }
    }

    public fun onAcknowledgeChange(checked: Boolean) {
        mutableState.update { it.copy(acknowledged = checked) }
    }

    /**
     * Submit the signup form.
     *
     * Validates locally (password length, confirm match), drives
     * [AuthRepository.signup], transitions the stage to
     * [Stage.Phrase] on success. Errors surface in the UI as
     * inline form messages. The password fields are cleared on
     * success so the textfields don't keep the bytes alive longer
     * than necessary — same posture as the web side.
     */
    public fun onSubmit() {
        val snapshot = mutableState.value
        if (snapshot.busy) return  // ignore double-tap

        // ADR-0048 §4 — a username failing the rule never reaches the network.
        val usernameCheck = checkUsername(snapshot.username, validateUsername)
        if (usernameCheck is UsernameCheck.Invalid) {
            mutableState.update {
                it.copy(error = usernameCheck.problem.copy, usernameError = usernameCheck.problem.copy)
            }
            return
        }
        val normalizedUsername = (usernameCheck as UsernameCheck.Valid).normalized
        val nameCheck = checkDisplayName(snapshot.displayName, normalizeDisplayName)
        if (nameCheck is DisplayNameCheck.Invalid) {
            mutableState.update { it.copy(error = nameCheck.problem.copy) }
            return
        }
        val chosenName = (nameCheck as DisplayNameCheck.Valid).normalized

        val pwd = snapshot.password
        val confirm = snapshot.confirm
        if (pwd != confirm) {
            mutableState.update {
                it.copy(error = "Passwords do not match.")
            }
            return
        }
        if (pwd.length < MIN_PASSWORD_LEN) {
            mutableState.update {
                it.copy(
                    error = "Password must be at least $MIN_PASSWORD_LEN characters.",
                )
            }
            return
        }

        mutableState.update { it.copy(busy = true, error = null) }
        viewModelScope.launch {
            val passwordBytes = pwd.toByteArray(Charsets.UTF_8)
            try {
                val outcome: SignupOutcome = repo.signup(normalizedUsername, passwordBytes)
                // ADR-0048 §4 — the account now exists; record its own
                // profile (an empty name means others see `@username`).
                // Best-effort: a failed write only means no name until the
                // user sets one in Settings -> Profile.
                repo.identitySecrets.value?.accountId?.let { accountId ->
                    try {
                        saveOwnProfile(
                            accountId,
                            OwnProfile(
                                displayName = chosenName,
                                username = normalizedUsername,
                                updatedAtMs = System.currentTimeMillis(),
                            ),
                        )
                    } catch (_: Throwable) {
                        // best-effort
                    }
                }
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = null,
                        // Wipe the password fields from UI state.
                        // The repository has already consumed +
                        // zeroized the bytes; this just cleans up
                        // the textfield buffers.
                        password = "",
                        confirm = "",
                        stage = Stage.Phrase(outcome.recoveryPhrase),
                    )
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
                // The cause is dropped here on purpose — every
                // transport-level failure surfaces with the same
                // user-facing copy. Once a logging sink lands
                // (1.15d7 or later) we wire the cause to it; the
                // current scaffold has no log destination so
                // attaching it would just produce a dead reference.
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "Network error. Check your connection and try again.",
                    )
                }
            } catch (_: DiscoveryNetworkError) {
                // Signup now claims the username in discovery; a transport
                // failure there reads the same as one against auth.
                mutableState.update {
                    it.copy(
                        busy = false,
                        error = "Network error. Check your connection and try again.",
                    )
                }
            } catch (err: DiscoveryHttpError) {
                mutableState.update {
                    it.copy(busy = false, error = userFacingDiscoveryError(err))
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, error = err.message ?: "Signup failed.")
                }
            } finally {
                // Wipe the local bytes copy. The repo also wiped
                // its own copy — both layers are best-effort.
                passwordBytes.fill(0)
            }
        }
    }

    /**
     * Called when the user has confirmed they wrote the recovery
     * phrase down. Drops the in-memory phrase reference + signals
     * the screen to navigate forward. The auth state was already
     * flipped to [me.lakko.app.auth.AuthState.Session] inside
     * [AuthRepository.signup]; the Activity-level router will
     * snap to the home screen automatically once we clear the
     * local stage.
     */
    public fun onAcknowledge() {
        mutableState.update {
            it.copy(stage = Stage.Form, acknowledged = false)
        }
    }

    /** Map a server-side HTTP error to a user-facing message.
     *  Today's auth service distinguishes a couple of cases by
     *  status code; future cases get added here as the service
     *  surface evolves. Indistinguishable-failure cases (login
     *  401) stay generic on purpose. */
    private fun userFacingHttpError(err: AuthHttpError): String =
        when (err.status) {
            409 -> "An account with that username already exists."
            429 -> "Too many requests. Please wait a moment and retry."
            in 500..599 -> "Server error. Please try again shortly."
            else -> err.message ?: "Signup failed (${err.status})."
        }

    /** Discovery failures other than a taken name (which the repository
     *  already turns into form copy): the username claim or the identity
     *  publish at signup. */
    private fun userFacingDiscoveryError(err: DiscoveryHttpError): String =
        when (err.status) {
            429 -> "Too many requests. Please wait a moment and retry."
            in 500..599 -> "Server error. Please try again shortly."
            else -> "Signup failed (${err.status})."
        }

    public companion object {
        /** Minimum password length enforced client-side. Mirrors
         *  the floor in `apps/web` — a real strength heuristic
         *  + Argon2id KSF cost feedback lands in a later sprint. */
        public const val MIN_PASSWORD_LEN: Int = 8
    }
}

// =============================================================================
// UI state shape
// =============================================================================

/** Snapshot of every field the signup screen renders. */
public data class SignupUiState(
    val username: String = "",
    /** ADR-0048 §4 — the reason the typed username fails, or `null`. */
    val usernameError: String? = null,
    /** ADR-0048 §4 — `Shown as @alice` when typed non-canonically. */
    val usernameHint: String? = null,
    /** ADR-0048 §4 — optional "Your name". */
    val displayName: String = "",
    /** The reason the typed name fails, or `null`. */
    val displayNameError: String? = null,
    val password: String = "",
    val confirm: String = "",
    val busy: Boolean = false,
    val error: String? = null,
    /** Two-stage local FSM — see [Stage]. */
    val stage: Stage = Stage.Form,
    /** "I have written the words down" checkbox state.
     *  Re-armed on every transition into the phrase stage. */
    val acknowledged: Boolean = false,
)

/** Two-stage signup state machine. [Form] collects the username
 *  + password; [Phrase] displays the BIP39 mnemonic post-signup. */
public sealed interface Stage {
    public data object Form : Stage
    public data class Phrase(val phrase: String) : Stage
}
