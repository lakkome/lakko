// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.SIGNUP_RATE_LIMITED_COPY
import me.lakko.app.auth.SignupOutcome
import kotlin.coroutines.cancellation.CancellationException
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
 * collection. The form is the only stage here: on success the
 * recovery phrase goes to [parkRecoveryPhrase] and the guided
 * phrase screen ([RecoveryPhraseOnboarding]) shows it exactly once
 * before the session is usable.
 */
public class SignupViewModel internal constructor(
    /** The signup itself; [AuthRepository.signup] in production, a fake in
     *  a host test (the proof-of-work solve needs the native library). */
    private val runSignup: suspend (username: String, password: ByteArray, onProofSolved: () -> Unit) -> SignupOutcome,
    /** The created account's id once [runSignup] returned. */
    private val createdAccountId: () -> ByteArray?,
    /** ADR-0048 §4 — the core's username rule (`validateUsername`). */
    private val validateUsername: (String) -> String,
    /** ADR-0048 §1 — the core's profile-name rule (`normalizeDisplayName`). */
    private val normalizeDisplayName: (String) -> String?,
    /** Persist this account's own profile once signup has created it. */
    private val saveOwnProfile: (accountId: ByteArray, profile: OwnProfile) -> Unit,
    /** Hand the just-minted recovery phrase to the guided phrase step. Sign-up
     *  lands the session inside the repository, which replaces this screen, so
     *  the phrase is parked for the app root to show (see [SignupPhraseGate]). */
    private val parkRecoveryPhrase: (String) -> Unit,
) : ViewModel() {

    public constructor(
        repo: AuthRepository,
        /** ADR-0048 §4 — the core's username rule (`validateUsername`). */
        validateUsername: (String) -> String = { uniffi.lakko_ffi_uniffi.validateUsername(it) },
        /** ADR-0048 §1 — the core's profile-name rule (`normalizeDisplayName`). */
        normalizeDisplayName: (String) -> String? = { uniffi.lakko_ffi_uniffi.normalizeDisplayName(it) },
        /** No-op by default (tests); `MainActivity` writes the [OwnProfileStore]. */
        saveOwnProfile: (accountId: ByteArray, profile: OwnProfile) -> Unit = { _, _ -> },
        /** No-op by default (tests). */
        parkRecoveryPhrase: (String) -> Unit = {},
    ) : this(
        runSignup = { username, password, onProofSolved -> repo.signup(username, password, onProofSolved) },
        createdAccountId = { repo.identitySecrets.value?.accountId },
        validateUsername = validateUsername,
        normalizeDisplayName = normalizeDisplayName,
        saveOwnProfile = saveOwnProfile,
        parkRecoveryPhrase = parkRecoveryPhrase,
    )

    /** The running signup, so [onCancel] can stop it. */
    private var signupJob: Job? = null

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

    /**
     * Submit the signup form.
     *
     * Validates locally (password length, confirm match), drives
     * [AuthRepository.signup], parks the recovery phrase on success. Errors surface in the UI as
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
                it.copy(error = "The passwords do not match. Type the same password twice.")
            }
            return
        }
        if (pwd.length < MIN_PASSWORD_LEN) {
            mutableState.update {
                it.copy(
                    error = "Use at least $MIN_PASSWORD_LEN characters for the password.",
                )
            }
            return
        }

        // ADR-0054 S4 - the working state (UX section 1): the one heavy piece
        // of work runs off the main thread; until its proof is ready nothing
        // has been sent, so Cancel returns to the filled-in form.
        mutableState.update { it.copy(busy = true, solving = true, error = null, rateLimited = false) }
        signupJob = viewModelScope.launch {
            val passwordBytes = pwd.toByteArray(Charsets.UTF_8)
            try {
                val outcome: SignupOutcome = runSignup(normalizedUsername, passwordBytes) {
                    mutableState.update { it.copy(solving = false) }
                }
                // ADR-0048 §4 — the account now exists; record its own
                // profile (an empty name means others see `@username`).
                // Best-effort: a failed write only means no name until the
                // user sets one in Settings -> Profile.
                createdAccountId()?.let { accountId ->
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
                // The guided phrase step renders above the session from here.
                parkRecoveryPhrase(outcome.recoveryPhrase)
                mutableState.update {
                    it.copy(
                        busy = false,
                        solving = false,
                        error = null,
                        // Wipe the password fields from UI state.
                        // The repository has already consumed +
                        // zeroized the bytes; this just cleans up
                        // the textfield buffers.
                        password = "",
                        confirm = "",
                    )
                }
            } catch (err: AuthClientArgError) {
                mutableState.update {
                    it.copy(busy = false, solving = false, error = err.message ?: "Invalid input.")
                }
            } catch (err: CancellationException) {
                throw err
            } catch (err: AuthHttpError) {
                val copy = userFacingHttpError(err)
                mutableState.update {
                    it.copy(
                        busy = false,
                        solving = false,
                        error = copy,
                        rateLimited = copy == SIGNUP_RATE_LIMITED_COPY,
                    )
                }
            } catch (_: me.lakko.app.anon.AnonTokenException) {
                // The username claim's OPRF token could not be drawn: the
                // generic network copy (ADR-0054 UX section 3).
                mutableState.update {
                    it.copy(
                        busy = false,
                        solving = false,
                        error = "No connection. Check your network and try again.",
                    )
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
                        solving = false,
                        error = "No connection. Check your network and try again.",
                    )
                }
            } catch (_: DiscoveryNetworkError) {
                // Signup now claims the username in discovery; a transport
                // failure there reads the same as one against auth.
                mutableState.update {
                    it.copy(
                        busy = false,
                        solving = false,
                        error = "No connection. Check your network and try again.",
                    )
                }
            } catch (err: DiscoveryHttpError) {
                mutableState.update {
                    it.copy(busy = false, solving = false, error = userFacingDiscoveryError(err))
                }
            } catch (err: Throwable) {
                mutableState.update {
                    it.copy(busy = false, solving = false, error = err.message ?: "Could not create the account. Try again.")
                }
            } finally {
                // Wipe the local bytes copy. The repo also wiped
                // its own copy — both layers are best-effort.
                passwordBytes.fill(0)
            }
        }
    }

    /**
     * `Cancel` in the working row (UX section 1): stop the proof-of-work solve
     * and return to the filled-in form. Only while the solve runs - once its
     * proof is ready the account is being created and the row offers no
     * Cancel. Leaving the screen cancels too (the ViewModel scope ends).
     */
    public fun onCancel() {
        if (!mutableState.value.solving) return
        signupJob?.cancel()
        signupJob = null
        mutableState.update { it.copy(busy = false, solving = false, error = null) }
    }


    /** Map a server-side HTTP error to a user-facing message.
     *  Today's auth service distinguishes a couple of cases by
     *  status code; future cases get added here as the service
     *  surface evolves. Indistinguishable-failure cases (login
     *  401) stay generic on purpose. */
    private fun userFacingHttpError(err: AuthHttpError): String =
        me.lakko.app.auth.authLimitCopy(err) ?: when (err.status) {
            409 -> "That username is taken. Choose another one."
            429 -> "Too many attempts. Wait a moment, then try again."
            in 500..599 -> "Something went wrong on our side. Try again in a moment."
            else -> "Could not create the account. Try again."
        }

    /** Discovery failures other than a taken name (which the repository
     *  already turns into form copy): the username claim or the identity
     *  publish at signup. */
    private fun userFacingDiscoveryError(err: DiscoveryHttpError): String =
        when (err.status) {
            429 -> "Too many attempts. Wait a moment, then try again."
            in 500..599 -> "Something went wrong on our side. Try again in a moment."
            else -> "Could not create the account. Try again."
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
    /** The proof-of-work solve is running: the working row offers `Cancel`. */
    val solving: Boolean = false,
    val error: String? = null,
    /** The last attempt met `signup-rate-limited`: the button reads
     *  `Try again`. The form keeps its contents. */
    val rateLimited: Boolean = false,
)
