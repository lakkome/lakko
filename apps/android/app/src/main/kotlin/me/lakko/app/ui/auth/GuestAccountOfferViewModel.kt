// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.GuestPhraseUnavailable
import me.lakko.app.auth.authLimitText
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError
import me.lakko.app.l10n.UiText
import me.lakko.app.profile.UsernameCheck
import me.lakko.app.profile.checkUsername
import me.lakko.app.profile.usernameCanonicalHintText
import me.lakko.app.profile.usernameFieldErrorText

/** The two steps of the offer. */
public enum class GuestOfferStep { ASK, FORM }

/** What the offer is doing. */
public enum class GuestOfferBusy { NONE, CREATING, DELETING }

/** Snapshot of everything the guest offer draws. */
public data class GuestOfferUiState(
    val step: GuestOfferStep = GuestOfferStep.ASK,
    val busy: GuestOfferBusy = GuestOfferBusy.NONE,
    /** The form's (or, after [cannotKeep], the question's) error sentence. */
    val error: UiText? = null,
    /** The delete failed; the account is still there and still a guest. */
    val deleteFailed: Boolean = false,
    /** This device cannot show the account's phrase, so it cannot be kept:
     *  only `No, thanks` is offered. */
    val cannotKeep: Boolean = false,
    val username: String = "",
    /** The reason the typed username fails the rule, or `null`. */
    val usernameError: UiText? = null,
    /** `Shown as @alice` when typed non-canonically. */
    val usernameHint: UiText? = null,
    val password: String = "",
    val confirm: String = "",
)

/**
 * State + dispatcher of the one question a guest is asked outside a call
 * (`docs/ux/call-links.md` section 9), Android twin of the web
 * `GuestAccountOffer`:
 *
 *   - `Create account`: a username and a password, then the recovery phrase
 *     (the app root shows it, as after any signup);
 *   - `No, thanks`: the account is deleted at once, on the servers and here.
 *     No confirmation: the line under the button says what it does.
 *
 * The same instance serves the call-link landing and the whole-screen form a
 * guest gets in place of the app, so a step taken on one is not lost on the
 * other.
 */
