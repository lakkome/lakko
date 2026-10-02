// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import android.content.Context
import android.content.Intent
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
import me.lakko.app.l10n.UiText

/**
 * State + dispatcher for the [PasskeysScreen] (Phase 2a2e).
 *
 * Mirror of:
 *
 *   * `apps/web/src/auth/PasskeysSection.tsx`
 *   * `apps/ios/Lakko/Webauthn/PasskeysViewModel.swift`
 *
 * Drives the management operations:
 *
 *   * [refresh]            — `GET /webauthn/credentials` and, for the
 *     per-passkey label, `GET /vault/passkeys`
 *   * [addPasskey]         — create a passkey; nothing is stored for it
 *     beyond the credential. When the provider can protect keys with it the
 *     opt-in choice opens ([PasskeysUiState.choice]); otherwise the result
 *     line says it signs in only.
 *   * [openChoice] / [onChoiceDone] — "Let this passkey restore your keys?"
 *   * [stopRestoring]      — `DELETE /vault/passkey`
 *   * [deleteCredential]   — `DELETE /webauthn/credentials/:id`
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

    /** The repository, for the keys choice the screen hosts. */
    public val repository: AuthRepository get() = repo

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
                // The credential ids that have keys stored for them; `null`
                // when the server cannot say, and every passkey then reads
                // "Signs in only", which is what it can do.
                val withKeys = repo.identitySecrets.value?.let { repo.passkeysWithKeys(it.sessionToken) }
                mutableState.update {
                    it.copy(loading = false, credentials = res.credentials, withKeys = withKeys)
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
        if (repo.state.value !is AuthState.Session || repo.identitySecrets.value == null) {
            mutableState.update {
                it.copy(message = Message.Error(SESSION_NOT_READY))
            }
            return
        }
        mutableState.update { it.copy(busy = true, message = null) }
        viewModelScope.launch {
            try {
                val added = addPasskeyForSession(
                    context = context,
                    repo = repo,
                    nickname = trimmed,
                    client = client,
                    registration = registration,
                )
                // Nothing is stored for the new passkey here. When the
                // provider can protect keys with it, the person is asked;
                // otherwise the line says it signs in only.
                val offer = passkeyAddOutcome(
                    prfOutput = added.prfOutput != null,
                    prfEnabled = added.prfEnabled,
                ) == PasskeyAddOutcome.OfferKeysChoice
                mutableState.update {
                    it.copy(
                        busy = false,
                        nickname = "",
                        message = Message.Ok(
                            if (offer) {
                                UiText("settings.passkeys.added", texts = mapOf("name" to trimmed))
                            } else {
                                UiText("settings.passkeys.added_no_keys")
                            },
                        ),
                        choice = if (offer) {
                            PasskeyChoice(added.credentialIdB64, trimmed, added.prfOutput)
                        } else {
                            null
                        },
                    )
                }
                refresh()
            } catch (_: WebauthnUserCancelledError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Error(UiText("settings.passkeys.error.cancelled")),
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

    /** Open "Let this passkey restore your keys?" for an existing passkey
     *  (its row's "Turn on restoring keys"). The passkey's secret then comes
     *  from one more prompt limited to that passkey. */
    public fun openChoice(credential: CredentialSummary) {
        if (mutableState.value.busy) return
        mutableState.update {
            it.copy(
                message = null,
                choice = PasskeyChoice(credential.credentialIdB64, credential.nickname, prfOutput = null),
            )
        }
    }

    /** The choice ended: say what happened and refresh the labels. */
    public fun onChoiceDone(end: KeysChoiceEnd) {
        mutableState.value.choice?.prfOutput?.fill(0)
        val text = keysChoiceEndText(end)
        mutableState.update {
            it.copy(
                choice = null,
                message = when {
                    text == null -> it.message
                    end == KeysChoiceEnd.Stored -> Message.Ok(text)
                    else -> Message.Error(text)
                },
            )
        }
        refresh()
    }

    /** "Stop restoring keys" for one passkey: it then only signs in. Keys
     *  already restored on other devices stay there. */
    public fun stopRestoring(credentialIdB64: String) {
        if (mutableState.value.busy) return
        mutableState.update { it.copy(busy = true, message = null) }
        viewModelScope.launch {
            try {
                repo.stopPasskeyKeys(credentialIdB64)
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Ok(UiText("settings.passkeys.stop_restoring.done")),
                    )
                }
                refresh()
            } catch (e: Throwable) {
                if (e is kotlinx.coroutines.CancellationException) throw e
                Log.w("PasskeysViewModel", "WARN stop restoring keys failed: ${e.javaClass.simpleName}")
                mutableState.update {
                    it.copy(busy = false, message = Message.Error(PASSKEYS_GENERIC))
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
     * wrap it in a portable JSON document, and hand it to the OS share sheet.
     * The E2EE content lives on-device and is exported via the encrypted
     * Backup, not here.
     */
    public fun exportData(context: Context) {
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
                val send = Intent(Intent.ACTION_SEND).apply {
                    type = "application/json"
                    putExtra(Intent.EXTRA_TITLE, "lakko-account-export.json")
                    putExtra(Intent.EXTRA_TEXT, jsonText)
                }
                val chooser = Intent.createChooser(send, UiText("settings.data.export").render()).apply {
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                }
                context.startActivity(chooser)
                // The share sheet is the confirmation; no status line.
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
    /** The credential ids (base64url, no padding) whose passkeys restore
     *  keys; `null` when not known. */
    val withKeys: List<String>? = null,
    /** The open "Let this passkey restore your keys?" choice, or `null`. */
    val choice: PasskeyChoice? = null,
)

/** One open keys choice. [prfOutput] is what the registration yielded, when
 *  it yielded it; otherwise "Turn on" asks the passkey once more. */
public class PasskeyChoice(
    public val credentialIdB64: String,
    public val nickname: String,
    public val prfOutput: ByteArray?,
)

public sealed interface Message {
    public data class Ok(val text: UiText) : Message
    public data class Error(val text: UiText) : Message
}

/** A3 — the portable JSON document handed to the share sheet. Wraps the
 *  server-held [AccountExport] with provenance + an E2EE-content note. */
@Serializable
private data class ExportDoc(
    val schema: String,
    @SerialName("exported_at") val exportedAt: String,
    val server: AccountExport,
    val note: String,
)
