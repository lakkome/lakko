// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState

/**
 * State + dispatcher for the [PasskeysScreen] (Phase 2a2e).
 *
 * Mirror of:
 *
 *   * `apps/web/src/auth/PasskeysSection.tsx`
 *   * `apps/ios/Lakko/Webauthn/PasskeysViewModel.swift`
 *
 * Drives the three management operations:
 *
 *   * [refresh]            — `GET /webauthn/credentials`
 *   * [addPasskey]         — `POST /register/start` →
 *     [WebauthnRegistration.register] → `POST /register/finish`
 *   * [deleteCredential]   — `DELETE /webauthn/credentials/:id`
 *
 * Sign-in-with-passkey is deferred to the cache-exportKey
 * sub-sprint — same architectural blocker as web 2a2c2 and iOS:
 * the existing [AuthRepository] flows derive Identity, device
 * root key, and MLS bootstrap from the OPAQUE exportKey, and a
 * WebAuthn assertion doesn't produce one.
 */
public class PasskeysViewModel(
    private val repo: AuthRepository,
    private val client: WebauthnClient = WebauthnClient(),
    private val registration: WebauthnRegistration = WebauthnRegistration(),
) : ViewModel() {

    private val mutableState: MutableStateFlow<PasskeysUiState> =
        MutableStateFlow(PasskeysUiState())

    public val state: StateFlow<PasskeysUiState> = mutableState.asStateFlow()

    init {
        rebindSession()
    }

    override fun onCleared() {
        super.onCleared()
        client.close()
    }

    /** Re-bind the bearer token from the current
     *  [AuthRepository.identitySecrets]. Called on init + before
     *  every operation so a token rotation between two calls
     *  doesn't strand the next request. */
    private fun rebindSession() {
        client.setSessionToken(repo.identitySecrets.value?.sessionToken)
    }

    public fun onNicknameChange(value: String) {
        mutableState.update { it.copy(nickname = value, message = null) }
    }

    public fun refresh() {
        rebindSession()
        if (repo.identitySecrets.value == null) {
            mutableState.update { it.copy(credentials = emptyList()) }
            return
        }
        mutableState.update { it.copy(loading = true) }
        viewModelScope.launch {
            try {
                val res = client.listCredentials()
                mutableState.update {
                    it.copy(loading = false, credentials = res.credentials)
                }
            } catch (e: WebauthnHttpError) {
                if (e.status == 401) {
                    // The publisher hasn't fired yet — stay quiet.
                    mutableState.update {
                        it.copy(loading = false, credentials = emptyList())
                    }
                } else {
                    mutableState.update {
                        it.copy(loading = false, message = Message.Error(explain(e)))
                    }
                }
            } catch (e: Throwable) {
                mutableState.update {
                    it.copy(
                        loading = false,
                        message = Message.Error(
                            e.message ?: "Could not load passkeys.",
                        ),
                    )
                }
            }
        }
    }

    public fun addPasskey(context: Context) {
        rebindSession()
        val snapshot = mutableState.value
        if (snapshot.busy) return
        val trimmed = snapshot.nickname.trim()
        if (trimmed.isEmpty()) {
            mutableState.update {
                it.copy(message = Message.Error("Give the passkey a nickname."))
            }
            return
        }
        val session = repo.state.value
        if (session !is AuthState.Session) {
            mutableState.update {
                it.copy(message = Message.Error("Session not ready."))
            }
            return
        }
        val userName = session.account.username
        mutableState.update { it.copy(busy = true, message = null) }
        viewModelScope.launch {
            try {
                val start = client.startRegistration(userName, trimmed)
                val publicKey = start.options.publicKeyField()
                val outcome = registration.register(context, publicKey)
                when (outcome) {
                    is WebauthnRegistration.Outcome.Success -> {
                        client.finishRegistration(
                            stateId = start.stateId,
                            nickname = start.nickname,
                            credential = outcome.value,
                        )
                        mutableState.update {
                            it.copy(
                                busy = false,
                                nickname = "",
                                message = Message.Ok("Passkey \"$trimmed\" added."),
                            )
                        }
                        refresh()
                    }
                    WebauthnRegistration.Outcome.Cancelled -> {
                        mutableState.update {
                            it.copy(
                                busy = false,
                                message = Message.Error(
                                    "Cancelled. No passkey was created.",
                                ),
                            )
                        }
                    }
                    is WebauthnRegistration.Outcome.Failed -> {
                        mutableState.update {
                            it.copy(
                                busy = false,
                                message = Message.Error(
                                    outcome.cause.message
                                        ?: "Could not create a passkey.",
                                ),
                            )
                        }
                    }
                }
            } catch (e: WebauthnHttpError) {
                mutableState.update {
                    it.copy(busy = false, message = Message.Error(explain(e)))
                }
            } catch (e: Throwable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Error(
                            e.message ?: "Could not add the passkey.",
                        ),
                    )
                }
            }
        }
    }

    public fun deleteCredential(credentialIdB64: String, nickname: String) {
        rebindSession()
        if (mutableState.value.busy) return
        mutableState.update { it.copy(busy = true, message = null) }
        viewModelScope.launch {
            try {
                client.deleteCredential(credentialIdB64)
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Ok("Removed \"$nickname\"."),
                    )
                }
                refresh()
            } catch (e: WebauthnHttpError) {
                mutableState.update {
                    it.copy(busy = false, message = Message.Error(explain(e)))
                }
            } catch (e: Throwable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Error(
                            e.message ?: "Could not remove the passkey.",
                        ),
                    )
                }
            }
        }
    }

    private fun explain(e: WebauthnHttpError): String = when (e.status) {
        401 -> "Session expired. Sign in again."
        503 -> "Server hasn't enabled passkey support."
        else -> if (e.body.isEmpty()) "Server error (${e.status})."
            else "Server error (${e.status}): ${e.body}"
    }
}

public data class PasskeysUiState(
    val loading: Boolean = false,
    val busy: Boolean = false,
    val credentials: List<CredentialSummary> = emptyList(),
    val nickname: String = "",
    val message: Message? = null,
)

public sealed interface Message {
    public data class Ok(val text: String) : Message
    public data class Error(val text: String) : Message
}
