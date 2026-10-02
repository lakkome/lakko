// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import android.content.Context
import android.util.Log
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import me.lakko.app.auth.AuthRepository
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.base64Encode
import me.lakko.app.devices.DeviceRemoval
import me.lakko.app.l10n.UiText

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
    /** Run once the server-side erasure succeeded: the app's Forget device
     *  orchestration (MainActivity), so Delete account leaves exactly what
     *  Forget device leaves: nothing of the account on this device. */
    private val onAccountDeleted: () -> Unit,
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
                        message = Message.Error(failure(e, PASSKEYS_GENERIC)),
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
                it.copy(message = Message.Error(UiText("settings.passkeys.error.name_required")))
            }
            return
        }
        val session = repo.state.value
        if (session !is AuthState.Session) {
            mutableState.update {
                it.copy(message = Message.Error(SESSION_NOT_READY))
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
                        // G-13 (ADR-0015): enable attested removals now that the
                        // account has a passkey, so device removals require a
                        // peer-verified gesture. Non-fatal.
                        repo.identitySecrets.value?.let { secrets ->
                            try {
                                DeviceRemoval.enableAttestedRemovalsIfNeeded(
                                    context = context,
                                    accountIdBytes = secrets.accountId,
                                    accountIdB64 = base64Encode(secrets.accountId),
                                )
                            } catch (cause: Throwable) {
                                Log.w(
                                    "PasskeysViewModel",
                                    "failed to enable attested removals",
                                    cause,
                                )
                            }
                        }
                        mutableState.update {
                            it.copy(
                                busy = false,
                                nickname = "",
                                message = Message.Ok(
                                    UiText("settings.passkeys.added", texts = mapOf("name" to trimmed)),
                                ),
                            )
                        }
                        refresh()
                    }
                    WebauthnRegistration.Outcome.Cancelled -> {
                        mutableState.update {
                            it.copy(
                                busy = false,
                                message = Message.Error(UiText("settings.passkeys.error.cancelled")),
                            )
                        }
                    }
                    is WebauthnRegistration.Outcome.Failed -> {
                        mutableState.update {
                            it.copy(
                                busy = false,
                                message = Message.Error(failure(outcome.cause, PASSKEYS_GENERIC)),
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
                        message = Message.Error(failure(e, PASSKEYS_GENERIC)),
                    )
                }
            }
        }
    }

    public fun deleteCredential(
        context: Context,
        credentialIdB64: String,
        nickname: String,
    ) {
        rebindSession()
        if (mutableState.value.busy) return
        mutableState.update { it.copy(busy = true, message = null) }
        viewModelScope.launch {
            try {
                // G-13 (ADR-0014): require a fresh passkey gesture
                // cryptographically bound to THIS credential's removal
                // before deleting it. A throw (incl. the user dismissing
                // the sheet) aborts the delete — fail-closed.
                val actionHashB64 = actionHashForDeleteCredential(credentialIdB64)
                client.assertAction(
                    context = context,
                    actionHashB64 = actionHashB64,
                    registration = registration,
                )
                client.deleteCredential(credentialIdB64)
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Ok(
                            UiText("settings.passkeys.removed", texts = mapOf("name" to nickname)),
                        ),
                    )
                }
                refresh()
            } catch (e: WebauthnUserCancelledError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Error(UiText("settings.passkeys.remove.error.cancelled")),
                    )
                }
            } catch (e: WebauthnHttpError) {
                mutableState.update {
                    it.copy(busy = false, message = Message.Error(explain(e)))
                }
            } catch (e: Throwable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Error(failure(e, PASSKEYS_GENERIC)),
                    )
                }
            }
        }
    }

    /**
     * A3 (GDPR erasure) — delete the whole account across services + wipe this
     * device. Auth `DELETE /account` (on a 403, the account has a passkey, so
     * run the action ceremony bound to this account + retry once, fail-closed)
     * -> best-effort discovery purge -> [onAccountDeleted], the same local
     * wipe + sign-out Forget device runs.
     */
    public fun deleteAccount(context: Context) {
        rebindSession()
        val secrets = repo.identitySecrets.value ?: run {
            mutableState.update { it.copy(message = Message.Error(SESSION_NOT_READY)) }
            return
        }
        if (mutableState.value.busy) return
        mutableState.update { it.copy(busy = true, message = null) }
        viewModelScope.launch {
            try {
                try {
                    client.deleteAccount()
                } catch (e: WebauthnHttpError) {
                    if (e.status == 403) {
                        val actionHashB64 = actionHashForDeleteAccount(secrets.accountId)
                        client.assertAction(
                            context = context,
                            actionHashB64 = actionHashB64,
                            registration = registration,
                        )
                        client.deleteAccount()
                    } else {
                        throw e
                    }
                }
                // Best-effort discovery purge — the auth tier is the critical
                // erasure; the session token is about to be invalidated anyway.
                try {
                    val discovery = DiscoveryClient()
                    discovery.setSessionToken(secrets.sessionToken)
                    discovery.deleteAccount()
                } catch (_: Throwable) {
                    // best-effort
                }
                // Local wipe + full sign-out on this device: the same path
                // Forget device takes (every per-account store, push first).
                onAccountDeleted()
            } catch (e: WebauthnUserCancelledError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Error(UiText("settings.account.delete.error.cancelled")),
                    )
                }
            } catch (e: WebauthnHttpError) {
                mutableState.update {
                    it.copy(busy = false, message = Message.Error(explain(e)))
                }
            } catch (e: Throwable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Error(failure(e, UiText("settings.account.delete.error.generic"))),
                    )
                }
            }
        }
    }

    /**
     * A3 (GDPR portability) — fetch what the server holds about the account,
     * wrap it in a portable JSON document, and hand it to [save], which asks
     * the person where to put the file (the system's save picker; never a
     * share sheet). The E2EE content lives on-device and is exported via the
     * encrypted Backup, not here.
     */
    public fun exportData(save: (name: String, mime: String, bytes: ByteArray) -> Unit) {
        rebindSession()
        if (mutableState.value.busy) return
        mutableState.update { it.copy(busy = true, message = null) }
        viewModelScope.launch {
            try {
                val server = client.exportAccount()
                val doc = ExportDoc(
                    schema = "lakko/account-export/v1",
                    exportedAt = java.time.Instant.now().toString(),
                    server = server,
                    note = UiText("settings.data.export_note").render(),
                )
                val jsonText = exportJson.encodeToString(doc)
                save("lakko-account-export.json", "application/json", jsonText.toByteArray(Charsets.UTF_8))
                // The save picker is the confirmation; no status line.
                mutableState.update { it.copy(busy = false) }
            } catch (e: WebauthnHttpError) {
                mutableState.update {
                    it.copy(busy = false, message = Message.Error(explain(e)))
                }
            } catch (e: Throwable) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Error(failure(e, UiText("settings.data.error.generic"))),
                    )
                }
            }
        }
    }

    private val exportJson = Json { prettyPrint = true }

    /** An HTTP refusal as a catalog message (ADR-0061: the server's body goes
     *  to the log, never to the screen). */
    private fun explain(e: WebauthnHttpError): UiText = when (e.status) {
        401 -> UiText("common.error.session_expired")
        503 -> UiText("auth.passkey.error.server_disabled")
        else -> {
            Log.w("PasskeysViewModel", "WARN server error ${e.status} on ${e.endpoint}")
            UiText("settings.passkeys.error.server_status", texts = mapOf("status" to e.status.toString()))
        }
    }

    /** Any other failure: a network failure says so, the rest is [generic]. */
    private fun failure(e: Throwable, generic: UiText): UiText {
        Log.w("PasskeysViewModel", "WARN failed: ${e.javaClass.simpleName}")
        return if (e is java.io.IOException) UiText("settings.passkeys.error.network") else generic
    }

    private companion object {
        val PASSKEYS_GENERIC = UiText("settings.passkeys.error.generic")
        val SESSION_NOT_READY = UiText("common.error.session_not_ready")
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
    public data class Ok(val text: UiText) : Message
    public data class Error(val text: UiText) : Message
}

/** A3 — the portable JSON document the person saves as a file. Wraps the
 *  server-held [AccountExport] with provenance + an E2EE-content note. */
@Serializable
private data class ExportDoc(
    val schema: String,
    @SerialName("exported_at") val exportedAt: String,
    val server: AccountExport,
    val note: String,
)