public class GuestAccountOfferViewModel internal constructor(
    /** [me.lakko.app.auth.AuthRepository.claimGuestAccount]. */
    private val claim: suspend (username: String, password: ByteArray) -> Unit,
    /** [me.lakko.app.auth.AuthRepository.discardGuestAccount], through the
     *  Forget-device orchestration. */
    private val discard: suspend () -> Unit,
    /** The username an earlier attempt already took: shown, never asked
     *  again. Read when it is needed, so a retry sees what the failed attempt
     *  recorded. */
    private val claimedUsername: () -> String?,
    /** ADR-0048 section 4 — the core's username rule (`validateUsername`). */
    private val validateUsername: (String) -> String = { uniffi.lakko_ffi_uniffi.validateUsername(it) },
    private val log: (String, Throwable) -> Unit = { message, cause ->
        android.util.Log.w("lakko-guest", "$message: ${cause.javaClass.simpleName}")
    },
) : ViewModel() {

    private val mutableState = MutableStateFlow(GuestOfferUiState())

    public val state: StateFlow<GuestOfferUiState> = mutableState.asStateFlow()

    /** `Create account` on the question: open the form. */
    public fun onChooseCreate() {
        if (mutableState.value.busy != GuestOfferBusy.NONE || mutableState.value.cannotKeep) return
        mutableState.update { it.copy(step = GuestOfferStep.FORM, error = null, deleteFailed = false) }
    }

    /** `Back` on the form: the question again. */
    public fun onBackToAsk() {
        if (mutableState.value.busy != GuestOfferBusy.NONE) return
        mutableState.update { it.copy(step = GuestOfferStep.ASK, error = null) }
    }

    /** Validated as the person types; the reason shows at once. */
    public fun onUsernameChange(value: String) {
        val check = checkUsername(value, validateUsername)
        mutableState.update {
            it.copy(
                username = value,
                usernameError = usernameFieldErrorText(check, value),
                usernameHint = usernameCanonicalHintText(check, value),
            )
        }
    }

    public fun onPasswordChange(value: String) {
        mutableState.update { it.copy(password = value) }
    }

    public fun onConfirmChange(value: String) {
        mutableState.update { it.copy(confirm = value) }
    }

    /** Submit the form: keep the account. */
    public fun onCreate() {
        val snapshot = mutableState.value
        if (snapshot.busy != GuestOfferBusy.NONE) return
        val fixed = claimedUsername()
        val name: String = if (fixed != null) {
            fixed
        } else {
            // A username failing the rule never reaches the network.
            when (val check = checkUsername(snapshot.username, validateUsername)) {
                is UsernameCheck.Valid -> check.normalized
                is UsernameCheck.Invalid -> {
                    mutableState.update {
                        it.copy(error = check.problem.text, usernameError = check.problem.text)
                    }
                    return
                }
            }
        }
        val problem = newPasswordProblem(snapshot.password, snapshot.confirm)
        if (problem != null) {
            mutableState.update { it.copy(error = problem) }
            return
        }
        mutableState.update { it.copy(busy = GuestOfferBusy.CREATING, error = null) }
        viewModelScope.launch {
            val passwordBytes = snapshot.password.toByteArray(Charsets.UTF_8)
            try {
                // Resolving leaves the recovery phrase parked; the app root
                // switches to the phrase screen on that.
                claim(name, passwordBytes)
                mutableState.update {
                    it.copy(busy = GuestOfferBusy.NONE, error = null, password = "", confirm = "")
                }
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (err: Throwable) {
                log("keeping the guest account failed", err)
                mutableState.update {
                    if (err is GuestPhraseUnavailable) {
                        it.copy(
                            busy = GuestOfferBusy.NONE,
                            error = guestClaimErrorText(err),
                            cannotKeep = true,
                            step = GuestOfferStep.ASK,
                        )
                    } else {
                        it.copy(busy = GuestOfferBusy.NONE, error = guestClaimErrorText(err))
                    }
                }
            } finally {
                passwordBytes.fill(0)
            }
        }
    }

    /** `No, thanks`: delete the account at once. */
    public fun onDecline() {
        if (mutableState.value.busy != GuestOfferBusy.NONE) return
        mutableState.update { it.copy(busy = GuestOfferBusy.DELETING, error = null, deleteFailed = false) }
        viewModelScope.launch {
            try {
                discard()
                // The session is gone; whoever drew the offer has left it.
                mutableState.update { it.copy(busy = GuestOfferBusy.NONE) }
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (err: Throwable) {
                log("deleting the guest account failed", err)
                mutableState.update { it.copy(busy = GuestOfferBusy.NONE, deleteFailed = true) }
            }
        }
    }
}

/** What is wrong with a new password and its confirmation, or `null`: the
 *  sign-up form's two rules, in the sign-up form's order. */
internal fun newPasswordProblem(password: String, confirm: String): UiText? = when {
    password != confirm -> UiText("auth.password.error.mismatch")
    password.length < SignupViewModel.MIN_PASSWORD_LEN -> UiText(
        "auth.password.error.too_short",
        counts = mapOf("count" to SignupViewModel.MIN_PASSWORD_LEN.toULong()),
    )
    else -> null
}

/**
 * The sentence the form shows when keeping a guest account failed (the web
 * `guestClaimErrorMessage`): the missing phrase, a taken or refused username
 * (the claim's own sentence), offline, a limit, an unavailable service, else
 * the generic line.
 */
internal fun guestClaimErrorText(err: Throwable): UiText = when (err) {
    is GuestPhraseUnavailable -> UiText("guest.claim.error.phrase_missing")
    is AuthClientArgError -> err.text ?: GUEST_CLAIM_GENERIC
    is AuthNetworkError, is DiscoveryNetworkError, is me.lakko.app.anon.AnonTokenException -> GUEST_CLAIM_OFFLINE
    is AuthHttpError -> authLimitText(err) ?: statusText(err.status)
    is DiscoveryHttpError -> statusText(err.status)
    else -> GUEST_CLAIM_GENERIC
}

private fun statusText(status: Int): UiText = when (status) {
    409 -> UiText("auth.signup.error.username_taken")
    429 -> UiText("common.error.too_many_attempts")
    503 -> UiText("common.error.unavailable")
    else -> GUEST_CLAIM_GENERIC
}

private val GUEST_CLAIM_GENERIC = UiText("guest.claim.error.generic")
private val GUEST_CLAIM_OFFLINE = UiText("common.error.offline")
