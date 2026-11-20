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
        client.setSessionToken(repo.accountForDeletion()?.bearer)
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
                me.lakko.app.rethrowIfCancelled(e) {
                    mutableState.update { it.copy(loading = false) }
                }
                mutableState.update {
                    it.copy(
                        loading = false,
                        message = Message.Error(failure(e, PASSKEYS_GENERIC)),
                    )
                }
            }
        }
    }

    /**
     * Add a passkey. The fresh-auth proof (UX section 17) is obtained by the
     * screen's proof block BEFORE this is called; when it ran out in between
     * nothing was added and [onProofRequired] makes the block appear, with no
     * error.
     */
    public fun addPasskey(context: Context, onProofRequired: () -> Unit = {}) {
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
            } catch (_: me.lakko.app.auth.FreshAuthRequired) {
                mutableState.update { it.copy(busy = false, message = null) }
                onProofRequired()
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
                me.lakko.app.rethrowIfCancelled(e) {
                    mutableState.update { it.copy(busy = false) }
                }
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
                me.lakko.app.rethrowIfCancelled(e) {
                    mutableState.update { it.copy(busy = false) }
                }
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
     * device. Auth `DELETE /account` (UX 17.3), which the server gates one of
     * two ways:
     *
     *   - an account WITH a passkey ([hasPasskey]): the ADR-0014 action
     *     ceremony bound to this account is itself the proof. ONE passkey
     *     sheet, never a passkey sign-in first; the `x-lakko-fresh-grant`
     *     header is not required and is sent only when a proof happens to be
     *     live;
     *   - an account with NO passkey: the header is required. The dialog's
     *     proof block obtains the proof BEFORE this is called; with none
     *     live, or when the server refuses it (`403 fresh-auth-required`),
     *     nothing was deleted and [onProofRequired] makes the block appear;
     *     that is not an error.
     *
     * A plain `403` for an account this screen thought had no passkey runs
     * the action ceremony once and retries, fail-closed. Then the best-effort
     * discovery purge and [onAccountDeleted], the same local wipe + sign-out
     * Forget device runs. Every failure is left on
     * [PasskeysUiState.deleteError], inside the dialog, which stays open.
     */
    public fun deleteAccount(context: Context, hasPasskey: Boolean, onProofRequired: () -> Unit) {
        rebindSession()
        // The live session's account, or (ADR-0089) a suspended account's on
        // a device without its chats, with the restricted session.
        val secrets = repo.accountForDeletion() ?: run {
            mutableState.update { it.copy(deleteError = SESSION_NOT_READY) }
            return
        }
        if (mutableState.value.busy) return
        // `null` when no proof is live: a passkey account goes on without it.
        val grant = repo.freshGrant()
        if (me.lakko.app.ui.auth.deleteNeedsProofFirst(hasPasskey = hasPasskey, proofLive = grant != null)) {
            onProofRequired()
            return
        }
        mutableState.update { it.copy(busy = true, deleteError = null, deleteCancelled = false) }
        viewModelScope.launch {
            try {
                suspend fun assertDeletion() {
                    client.assertAction(
                        context = context,
                        actionHashB64 = actionHashForDeleteAccount(secrets.accountId),
                        registration = registration,
                    )
                }
                // ADR-0014: a passkey account always makes the gesture for
                // THIS deletion, whatever was proven before, and makes only
                // this one.
                if (hasPasskey) assertDeletion()
                // ADR-0078 section 10: the invite ledgers name no owner, so
                // the directory cannot delete this account's links. Revoke
                // every live link this device keeps a record of first, while
                // the session still works; one it has no record of lives
                // until it expires.
                try {
                    DiscoveryClient().use { discovery ->
                        discovery.setSessionToken(secrets.bearer)
                        me.lakko.app.contacts.OwnLinks.revokeAll(
                            me.lakko.app.auth.base64Encode(secrets.accountId), discovery,
                        )
                    }
                } catch (e: Throwable) {
                    me.lakko.app.rethrowIfCancelled(e) {}
                }
                try {
                    client.deleteAccount(grant)
                } catch (e: WebauthnHttpError) {
                    when {
                        me.lakko.app.auth.isFreshAuthRequired(e) -> throw e
                        e.status == 403 && !hasPasskey -> {
                            // The account has a passkey this screen did not
                            // know of: the action ceremony, then once more.
                            assertDeletion()
                            client.deleteAccount(grant)
                        }
                        else -> throw e
                    }
                }
                // Best-effort discovery purge — the auth tier is the critical
                // erasure; the session token is about to be invalidated anyway.
                // ADR-0089 section 4 - a suspended account sends the `s1`,
                // which discovery's and storage's account deletions accept.
                try {
                    val discovery = DiscoveryClient()
                    discovery.setSessionToken(secrets.bearer)
                    discovery.deleteAccount()
                } catch (e: Throwable) {
                    me.lakko.app.rethrowIfCancelled(e) {}
                    // best-effort
                }
                // The encrypted backup goes with the account (best effort,
                // like the directory: the auth erasure above is the critical one).
                try {
                    me.lakko.app.storage.StorageClient().use { storage ->
                        storage.setSessionToken(secrets.bearer)
                        storage.deleteBackup()
                    }
                } catch (e: Throwable) {
                    me.lakko.app.rethrowIfCancelled(e) {}
                    // best-effort
                }
                // Local wipe + full sign-out on this device: the same path
                // Forget device takes (every per-account store, push first).
                onAccountDeleted()
            } catch (e: Throwable) {
                me.lakko.app.rethrowIfCancelled(e) {
                    mutableState.update { it.copy(busy = false) }
                }
                if (me.lakko.app.auth.isFreshAuthRequired(e)) {
                    // The proof ran out between the block and the call:
                    // nothing was deleted; the dialog shows the block.
                    repo.dropFreshProof()
                    mutableState.update { it.copy(busy = false, deleteError = null) }
                    onProofRequired()
                    return@launch
                }
                mutableState.update {
                    it.copy(
                        busy = false,
                        deleteError = deleteAccountFailure(e),
                        // Dismissing the sheet is not an error (UX 17.1).
                        deleteCancelled = e is WebauthnUserCancelledError,
                    )
                }
            }
        }
    }

    /** The delete dialog closed: its error line is not kept. */
    public fun clearDeleteError() {
        mutableState.update { it.copy(deleteError = null, deleteCancelled = false) }
    }

    /** The sentence for a deletion that did not happen (UX 17.3). */
    private fun deleteAccountFailure(e: Throwable): UiText = when (e) {
        is WebauthnUserCancelledError -> UiText("settings.account.delete.error.cancelled")
        is java.io.IOException -> UiText("common.error.offline")
        is WebauthnHttpError -> {
            Log.w("PasskeysViewModel", "WARN delete account refused: ${e.status}")
            deleteAccountRefusalText(e.status, e.body, me.lakko.app.l10n.Localization.strings.value.language)
        }
        else -> {
            Log.w("PasskeysViewModel", "WARN delete account failed: ${e.javaClass.simpleName}")
            UiText("settings.account.delete.error.generic")
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
                me.lakko.app.rethrowIfCancelled(e) {
                    mutableState.update { it.copy(busy = false) }
                }
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
    /** Why the account was not deleted, shown inside the delete dialog. */
    val deleteError: UiText? = null,
    /** [deleteError] says the passkey sheet was dismissed: drawn as a plain
     *  line, with no error styling. */
    val deleteCancelled: Boolean = false,
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

/** A3 — the portable JSON document the person saves as a file. Wraps the
 *  server-held [AccountExport] with provenance + an E2EE-content note. */
@Serializable
private data class ExportDoc(
    val schema: String,
    @SerialName("exported_at") val exportedAt: String,
    val server: AccountExport,
    val note: String,
)

/**
 * The sentence for a `DELETE /account` the server refused. ADR-0074 section
 * 1.3: while the account can still be taken back after a key reset the
 * server answers `409 recall-window-open` with the date, and the dialog says
 * until when ([me.lakko.app.auth.deleteBlockedUntilMs]); the date is the
 * platform's medium date in the interface [language].
 */
internal fun deleteAccountRefusalText(status: Int, body: String?, language: String): UiText {
    val blockedUntilMs = me.lakko.app.auth.deleteBlockedUntilMs(status, body)
    return when {
        blockedUntilMs != null -> UiText(
            "settings.account.delete.blocked",
            texts = mapOf(
                "date" to me.lakko.app.l10n.DeviceFormats.mediumDate(blockedUntilMs, language),
            ),
        )
        status == 429 -> UiText("common.error.too_many_attempts")
        else -> UiText("settings.account.delete.error.generic")
    }
}
