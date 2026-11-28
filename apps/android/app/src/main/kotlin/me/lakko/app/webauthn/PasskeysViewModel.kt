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
 *   * [addPasskey]         — create a passkey and store the keys for it,
 *     with nothing to choose (UX 10.2, amended 2026-11-11)
 *   * [restoreKeys] / [cancelRepair] — a row's "Restore keys with this
 *     passkey", with the phrase input when this device holds no phrase
 *   * [deleteCredential]   — `DELETE /webauthn/credentials/:id` (the server
 *     also deletes what the passkey stored for restoring keys)
 */
public class PasskeysViewModel(
    private val repo: AuthRepository,
    /** Run once the server-side erasure succeeded: the app's Forget device
     *  orchestration (MainActivity), so Delete account leaves exactly what
     *  Forget device leaves: nothing of the account on this device. */
    private val onAccountDeleted: () -> Unit,
    /**
     * ADR-0098 section 1 — post the account-deleted statement in every
     * conversation this device holds ([me.lakko.app.chat.ChatRepository.announceAccountDeleted]):
     * the candidate 64-byte account seeds, and the conversations already
     * reached across retries. `null` sends nothing.
     */
    private val announceDeletion: (suspend (seeds: List<ByteArray>, sent: MutableSet<String>) -> Unit)? = null,
    private val client: WebauthnClient = WebauthnClient(),
    private val registration: WebauthnRegistration = WebauthnRegistration(),
    /** For the passkeys this device learned cannot carry keys
     *  ([PasskeyNoPrfStore]); without it none is known. */
    private val appContext: Context? = null,
) : ViewModel() {

    /** The repository, for the proof blocks the screen hosts. */
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

    /** The last-factor dialog was closed. */
    public fun closeLastFactor() {
        mutableState.update { it.copy(lastFactor = false) }
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
                val knownNoPrf = knownNoPrf(res.credentials.map { it.credentialIdB64 }.toSet())
                mutableState.update {
                    it.copy(
                        loading = false,
                        credentials = res.credentials,
                        withKeys = withKeys,
                        knownNoPrf = knownNoPrf,
                    )
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
                // UX 10.2: the keys are stored for the new passkey with
                // nothing to choose; the line says how that ended.
                val added = addPasskeyWithKeys(
                    context = context,
                    repo = repo,
                    nickname = trimmed,
                    client = client,
                    registration = registration,
                )
                val text = addedPasskeyText(added.keys, trimmed)
                mutableState.update {
                    it.copy(
                        busy = false,
                        nickname = "",
                        message = if (addedPasskeyIsError(added.keys)) Message.Error(text) else Message.Ok(text),
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
            } catch (_: PasskeyAlreadyExistsError) {
                // UX 10.2: this device has one already; nothing changed.
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Error(UiText("settings.passkeys.error.already_exists")),
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
     * A row's "Restore keys with this passkey" (UX 10.2): one passkey prompt
     * limited to [credential]. When this device holds no recovery phrase the
     * phrase input opens first ([PasskeysUiState.repair]) and the person's
     * [typedPhrase] comes back here, checked against this account.
     */
    public fun restoreKeys(context: Context, credential: CredentialSummary, typedPhrase: String? = null) {
        rebindSession()
        if (mutableState.value.busy) return
        val credentialIdB64 = credential.credentialIdB64
        mutableState.update { it.copy(busy = true, message = null, repairError = null) }
        viewModelScope.launch {
            val end = runPasskeyKeysStore(
                onFailure = { cause ->
                    Log.w("PasskeysViewModel", "WARN restoring keys with a passkey failed: ${cause.javaClass.simpleName}")
                },
            ) {
                repo.turnOnPasskeyKeys(
                    activityContext = context,
                    credentialIdB64 = credentialIdB64,
                    registrationPrfOutput = null,
                    typedPhrase = typedPhrase,
                )
            }
            if (end == PasskeyKeysStoreEnd.NoPrf) {
                (repo.state.value as? AuthState.Session)?.let { session ->
                    PasskeyNoPrfStore.record(context, session.account.accountIdB64, credentialIdB64)
                }
            }
            val text = restoreKeysText(end)
            mutableState.update { s ->
                val open = s.repair?.takeIf { it.credentialIdB64 == credentialIdB64 }
                when (end) {
                    PasskeyKeysStoreEnd.PhraseNeeded -> s.copy(
                        busy = false,
                        repair = PasskeyRepair(credentialIdB64, credential.nickname),
                    )
                    // The sheet was dismissed: nothing changed, nothing to say.
                    PasskeyKeysStoreEnd.Cancelled -> s.copy(busy = false)
                    PasskeyKeysStoreEnd.Stored -> s.copy(busy = false, repair = null, message = text?.let(Message::Ok))
                    PasskeyKeysStoreEnd.NoPrf -> s.copy(busy = false, repair = null, message = text?.let(Message::Error))
                    // A refusal while the phrase input is open stays inside it.
                    else -> if (open != null) {
                        s.copy(busy = false, repairError = text)
                    } else {
                        s.copy(busy = false, message = text?.let(Message::Error))
                    }
                }
            }
            if (end == PasskeyKeysStoreEnd.Stored || end == PasskeyKeysStoreEnd.NoPrf) refresh()
        }
    }

    /** The phrase input of a repair was closed: nothing changed. */
    public fun cancelRepair() {
        if (mutableState.value.busy) return
        mutableState.update { it.copy(repair = null, repairError = null) }
    }

    /** The passkeys among [current] this device learned cannot carry keys;
     *  ids of passkeys the account no longer has are dropped. */
    private fun knownNoPrf(current: Set<String>): Set<String> {
        val context = appContext ?: return emptySet()
        val account = (repo.state.value as? AuthState.Session)?.account?.accountIdB64 ?: return emptySet()
        PasskeyNoPrfStore.retain(context, account, current)
        return PasskeyNoPrfStore.of(context, account)
    }

    public fun deleteCredential(
        context: Context,
        credentialIdB64: String,
        nickname: String,
    ) {
        rebindSession()
        if (mutableState.value.busy) return
        // UX 19.1c - the account's last second way in cannot be removed: the
        // app says so before any passkey sheet. The server refuses it too.
        val factors = repo.accountFactors.value
        if (factors != null &&
            me.lakko.app.auth.removalLeavesNoFactor(
                factors,
                me.lakko.app.auth.FactorKind.Passkey,
                passkeyCount = mutableState.value.credentials.size,
            )
        ) {
            mutableState.update { it.copy(lastFactor = true, message = null) }
            return
        }
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
                // ADR-0101 sections 7 and 10: the account's published
                // protection and its owned-channels locks follow the removal
                // (a deleted passkey that kept the chats no longer does).
                repo.refreshSealsAfter("passkey-removed")
            } catch (e: WebauthnUserCancelledError) {
                mutableState.update {
                    it.copy(
                        busy = false,
                        message = Message.Error(UiText("settings.passkeys.remove.error.cancelled")),
                    )
                }
            } catch (e: WebauthnHttpError) {
                if (me.lakko.app.auth.isLastFactor(e)) {
                    mutableState.update { it.copy(busy = false, lastFactor = true) }
                } else {
                    mutableState.update {
                        it.copy(busy = false, message = Message.Error(explain(e)))
                    }
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
     * the action ceremony once and retries, fail-closed. Only once auth has
     * deleted the account, the bounded best-effort cleanups only this device
     * can do (its push subscriptions, the backup blob, the directory rows),
     * then [onAccountDeleted], the same local wipe + sign-out Forget device
     * runs ([me.lakko.app.account.AccountDeletion]). Every failure is left on
     * [PasskeysUiState.deleteError], inside the dialog, which stays open.
     */
    public fun deleteAccount(context: Context, hasPasskey: Boolean, onProofRequired: () -> Unit) {
        rebindSession()
        // The live session's account, or (ADR-0099) a suspended account's on
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
                // ADR-0064 section 1 — the vaults of the channels this
                // account owns go before the account, best effort and
                // bounded in time; keys from the lineage identity.
                // ADR-0099: a suspended device without its chats holds no seed.
                // ADR-0101 section 7 item 9 - the owned-channels list and
                // its locks go with the account, the same way.
                secrets.accountIdentitySeed?.let { seed ->
                    me.lakko.app.channels.OwnedChannelsRuntime.deleteAllWithSeed(
                        context = context,
                        account = me.lakko.app.auth.base64Encode(secrets.accountId),
                        lineageSeed = me.lakko.app.auth.conversationAccountSeedOf(
                            context,
                            me.lakko.app.auth.base64Encode(secrets.accountId),
                            seed,
                        ),
                    )
                }
                secrets.accountIdentitySeed?.let { seed ->
                    me.lakko.app.channels.ChannelVaultHeads.deleteOwnedVaults(
                        me.lakko.app.auth.conversationAccountSeedOf(
                            context,
                            me.lakko.app.auth.base64Encode(secrets.accountId),
                            seed,
                        ),
                    )
                }
                // The push subscriptions name no account, so only this
                // device can remove them, and only after auth deleted the
                // account (a refusal must leave them). Each removal spends a
                // push-register token, which auth issues to a session: they
                // are taken in `prepare`, while the account still has one.
                val accountIdB64 = me.lakko.app.auth.base64Encode(secrets.accountId)
                val subscriptionIds = me.lakko.app.push.PushRouteStore(context).subscriptionIds(accountIdB64)
                val pushTokens = me.lakko.app.anon.PrefetchedAnonTokens(emptyList(), me.lakko.app.anon.AnonTokens.shared)
                me.lakko.app.account.AccountDeletion(
                    prepare = {
                        val needed = me.lakko.app.account.AccountDeletion.pushTokensNeeded(subscriptionIds.size)
                        val got = me.lakko.app.anon.AnonTokens.reserve(
                            me.lakko.app.anon.AnonTokenContext.PUSH_REGISTER, needed,
                        )
                        pushTokens.add(got)
                        if (got.size < needed) Log.w("PasskeysViewModel", "WARN delete account: ${got.size} of $needed push tokens")
                    },
                    deleteAtAuth = {
                        try {
                            client.deleteAccount(grant)
                        } catch (e: WebauthnHttpError) {
                            when {
                                me.lakko.app.auth.isFreshAuthRequired(e) -> throw e
                                e.status == 403 && !hasPasskey -> {
                                    // The account has a passkey this screen did
                                    // not know of: the action ceremony, then
                                    // once more.
                                    assertDeletion()
                                    client.deleteAccount(grant)
                                }
                                else -> throw e
                            }
                        }
                    },
                    // ADR-0098 - the session refuses from here on; no
                    // cleanup's 401 may sign this device out before the
                    // conversations are told.
                    afterAuth = { me.lakko.app.auth.SessionTokens.provider?.markAccountDeleted() },
                    // ADR-0098: the statement first, to the end, before the
                    // directory entry is removed (walk 10: running both at
                    // once rebuilt the conversation identity and stopped it).
                    // A suspended device without its chats holds no seed and
                    // has nothing to tell.
                    first = listOfNotNull(
                        announceDeletion?.let { announce ->
                            secrets.accountIdentitySeed?.let { seed ->
                                me.lakko.app.account.accountDeletedAnnouncement(
                                    seeds = deletionAnnouncementSeeds(context, accountIdB64, seed),
                                    announce = announce,
                                )
                            }
                        },
                    ),
                    cleanups = accountDeletionCleanups(subscriptionIds, pushTokens, secrets.bearer),
                    // Local wipe + full sign-out on this device: the same path
                    // Forget device takes (every per-account store, push first).
                    wipe = onAccountDeleted,
                ).run()
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

    /**
     * What Delete account removes on the servers after auth deleted the
     * account, before the device is wiped ([me.lakko.app.account.AccountDeletion]).
     * The storage and directory calls carry the session token taken before
     * the delete: both services verify it on its own, with no account row,
     * until it expires. A push unsubscribe carries no session, only a token
     * from [pushTokens].
     */
    private fun accountDeletionCleanups(
        subscriptionIds: List<ByteArray>,
        pushTokens: me.lakko.app.anon.AnonTokenSource,
        /** The access token, or (ADR-0099 section 4) a suspended account's
         *  `s1`, which storage's and discovery's account deletions accept. */
        sessionToken: String,
    ): List<me.lakko.app.account.AccountDeletion.Cleanup> {
        val push = me.lakko.app.push.PushSubscriptionsRemoval(subscriptionIds)
        return listOf(
            me.lakko.app.account.AccountDeletion.Cleanup("push subscriptions") {
                me.lakko.app.push.PushClient(anonTokens = pushTokens).use { pushGw ->
                    push.run { subId -> pushGw.unsubscribe(subId) }
                }
            },
            me.lakko.app.account.AccountDeletion.Cleanup("backup") {
                me.lakko.app.storage.StorageClient().use { storage ->
                    storage.setSessionToken(sessionToken)
                    storage.deleteBackup()
                }
            },
            me.lakko.app.account.AccountDeletion.Cleanup("directory") {
                DiscoveryClient().use { discovery ->
                    discovery.setSessionToken(sessionToken)
                    discovery.deleteAccount()
                }
            },
        )
    }

    /**
     * ADR-0098 — the account seeds the statement may be signed with: this
     * account's current seed and every own predecessor's (after an ADR-0074
     * rotation the conversation leaves stay bound under the earliest). The
     * chat repository picks the one its binding names. Fresh copies.
     */
    private fun deletionAnnouncementSeeds(context: Context, accountIdB64: String, current: ByteArray): List<ByteArray> {
        val predecessors = try {
            me.lakko.app.auth.OwnPredecessorStore.onDevice(context).load(accountIdB64)
        } catch (_: Throwable) {
            emptyList()
        }
        return try {
            predecessors.map { it.phraseSeed.copyOf() } + current.copyOf()
        } finally {
            predecessors.forEach { it.zero() }
        }
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
    /** The credential ids this device learned cannot carry keys: their rows
     *  offer no "Restore keys with this passkey". */
    val knownNoPrf: Set<String> = emptySet(),
    /** The open phrase input of a "Restore keys with this passkey", or
     *  `null`. */
    val repair: PasskeyRepair? = null,
    /** Why the open repair did not store the keys, shown inside it. */
    val repairError: UiText? = null,
    /** Why the account was not deleted, shown inside the delete dialog. */
    val deleteError: UiText? = null,
    /** [deleteError] says the passkey sheet was dismissed: drawn as a plain
     *  line, with no error styling. */
    val deleteCancelled: Boolean = false,
    /** The passkey to remove is the account's last second way in (UX
     *  19.1c): the app's own dialog says so; nothing was removed. */
    val lastFactor: Boolean = false,
)

/** A "Restore keys with this passkey" waiting for the recovery phrase,
 *  which this device does not hold. */
public data class PasskeyRepair(
    public val credentialIdB64: String,
    public val nickname: String,
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
