// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.util.Log
import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.withContext
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.auth.base64Encode
import me.lakko.app.profile.toFfi
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ConversationKind
import me.lakko.app.chat.ConversationKindStore
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.InviteCapability
import uniffi.lakko_ffi_uniffi.composeInviteCapability

/**
 * In-memory state holder for the user's contacts surface.
 *
 * Wraps [ContactsClient] (1.15e5) and pairs it with the active
 * session's [IdentitySecrets] from [AuthRepository]. Exposes a
 * single [StateFlow] of [ContactsState] for the ViewModel to
 * `collectAsStateWithLifecycle` against.
 *
 * **Persistence** — every store is keyed on the signed-in ACCOUNT: the
 * accepted list with its renames ([AcceptedContactsStore], per account since
 * 2026-09-30 — before that one device-wide list leaked a first account's
 * contacts into a second account signed in on the same phone), and — since
 * the 2026-09-16 live finding that a relaunch re-fetched the whole inbox from
 * cursor `0` (accepted people back under "Requests", duplicate pending rows,
 * declined requests returning) — the stranger-inbox cursor + the pending list
 * ([ContactsInboxStore]). Both are loaded when the repository binds to the
 * account ([bindAccount], from construction when a session is already live,
 * from the session bootstrap, and lazily from every operation); nothing is
 * read or written for an unbound account. The inbox cursor is saved after
 * each successful poll batch has landed in state, the pending list and the
 * accepted list after each of their mutations. Log out keeps the accepted
 * list and the inbox cursor + pending list for the next login of the same
 * account (deleting the inbox there brought every declined request back);
 * Forget device deletes them.
 * The sent log + outstanding invites stay in memory (no server mirror to
 * rehydrate from).
 *
 * **No continuous polling yet** — manual refresh via
 * [refreshInbox]. A polling loop lands in 1.15e7 alongside the
 * service-worker-style "fetch in the background" plumbing.
 *
 * ## Identity-secrets gate
 *
 * Every operation that signs / decrypts requires
 * [AuthRepository.identitySecrets] to be non-null. A cold-start
 * restore now either populates them or lands signed out, so a
 * live session normally has them; if a poll still finds them
 * `null` (e.g. racing a logout) the repository surfaces
 * [ContactsState.requiresReauth] so the screen can render a
 * "please re-enter password" panel instead of the normal
 * contacts list. The flag clears on the next poll that has them.
 */
public class ContactsRepository(
    private val authRepo: AuthRepository,
    /**
     * Application [Context] used to build the production
     * [PeerPinStore] for the G-4 (ADR-0016) fetch-path supersession
     * gate. When non-null, the default [clientFactory] wires a
     * persistent, per-account pin store into every [ContactsClient]
     * so a changed peer AccountIdentity key fails closed without a
     * valid supersession proof. When `null` (tests) the factory falls
     * back to an ephemeral [InMemoryPinStore] — the gate still runs
     * (it can never be disabled), it just does not persist across a
     * relaunch. Production (`MainActivity`) always passes
     * `applicationContext`, so the pin is persistent there.
     */
    appContext: Context? = null,
    /** Per-account persistence for the stranger-inbox cursor + pending list.
     *  Defaults to the `SharedPreferences` store when a [Context] is supplied
     *  and to `null` (in-memory only, as before) otherwise; tests may inject
     *  an [InMemoryContactsInboxStore]. */
    private val inboxStore: ContactsInboxStore? =
        appContext?.let { SharedPreferencesContactsInboxStore(it) },
    private val clientFactory: () -> ContactsClient = {
        ContactsClient(
            // Non-null by construction (ContactsClient requires a store):
            // persistent PeerPinStore in production, ephemeral
            // InMemoryPinStore when no Context is supplied (tests). The
            // verify gate is therefore never silently disabled.
            // Scoped to the signed-in account: a second account on this
            // phone neither reads nor inherits the first account's pins.
            pinStore = appContext?.let { ctx ->
                authRepo.identitySecrets.value?.let { PeerPinStore(ctx, base64Encode(it.accountId)) }
            } ?: InMemoryPinStore(),
            // KT-log S3e — persistent no-rollback high-water-mark store in
            // production (so a relaunch can't re-arm a rollback by
            // forgetting the highest log_size); ephemeral in tests. The log
            // check itself is still OFF unless a transparency URL +
            // operator key are build-time configured.
            lastSeenRoot = appContext?.let {
                me.lakko.app.transparency.LastSeenRoot(it)
            } ?: me.lakko.app.transparency.InMemoryLastSeenRoot(),
        )
    },
    /** Factory for the [me.lakko.app.discovery.DiscoveryClient] the invite-
     *  link operations need ([generateInviteLink] /
     *  [revokeInviteLink]). Distinct factory rather than reusing the
     *  one inside [ContactsClient] because invite operations don't go
     *  through `ContactsClient` — they're discovery-side operations the
     *  Repository drives directly so the orchestration sits next to
     *  the `outstandingInvites` state. */
    private val discoveryClientFactory: () -> me.lakko.app.discovery.DiscoveryClient = {
        me.lakko.app.discovery.DiscoveryClient()
    },
    /** Optional bridge to the chat surface. When non-null,
     *  [refreshInbox] forwards Welcome envelopes to
     *  [ChatRepository.joinGroupFromWelcome] so a peer's
     *  startGroupWith lands a live 1×1 chat on this device.
     *  Tests + the contacts-only configurations pass `null` and
     *  Welcomes drop on the floor (the cursor still advances). */
    private val chatRepo: ChatRepository? = null,
    /** UX 5.3 — the link secrets of the `/i/` links this device created, kept
     *  per account across restarts (EncryptedSharedPreferences in production,
     *  in memory without a [Context]). */
    private val inviteSecretStore: InviteLinkSecretStore =
        appContext?.let { EncryptedInviteLinkSecretStore(it) } ?: InMemoryInviteLinkSecretStore(),
) {
    /**
     * Build a [DiscoveryClient] pre-stamped with the current
     * session's bearer token (Phase 1.5b). Every protected
     * discovery write (register, identity upload, keypackage
     * upload, invite upload, invite revoke) needs the
     * `Authorization: Bearer <token>` header; reading
     * `authRepo.identitySecrets.value?.sessionToken` at
     * factory-call time keeps the threading model simple.
     */
    private fun authenticatedDiscoveryClient(): me.lakko.app.discovery.DiscoveryClient {
        val client = discoveryClientFactory()
        client.setSessionToken(authRepo.identitySecrets.value?.sessionToken)
        return client
    }

    /** Application context for the accepted-contacts persistence
     *  (ADR-0021). `null` in tests → in-memory only, as before. */
    private val appCtx: Context? = appContext

    private val mutableState: MutableStateFlow<ContactsState> =
        // Empty until [bindAccount]: the accepted list, the inbox cursor and
        // the pending list are all per account, so they load once the
        // account is known (at the end of construction when a session is
        // already live).
        MutableStateFlow(ContactsState())

    public val state: StateFlow<ContactsState> = mutableState.asStateFlow()

    // -----------------------------------------------------------------
    // Per-account persistence (accepted list + inbox cursor + pending)
    // -----------------------------------------------------------------

    private val bindLock = Any()

    /** The account whose accepted list and inbox snapshot are loaded into
     *  [state]; `null` until [bindAccount] ran, and again after [clear].
     *  Read by the persist helpers, so nothing is written for an unbound
     *  account and never into another account's list. */
    @Volatile
    private var boundAccountB64: String? = null

    /**
     * Load [accountId]'s persisted accepted list, inbox cursor and pending
     * list into [state]. Idempotent per account. Called at construction when
     * a session is already live, from the session bootstrap, and lazily from
     * every operation, so the first poll after a relaunch resumes from the
     * saved cursor instead of re-fetching the whole inbox.
     *
     * Binding a DIFFERENT account than the one bound starts from an empty
     * state: nothing of the previous account's survives into the new one.
     * On a first bind, contacts already in memory (a mutation that ran
     * before the bind) are merged into the stored list and persisted.
     *
     * Rows the persisted snapshot holds for people who are accepted by now
     * (a pairing bundle or a backup restore may have grown the accepted list
     * while the snapshot was stale) are dropped on load; anything already in
     * memory is merged under the same dedupe rule.
     */
    public fun bindAccount(accountId: ByteArray) {
        val accountB64 = base64Encode(accountId)
        var acceptedGrew = false
        synchronized(bindLock) {
            if (boundAccountB64 == accountB64) return
            val switching = boundAccountB64 != null
            val storedAccepted = appCtx?.let { AcceptedContactsStore.load(it, accountB64) }
            val snapshot = inboxStore?.load(accountB64) ?: ContactsInboxSnapshot()
            mutableState.update { current ->
                // `update` may re-run the lambda on a lost CAS.
                acceptedGrew = false
                val state = if (switching) ContactsState() else current
                val accepted = if (storedAccepted == null) {
                    state.accepted
                } else {
                    val known = storedAccepted.mapTo(HashSet()) { it.request.fromPubkey.toList() }
                    val extra = state.accepted.filter { known.add(it.request.fromPubkey.toList()) }
                    acceptedGrew = extra.isNotEmpty()
                    storedAccepted + extra
                }
                val merged = mergeFetchedRequests(
                    pending = emptyList(),
                    accepted = accepted,
                    fetched = snapshot.pending + state.pending,
                )
                state.copy(
                    accepted = accepted,
                    pending = merged.pending,
                    cursor = advanceInboxCursor(state.cursor, snapshot.cursor),
                )
            }
            boundAccountB64 = accountB64
        }
        // The learned names are per account too: screens re-read them.
        profileDirectory?.onAccountChanged()
        if (acceptedGrew) persistAccepted() else pushRenamesToDirectory()
    }

    /** [bindAccount] for the current session's account, when there is one. */
    private fun bindAccountIfSignedIn() {
        authRepo.identitySecrets.value?.let { bindAccount(it.accountId) }
    }

    /** Persist the current pending list for the bound account. Called after
     *  every mutation of [ContactsState.pending] has landed in state. */
    private fun persistInboxPending() {
        val store = inboxStore ?: return
        val accountB64 = boundAccountB64 ?: return
        store.savePending(accountB64, mutableState.value.pending)
    }

    /** Persist the current inbox cursor for the bound account. Called only
     *  after a successful poll batch has landed in state, never before. */
    private fun persistInboxCursor() {
        val store = inboxStore ?: return
        val accountB64 = boundAccountB64 ?: return
        store.saveCursor(accountB64, mutableState.value.cursor)
    }

    init {
        // ADR-0050 section 2.9 — a pairing state bundle's contacts land in the
        // live accepted list (and from there the store), never beside it: a
        // store-only write would be overwritten by the next [persistAccepted].
        if (appContext != null) {
            me.lakko.app.devices.StateBundle.contactsSink = { accountIdB64, incoming ->
                mergeAcceptedFor(accountIdB64, incoming)
            }
        }
    }

    /** [mergeAccepted] into [accountIdB64]'s live list, or `null` when this
     *  repository is not bound to that account (the caller then writes the
     *  store for that account directly). */
    public fun mergeAcceptedFor(accountIdB64: String, incoming: List<AcceptedContact>): Int? {
        bindAccountIfSignedIn()
        if (boundAccountB64 != AcceptedContactsStore.canonicalAccountB64(accountIdB64)) return null
        return mergeAccepted(incoming)
    }

    /** Add the [incoming] contacts not already accepted (by signature key) and
     *  persist. Returns how many were added. */
    public fun mergeAccepted(incoming: List<AcceptedContact>): Int {
        bindAccountIfSignedIn()
        // No account, no list to merge into: a write that landed now would
        // be carried into whichever account binds next.
        if (boundAccountB64 == null) return 0
        var added = 0
        var pendingChanged = false
        mutableState.update { state ->
            val known = state.accepted.map { it.request.fromPubkey.toList() }.toHashSet()
            val fresh = incoming.filter { known.add(it.request.fromPubkey.toList()) }
            added = fresh.size
            if (fresh.isEmpty()) return@update state
            val accepted = state.accepted + fresh
            // One row per person: a request from someone who just became a
            // contact by this route leaves "Requests".
            val pending = withoutAcceptedSenders(state.pending, accepted)
            pendingChanged = pending.size != state.pending.size
            state.copy(accepted = accepted, pending = pending)
        }
        if (added > 0) persistAccepted()
        if (pendingChanged) persistInboxPending()
        return added
    }

    /** Persist the current accepted list for the bound account (best-effort;
     *  no-op in tests where no Context was supplied, and for an unbound
     *  account). Called after every mutation of the accepted list. */
    private fun persistAccepted() {
        val ctx = appCtx
        val accountB64 = boundAccountB64
        if (ctx != null && accountB64 != null) {
            AcceptedContactsStore.save(ctx, accountB64, mutableState.value.accepted)
        }
        pushRenamesToDirectory()
    }

    // -----------------------------------------------------------------
    // A one-to-one conversation IS a contact
    // -----------------------------------------------------------------

    /**
     * Give the other person of a one-to-one conversation a contact row (see
     * [adoptingOneToOnePeer] for the rule and the defect it closes). Adds a
     * row for [signatureKey] when none exists, or completes an existing row's
     * `groupIdHex` when it was never recorded. Returns whether anything
     * changed; a changed list is persisted. Never overwrites a recorded chat,
     * a display name or an account id the row already holds.
     */
    public fun adoptOneToOnePeer(
        signatureKey: ByteArray,
        accountId: ByteArray?,
        groupIdHex: String,
        nowMs: ULong,
        createdAt: String,
    ): Boolean {
        bindAccountIfSignedIn()
        // No account, no list (see [mergeAccepted]).
        if (boundAccountB64 == null) return false
        var changed = false
        mutableState.update { state ->
            // `update` may re-run the lambda on a lost CAS; the verdict is the
            // last attempt's, so reset before deciding.
            changed = false
            val next = adoptingOneToOnePeer(
                accepted = state.accepted,
                signatureKey = signatureKey,
                accountId = accountId,
                groupIdHex = groupIdHex,
                nowMs = nowMs,
                createdAt = createdAt,
            ) ?: return@update state
            changed = true
            state.copy(accepted = next)
        }
        if (changed) persistAccepted()
        return changed
    }

    // -----------------------------------------------------------------
    // ADR-0048 — profile names
    // -----------------------------------------------------------------

    /** The process-wide name directory; `null` in tests (no Context). */
    private val profileDirectory: me.lakko.app.profile.ProfileDirectory? =
        appContext?.let { me.lakko.app.profile.ProfileDirectory.get(it) }

    init {
        // ADR-0048 §3 — username verification resolves a claim through the
        // plain discovery path (lookup -> bundle -> key), NOT the G-4 gate a
        // contact request runs: see [me.lakko.app.profile.resolveUsernameForVerification].
        // Attaching it also verifies every stored claim not yet verified.
        profileDirectory?.attachUsernameResolver { username -> resolveForVerification(username) }
        // ADR-0060 §5 — and since scoped discovery, the certificate path
        // replaces that lookup: a person outside Everyone answers no lookup.
        profileDirectory?.attachCertificateVerifier { username, certHex, accountHex, keyB64 ->
            verifyByCertificate(username, certHex, accountHex, keyB64)
        }
    }

    init {
        // A session already live at construction (a cold-start restore lands
        // before the repository is built): its contacts are the ones shown.
        // Placed after [profileDirectory] so the renames reach the directory.
        bindAccountIfSignedIn()
    }

    /** The username-certificate verifying key: the build-time pin, else the
     *  well-known key fetched once. */
    @Volatile
    private var usernameCertKey: ByteArray? = null

    private suspend fun usernameCertKey(): ByteArray? {
        usernameCertKey?.let { return it }
        val pinned = me.lakko.app.BuildConfig.LAKKO_USERNAME_CERT_KEY
        val key = if (pinned.isNotBlank()) {
            try {
                me.lakko.app.auth.base64UrlDecodeNoPad(pinned).takeIf { it.size == 32 }
            } catch (_: IllegalArgumentException) {
                null
            }
        } else {
            discoveryClientFactory().use { it.fetchUsernameCertKey() }
        }
        usernameCertKey = key
        return key
    }

    private suspend fun verifyByCertificate(
        username: String,
        certificateHex: String,
        accountIdHex: String,
        senderKeyB64: String,
    ): me.lakko.app.profile.UsernameResolution =
        try {
            val key = usernameCertKey()
            authenticatedDiscoveryClient().use { discovery ->
                clientFactory().use { client ->
                    me.lakko.app.profile.verifyUsernameByCertificate(
                        username = username,
                        certificateHex = certificateHex,
                        accountIdHex = accountIdHex,
                        senderKeyB64 = senderKeyB64,
                        lookupKey = { name -> discovery.oprfUsername(name).lookupKey },
                        verifyBinding = key?.let { k ->
                            { cert, lookupKey, account ->
                                try {
                                    uniffi.lakko_ffi_uniffi.verifyUsernameBinding(cert, k, lookupKey, account)
                                    true
                                } catch (_: Throwable) {
                                    false
                                }
                            }
                        },
                        fetchSignatureKey = { accountId -> client.fetchPublishedSignatureKey(accountId) },
                    )
                }
            }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (err: Throwable) {
            me.lakko.app.profile.UsernameResolution.Unavailable(err)
        }

    /** Hand the local renames (keyed by signature key) to the directory. */
    private fun pushRenamesToDirectory() {
        val directory = profileDirectory ?: return
        directory.setRenames(
            mutableState.value.accepted.mapNotNull { c ->
                val name = c.displayName?.trim()?.takeIf { it.isNotEmpty() } ?: return@mapNotNull null
                java.util.Base64.getEncoder().encodeToString(c.request.fromPubkey) to name
            }.toMap(),
        )
    }

    /**
     * ADR-0048 §3 — resolve a claimed username to the signature key discovery
     * serves for it, for the verifier.
     *
     * The plain path (lookup -> identity bundle -> key). It never reads or
     * writes a peer pin: verifying a displayed username is a comparison
     * against the sender's authenticated key, not a trust decision, and the
     * G-4 gate would TOFU-pin the key as a side effect. The client's pin
     * store is simply not consulted on this path.
     */
    private suspend fun resolveForVerification(
        username: String,
    ): me.lakko.app.profile.UsernameResolution =
        try {
            clientFactory().use { client ->
                me.lakko.app.profile.resolveUsernameForVerification(
                    username = username,
                    lookupAccountId = { name -> client.lookupAccountId(name) },
                    fetchSignatureKey = { accountId -> client.fetchPublishedSignatureKey(accountId) },
                )
            }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (err: Throwable) {
            // Constructing the client failed; no conclusion about the person.
            me.lakko.app.profile.UsernameResolution.Unavailable(err)
        }

    /** This account's own profile in wire form, or `null` when none is stored.
     *  A contact request states the username with its certificate (the person
     *  asked is being told who asks); it carries no reach grant — the reply
     *  grant rides beside it. */
    private fun ownProfileForWire(accountId: ByteArray): uniffi.lakko_ffi_uniffi.FfiProfile? {
        val ctx = appCtx ?: return null
        // ADR-0048 v2 — states this account's id alongside the name.
        return me.lakko.app.profile.OwnProfileStore.load(ctx, accountId)?.toFfi(
            accountId = accountId,
            usernameCertificate = reach()?.usernameCertificate(),
        )
    }

    /** ADR-0060 — the reach manager, `null` without a context (tests). */
    private fun reach(): me.lakko.app.reach.ReachManager? =
        appCtx?.let { me.lakko.app.reach.ReachManager.get(it) }

    /**
     * Resolve the username typed in the search box, compose a
     * `ContactRequest`, publish it to the recipient's mailbox.
     *
     * On success the recipient gets the request in their
     * stranger inbox; we add an entry to [ContactsState.sent]
     * so the UI can render an outgoing-request row.
     *
     * Throws [ContactsError] on any failure — same shape as the
     * underlying [ContactsClient.sendContactRequest].
     */
    public suspend fun sendContactRequest(
        recipientUsername: String,
        introMessage: String,
    ): SendResult {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(
                IllegalStateException(
                    "no identity secrets — please re-enter your password",
                ),
            )

        return useClientWith(secrets) { client, sender ->
            val result = client.sendContactRequest(
                senderSigningSeed = sender.signingSeed,
                senderAccountId = secrets.accountId,
                senderSignaturePubkey = sender.signaturePubkey,
                recipientUsername = recipientUsername,
                introMessage = introMessage.toByteArray(Charsets.UTF_8),
                // ADR-0048 §2 — who is asking, sealed to the recipient.
                senderProfile = ownProfileForWire(secrets.accountId),
                // ADR-0060 §6 — this request's reply key, registered before
                // the request leaves, and the grant that lets it in.
                mintReply = { key -> reach()?.mintReply(key) },
                reachFor = { key ->
                    reach()?.reachAuthFor(key, me.lakko.app.reach.WritePurpose.CONTACT_REQUEST)
                },
            )
            mutableState.update { state ->
                state.copy(
                    sent = state.sent + SentContactRequest(
                        recipientUsername = result.recipientUsername,
                        recipientAccountIdB64 = base64Encode(result.recipientAccountId),
                        messageId = result.messageId,
                        createdAt = result.createdAt,
                    ),
                )
            }
            result
        }
    }

    /**
     * ADR-0060 — a contact request to a person a search found by their
     * username (they are in Everyone). [grantWire] is the grant the search
     * found them under (`encodeReachGrant`), used for this one request and
     * never kept; without one the write takes the grant this device holds
     * for them, or goes unsigned into their open mailbox.
     */
    public suspend fun sendFoundContactRequestByUsername(
        recipientUsername: String,
        introMessage: ByteArray,
        grantWire: ByteArray?,
    ): SendResult {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(IllegalStateException("no identity secrets — please re-enter your password"))
        val explicit = grantWire?.let(::reachAuthOfGrantWire)
        return useClientWith(secrets) { client, sender ->
            val result = client.sendContactRequest(
                senderSigningSeed = sender.signingSeed,
                senderAccountId = secrets.accountId,
                senderSignaturePubkey = sender.signaturePubkey,
                recipientUsername = recipientUsername,
                introMessage = introMessage,
                senderProfile = ownProfileForWire(secrets.accountId),
                mintReply = { key -> reach()?.mintReply(key) },
                reachFor = { key ->
                    explicit ?: reach()?.reachAuthFor(key, me.lakko.app.reach.WritePurpose.CONTACT_REQUEST)
                },
            )
            recordSent(result.recipientUsername, result)
            result
        }
    }

    /**
     * ADR-0060 — a contact request to a person a search found, written to the
     * identity bundle the caller fetched and cross-checked against the key the
     * person was found as. [grantWire] as in [sendFoundContactRequestByUsername].
     * [shownUsername] (a verified one, or `null`) names the request in Sent.
     */
    public suspend fun sendFoundContactRequestToBundle(
        recipientAccountId: ByteArray,
        recipientSignaturePubkey: ByteArray,
        recipientX25519Pubkey: ByteArray,
        introMessage: ByteArray,
        grantWire: ByteArray?,
        shownUsername: String?,
    ): SendResult {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(IllegalStateException("no identity secrets — please re-enter your password"))
        val explicit = grantWire?.let(::reachAuthOfGrantWire)
        val replyGrant = reach()?.mintReply(recipientSignaturePubkey)
        return useClientWith(secrets) { client, sender ->
            val result = client.sendContactRequestToBundle(
                senderSigningSeed = sender.signingSeed,
                senderAccountId = secrets.accountId,
                senderSignaturePubkey = sender.signaturePubkey,
                recipientAccountId = recipientAccountId,
                recipientSignaturePubkey = recipientSignaturePubkey,
                recipientX25519Pubkey = recipientX25519Pubkey,
                introMessage = introMessage,
                senderProfile = ownProfileForWire(secrets.accountId),
                replyGrant = replyGrant,
                reach = explicit ?: reach()?.reachAuthFor(
                    recipientSignaturePubkey,
                    me.lakko.app.reach.WritePurpose.CONTACT_REQUEST,
                ),
            )
            // The Sent list shows `@username`; a person found by name alone
            // has none to show there.
            shownUsername?.let { recordSent(it, result) }
            result
        }
    }

    private fun recordSent(username: String, result: SendResult) {
        mutableState.update { state ->
            state.copy(
                sent = state.sent + SentContactRequest(
                    recipientUsername = username,
                    recipientAccountIdB64 = base64Encode(result.recipientAccountId),
                    messageId = result.messageId,
                    createdAt = result.createdAt,
                ),
            )
        }
    }

    private fun reachAuthOfGrantWire(wire: ByteArray): me.lakko.app.ds.DsReachAuth {
        val grant = uniffi.lakko_ffi_uniffi.decodeReachGrant(wire)
        return me.lakko.app.ds.DsReachAuth(me.lakko.app.ds.reachSigner(grant.seed))
    }

    /**
     * Resolve `@username -> (accountId, signaturePubkey)` through the
     * G-4-verified [ContactsClient.resolveVerifiedRecipient]. Used by the
     * group-chat add-member flow ([me.lakko.app.chat.ChatViewModel]); the
     * fresh [clientFactory] client carries the same pin store the 1x1
     * ContactRequest path verifies against.
     */
    public suspend fun resolveVerifiedRecipient(
        username: String,
    ): Pair<ByteArray, ByteArray> =
        clientFactory().use { it.resolveVerifiedRecipient(username) }

    /**
     * Poll the stranger inbox for new ContactRequest / Welcome
     * envelopes. Updates [ContactsState.pending] with any new
     * requests + bumps the cursor.
     *
     * Welcome envelopes — produced by a peer calling
     * [ChatRepository.startGroupWith] against us — are
     * forwarded to [chatRepo].joinGroupFromWelcome when a
     * [ChatRepository] reference is wired. The resulting
     * `groupIdHex` is left to the chat surface to surface;
     * this repository is concerned with the contact-side
     * lifecycle only.
     *
     * @param waitSecs DS long-poll budget. `0` (the default)
     *   returns immediately whatever's there — manual refresh
     *   from a button press; the UI spinner blips on /
     *   off. Non-zero values hold the connection up to that
     *   many seconds; the [ContactsState.refreshing] flag is
     *   **not** raised so the spinner doesn't flicker for the
     *   full long-poll window. 1.15e7's continuous-polling
     *   `LaunchedEffect` uses `waitSecs = 30`.
     */
    public suspend fun refreshInbox(waitSecs: Int = 0) {
        val secrets = authRepo.identitySecrets.value ?: run {
            mutableState.update { it.copy(requiresReauth = true) }
            return
        }
        // Resume from the persisted cursor (and show the persisted pending
        // rows) BEFORE the cursor is read below; idempotent once bound.
        bindAccount(secrets.accountId)
        val accountB64 = base64Encode(secrets.accountId)
        val isManualRefresh = waitSecs == 0
        if (isManualRefresh) {
            mutableState.update { it.copy(refreshing = true, lastError = null) }
        }
        // Off the main thread, for every caller. Everything below blocks:
        // `useClientWith` opens the identity over the FFI, and the poll holds
        // the connection for up to `waitSecs`. Both callers reach this from a
        // `LaunchedEffect` or a `viewModelScope`, i.e. the main dispatcher, so
        // without this the long poll freezes the UI for its whole window and
        // Android raises "Lakko isn't responding". Nothing here needs the main
        // thread: the state is a `MutableStateFlow` and the stores below it
        // are their own.
        withContext(Dispatchers.IO) {
            try {
                useClientWith(secrets) { client, sender ->
                    val result = client.pollStrangerInbox(
                        mySignaturePubkey = sender.signaturePubkey,
                        myX25519Secret = sender.x25519Secret,
                        after = mutableState.value.cursor,
                        waitSecs = waitSecs,
                        mailboxSigner = sender.mailboxSigner,
                    )
                    val newIncoming = result.events.filterIsInstance<
                        IncomingMailboxEvent.ContactRequest>(
                    ).map { it.request }
                    val welcomes = result.events.filterIsInstance<
                        IncomingMailboxEvent.Welcome>(
                    )
                    val acks = result.events.filterIsInstance<
                        IncomingMailboxEvent.ContactAck>(
                    ).map { it.ack }
                    // ADR-0060 §6 — a request's reply key is kept for the
                    // accept and the ack (bound to the signature-verified
                    // sender). This device's own reply key is NOT retired on
                    // their ack: the ack and the 1:1 Welcome are sent
                    // independently, and a Welcome landing after a retired key
                    // would be dropped by the closed mailbox. It is retired
                    // once the Welcome joined (below), else it expires.
                    reach()?.let { manager ->
                        for (req in newIncoming) {
                            val grant = req.replyGrant ?: continue
                            if (grant.`class` == uniffi.lakko_ffi_uniffi.FfiReachClass.REPLY) {
                                manager.keepHeld(req.fromPubkey, grant, scopeHex = "")
                            }
                        }
                    }
                    // Forward Welcomes to the chat repository so the
                    // peer's startGroupWith lands a live 1×1 chat.
                    // Best-effort: a single bad Welcome doesn't
                    // block the inbox poll from advancing the
                    // cursor for the rest of the events.
                    // `joinGroupFromWelcome` republishes this device's
                    // KeyPackage after every attempt, success or failure.
                    var joinFailure: String? = null
                    for (welcome in welcomes) {
                        if (chatRepo != null) {
                            try {
                                val joinedHex = chatRepo.joinGroupFromWelcome(welcome.welcomeBytes)
                                // ADR-0060 §6 — the answer to a request of this
                                // device's arrived: retire its reply key for
                                // that person.
                                chatRepo.oneToOnePeerKey(joinedHex)?.let { peer ->
                                    reach()?.retireRepliesTo(peer)
                                }
                            } catch (e: kotlinx.coroutines.CancellationException) {
                                throw e
                            } catch (e: Throwable) {
                                // ADR-0060 — every device of the account reads
                                // the one account mailbox, so a Welcome sealed
                                // to another of its devices lands here too and
                                // fails to open: not a failure of this device.
                                if (me.lakko.app.chat.isWelcomeForAnotherDevice(e)) continue
                                // A dropped Welcome is a chat someone started
                                // with this user that will never appear, so it
                                // must not vanish silently. The detail goes to
                                // the log; the inline error tells the user.
                                android.util.Log.w(
                                    "ContactsRepository",
                                    "joining a group from a Welcome failed",
                                    e,
                                )
                                // ADR-0053 — unless it is a co-publisher's
                                // Welcome, which shares this mailbox and is
                                // opened by the channel screen instead.
                                if (!me.lakko.app.channels.CoPublisher.awaitingCoPublishWelcome()) {
                                    joinFailure = WELCOME_JOIN_FAILED_MESSAGE
                                }
                            }
                        }
                    }
                    // ADR-0048 §3 — a request's profile is bound to its
                    // signature-verified `from_pubkey`; store it (strictly newer
                    // only) so the request card can say who is asking.
                    profileDirectory?.let { directory ->
                        for (req in newIncoming) {
                            val profile = req.profile ?: continue
                            directory.onProfileReceived(
                                signatureKeyB64 = java.util.Base64.getEncoder()
                                    .encodeToString(req.fromPubkey),
                                profile = profile,
                            )
                        }
                    }
                    // The session ended (or switched) during the poll: this batch
                    // belongs to the previous account's inbox. `clear()` already
                    // reset the state; do not land the rows or the cursor in it.
                    if (boundAccountB64 != accountB64) return@useClientWith

                    var dropped: List<IncomingContactRequest> = emptyList()
                    var acceptedChanged = false
                    mutableState.update { state ->
                        // Someone accepted one of this device's requests: they are
                        // a contact here too, with their account id.
                        val applied = applyContactAcks(state.accepted, state.sent, acks)
                        acceptedChanged = applied.acceptedChanged
                        // Dedupe: a row already pending (same DS messageId) or a
                        // sender already accepted is not a new request.
                        val merged = mergeFetchedRequests(
                            pending = state.pending,
                            accepted = applied.accepted,
                            fetched = newIncoming,
                        )
                        dropped = merged.dropped
                        state.copy(
                            accepted = applied.accepted,
                            sent = applied.sent,
                            pending = withoutAcceptedSenders(merged.pending, applied.accepted),
                            // Never backwards: a stale batch cannot re-arm a
                            // re-fetch of rows this device already handled.
                            cursor = advanceInboxCursor(state.cursor, result.cursor),
                            // Always clear the refreshing flag —
                            // even if this was a long-poll that
                            // didn't raise it, clearing is a no-op.
                            refreshing = false,
                            // Secrets were present for this poll, so any earlier
                            // re-auth prompt no longer applies.
                            requiresReauth = false,
                            // A failed join stays visible across the silent
                            // background long polls (which would otherwise clear
                            // it within seconds) until the user refreshes by hand.
                            lastError = joinFailure
                                ?: state.lastError.takeIf {
                                    !isManualRefresh && it == WELCOME_JOIN_FAILED_MESSAGE
                                },
                        )
                    }
                    if (dropped.isNotEmpty()) {
                        android.util.Log.d(
                            "ContactsRepository",
                            "inbox: dropped ${dropped.size} fetched request(s) already pending " +
                                "or from an accepted contact (ids ${dropped.map { it.messageId }})",
                        )
                    }
                    // Both writes strictly after the batch landed in state: a
                    // crash between the poll and here re-fetches the batch (the
                    // dedupe above absorbs it) rather than losing it.
                    if (acceptedChanged) persistAccepted()
                    if (newIncoming.isNotEmpty() || acceptedChanged) persistInboxPending()
                    persistInboxCursor()
                }
            } catch (e: ContactsError) {
                // Background long-polls surface failures on the `lastError`
                // slot too — the UI renders one plain sentence, never the
                // transport's own message (endpoint paths and pseudonyms are
                // not user copy). The raw error goes to the log; the caller
                // (a `LaunchedEffect` loop) retries on the next iteration.
                Log.w(TAG, "contacts inbox: poll failed", e)
                mutableState.update {
                    it.copy(refreshing = false, lastError = INBOX_FETCH_FAILURE_MESSAGE)
                }
            }
        }
    }

    /**
     * Mark an incoming ContactRequest as accepted locally.
     *
     * Moves the matching pending row to [ContactsState.accepted]
     * with an optional [groupIdHex] — set by the orchestration
     * layer (`ContactsViewModel.onAcceptClicked` from 1.15f4c)
     * when it has just called `ChatRepository.startGroupWith`
     * for this peer. The UI uses the id to offer an "Open chat"
     * affordance on the accepted row.
     *
     * Pure state mutation — the actual MLS group creation
     * happens in the caller before this method is invoked, so
     * the repository stays uncoupled from `ChatRepository` on
     * the Accept path (`refreshInbox` keeps the dependency on
     * the Welcome-forwarding side).
     */
    public fun acceptContactRequest(messageId: Long, groupIdHex: String? = null) {
        bindAccountIfSignedIn()
        mutableState.update { state ->
            val (accepted, remaining) = state.pending.partition { it.messageId == messageId }
            if (accepted.isEmpty()) return@update state
            // One row per person: a second request from a contact that is
            // already accepted (a re-send, or a request from their other
            // device) must not duplicate the accepted row. Seen live on
            // 2026-09-16 as "Web Wanda" listed twice, which the keyed
            // LazyColumn of the redesigned Contacts screen then refused.
            val known = state.accepted.map { it.request.fromPubkey.toList() }.toHashSet()
            val fresh = accepted.filter { it.fromPubkey.toList() !in known }
            state.copy(
                pending = remaining,
                accepted = state.accepted + fresh.map {
                    AcceptedContact(request = it, groupIdHex = groupIdHex)
                },
            )
        }
        persistAccepted()
        persistInboxPending()
    }

    /**
     * Set (or clear, when blank) the user-chosen display name for an
     * accepted contact (ADR-0021 follow-up). A blank name reverts the UI +
     * the group-call roster to the short pubkey fingerprint. Persists the
     * accepted list so the change survives a relaunch.
     */
    public fun renameContact(messageId: Long, newDisplayName: String) {
        bindAccountIfSignedIn()
        val trimmed = newDisplayName.trim()
        mutableState.update { state ->
            state.copy(
                accepted = state.accepted.map { contact ->
                    if (contact.request.messageId == messageId) {
                        contact.copy(displayName = trimmed.ifBlank { null })
                    } else {
                        contact
                    }
                },
            )
        }
        persistAccepted()
    }

    /**
     * Send a `ContactAck` back to the original sender's stranger
     * inbox so their UI flips from "pending" to "accepted"
     * without waiting on the Welcome arrival (Phase 1.9b1).
     *
     * Best-effort: called from the view-model's
     * `onAcceptClicked` after the local state mutation; a network
     * failure here is logged at the orchestration layer and the
     * local "accepted" state stands on its own (the user already
     * trusted the peer).
     */
    public suspend fun sendContactAck(
        request: IncomingContactRequest,
    ): PublishedAck {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(
                IllegalStateException(
                    "no identity secrets — please re-enter your password",
                ),
            )

        return useClientWith(secrets) { client, sender ->
            client.sendContactAck(
                accepterSigningSeed = sender.signingSeed,
                accepterAccountId = secrets.accountId,
                accepterX25519Pubkey = sender.x25519Pubkey,
                originalRequest = request,
                // ADR-0060 §6 — the requester's reply key opens their mailbox.
                reach = reach()?.reachAuthFor(request.fromPubkey, me.lakko.app.reach.WritePurpose.REPLY),
            )
        }
    }

    /**
     * Compute the Signal-style safety number (Phase 1.9b3 —
     * ADR-0005 §13.5) between the local identity and the
     * supplied contact pubkey. Returns the canonical 71-byte
     * ASCII string the user reads OOB to verify their pinned
     * keys agree.
     *
     * Throws [ContactsError] when the session has no identity
     * secrets (the caller should prompt the user to re-enter
     * their password) or when the UniFFI surface rejects the
     * pubkey shape.
     */
    public fun safetyNumber(theirPubkey: ByteArray): String {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(
                IllegalStateException(
                    "no identity secrets — please re-enter your password",
                ),
            )
        return Identity.fromSeed(secrets.accountIdentitySeed).use { identity ->
            uniffi.lakko_ffi_uniffi.computeSafetyNumber(
                myPubkey = identity.signaturePublicKey(),
                theirPubkey = theirPubkey,
            )
        }
    }

    /**
     * Remove an accepted contact from this device's contact list
     * (docs/ux/client-redesign.md section 5.3, "Remove"). Local only, the
     * mirror of [declineContactRequest] for the accepted list: the peer is
     * not told, no message is sent, and an existing 1x1 chat is left as it
     * is (leaving a conversation is the chat's own action). The persisted
     * list and the name directory's renames follow.
     */
    public fun removeContact(messageId: Long) {
        bindAccountIfSignedIn()
        var changed = false
        var removedKey: ByteArray? = null
        mutableState.update { state ->
            val remaining = state.accepted.filterNot { it.request.messageId == messageId }
            changed = remaining.size != state.accepted.size
            removedKey = state.accepted.firstOrNull { it.request.messageId == messageId }?.request?.fromPubkey
            if (changed) state.copy(accepted = remaining) else state
        }
        if (changed) persistAccepted()
        // ADR-0060 §1 — the removed person held this account's `contact` key
        // (and, through their contacts, its `fof` key): both rotate, the old
        // ones come off the mailbox at once, and the remaining one-to-ones get
        // the new contact grant in a republished profile. Their grants for
        // this device are dropped too.
        val manager = reach() ?: return
        if (!changed) return
        reachScope.launch {
            try {
                removedKey?.let { manager.forgetHeld(it) }
                manager.rotate(me.lakko.app.reach.ReachKind.CONTACT)
                manager.rotate(me.lakko.app.reach.ReachKind.FOF)
                chatRepo?.publishOwnProfileToAllGroups()
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (cause: Throwable) {
                android.util.Log.w("ContactsRepository", "ADR-0060 contact-key rotation failed: ${cause.javaClass.simpleName}")
            }
        }
    }

    /** ADR-0060 — background work that must outlive the calling screen. */
    private val reachScope = kotlinx.coroutines.CoroutineScope(
        kotlinx.coroutines.SupervisorJob() + Dispatchers.IO,
    )

    /** Drop an incoming request from the pending list without
     *  acknowledging it to the sender. Mirror of the web
     *  Decline semantics — server doesn't get told, the row
     *  just disappears locally. */
    public fun declineContactRequest(messageId: Long) {
        bindAccountIfSignedIn()
        mutableState.update { state ->
            state.copy(pending = state.pending.filterNot { it.messageId == messageId })
        }
        // Persisted so the decline holds across a relaunch: the cursor is
        // already past the row, and the row is no longer in the saved list.
        persistInboxPending()
    }

    /**
     * Joiner-side: open + verify an invite-link slug, fetch the
     * capability bytes from the discovery directory, consume
     * (atomically bumps the server's used-counter + records
     * this joiner in the dedup side table), and drive the
     * full Accept flow — `sendContactRequestToBundle` →
     * `ChatRepository.startGroupWith`. Surfaces every distinct
     * step as an [AcceptInviteResult] variant so the
     * `InviteLandingScreen` (1.15e11b) can render the right
     * outcome copy.
     *
     * Steps:
     *   1. base64url-decode the 8-character slug to recover the
     *      6-byte `blob_id`.
     *   2. `DiscoveryClient.fetchInvite(blobId)` → capability
     *      wire bytes (404 → `NotFound`).
     *   3. `InviteCapability.decodeAndVerify(wire)` — local
     *      Ed25519 signature check (a malicious server cannot
     *      serve substituted bytes; the joiner re-verifies).
     *   4. `isValidNow(now)` — expiry check. Expired → return
     *      `Expired` without touching the consume counter.
     *   5. `DiscoveryClient.consumeInvite(blobId, capabilityBytes,
     *      joinerAccountId)` — atomic counter bump + dedup tag.
     *      `Exhausted` / `AlreadyConsumed` short-circuit out
     *      here without further side effects.
     *   6. `ContactsClient.sendContactRequestToBundle(...)` —
     *      ship a ContactRequest to the inviter's stranger
     *      inbox (the bundle is already in hand from the
     *      capability; no discovery lookup needed).
     *   7. If a [ChatRepository] is wired,
     *      `ChatRepository.startGroupWith(...)` — fetch the
     *      inviter's KeyPackage + create the 1×1 MLS group +
     *      ship the Welcome.
     *
     * The pending request list is **not** touched on this
     * path — invites bypass the stranger-inbox + Accept dance
     * by design (the cryptographic provenance is already
     * established by the signature on the capability). The
     * accepted-contacts list gets a fresh row with the
     * resulting `groupIdHex` so the joiner can immediately
     * Open Chat.
     */
    public suspend fun acceptInviteLink(slug: String): AcceptInviteResult {
        val secrets = authRepo.identitySecrets.value
            ?: return AcceptInviteResult.RequiresReauth
        // Sealed links (2026-09-28): the argument is `<slug>#<secret>`; a
        // missing or malformed secret is refused before any request.
        val link = me.lakko.app.discovery.InviteLinkArgument.decode(slug.trim())
        val secret = me.lakko.app.discovery.inviteLinkSecretOf(link.fragment)
            ?: return AcceptInviteResult.Damaged
        // ADR-0028 amendment of 2026-09-27: a pointer slug is the 8-character
        // ledger id (6 bytes); anything longer is an inline sealed link (the
        // inviter's upload failed in transport). The pre-amendment
        // 43-character id is neither.
        val blobId = me.lakko.app.discovery.decodeInviteBlobId(link.slug)
        val sealed: ByteArray = if (blobId != null) {
            try {
                authenticatedDiscoveryClient().use { discovery ->
                    discovery.fetchInvite(blobId)
                }
            } catch (e: Throwable) {
                if (me.lakko.app.discovery.isDiscoveryRateLimited(e)) return AcceptInviteResult.RateLimited
                return AcceptInviteResult.Failed(message = e.message ?: "fetch failed")
            } ?: return AcceptInviteResult.NotFound
        } else {
            inlineSealedInviteOrNull(link.slug) ?: return AcceptInviteResult.MalformedSlug
        }

        val cap = try {
            me.lakko.app.discovery.openSealedInviteLink(sealed, secret) { bytes, key ->
                InviteCapability.openLink(bytes, key)
            }
        } catch (_: me.lakko.app.discovery.InviteLinkDamaged) {
            // A wrong secret, a tampered link and a blob a server swapped in
            // under a live id all fail the same way: the damaged state.
            return AcceptInviteResult.Damaged
        }
        // The consume dedup tag is computed over the PLAINTEXT capability.
        val capabilityBytes = cap.wire()
        return cap.use { capability ->
            val nowMs = System.currentTimeMillis().toULong()
            if (!capability.isValidNow(nowMs)) {
                return@use AcceptInviteResult.Expired
            }
            // ADR-0060 §6 — the link grant lets this device write the request
            // (and the 1:1 Welcome) into the inviter's closed mailbox. Used
            // for this flow only and never kept.
            val linkReach = try {
                me.lakko.app.ds.DsReachAuth(me.lakko.app.ds.reachSigner(capability.linkGrant().seed))
            } catch (_: Throwable) {
                null
            }

            // Snapshot the bundle fields we need post-consume.
            val inviterAccountId = capability.ownerAccountId()
            val inviterSigPubkey = capability.ownerSignaturePubkey()
            val inviterX25519Pubkey = capability.ownerX25519Pubkey()

            // Consume — atomic counter bump + per-joiner dedup. An inline link
            // has no ledger row, so nothing to consume.
            val consumeResult = if (blobId == null) {
                me.lakko.app.discovery.ConsumeInviteResult.Consumed(usedCount = 0, maxUses = 0)
            } else try {
                authenticatedDiscoveryClient().use { discovery ->
                    discovery.consumeInvite(blobId, capabilityBytes, secrets.accountId)
                }
            } catch (e: Throwable) {
                if (me.lakko.app.discovery.isDiscoveryRateLimited(e)) {
                    return@use AcceptInviteResult.RateLimited
                }
                // ADR-0054 - the consume spends an `invite-consume` token.
                if (me.lakko.app.anon.isAnonTokenQuotaExhausted(e)) {
                    return@use AcceptInviteResult.AllowanceUsed
                }
                return@use AcceptInviteResult.Failed(
                    message = me.lakko.app.anon.anonTokenFailureCopy(e, "")
                        ?: e.message ?: "consume failed",
                )
            }
            when (consumeResult) {
                is me.lakko.app.discovery.ConsumeInviteResult.Exhausted ->
                    return@use AcceptInviteResult.Exhausted
                is me.lakko.app.discovery.ConsumeInviteResult.NotFound ->
                    return@use AcceptInviteResult.NotFound
                is me.lakko.app.discovery.ConsumeInviteResult.AlreadyConsumed ->
                    return@use AcceptInviteResult.AlreadyAccepted
                is me.lakko.app.discovery.ConsumeInviteResult.Consumed -> {
                    // Drop the consumeResult counters — the UI
                    // doesn't need them for the V1 flow.
                }
            }

            // Ship a ContactRequest to the inviter's stranger
            // inbox. The bundle is inline in the capability —
            // no discovery round-trip needed.
            val sendResult = try {
                useClientWith(secrets) { client, sender ->
                    client.sendContactRequestToBundle(
                        senderSigningSeed = sender.signingSeed,
                        senderAccountId = secrets.accountId,
                        senderSignaturePubkey = sender.signaturePubkey,
                        recipientAccountId = inviterAccountId,
                        recipientSignaturePubkey = inviterSigPubkey,
                        recipientX25519Pubkey = inviterX25519Pubkey,
                        introMessage = ByteArray(0),
                        // ADR-0048 §2 — who is asking, sealed to the inviter.
                        senderProfile = ownProfileForWire(secrets.accountId),
                        // ADR-0060 §6 — this device's own reply key.
                        replyGrant = reach()?.mintReply(inviterSigPubkey),
                        reach = linkReach,
                    )
                }
            } catch (e: Throwable) {
                return@use AcceptInviteResult.Failed(
                    message = e.message ?: "ContactRequest send failed",
                )
            }

            // Drive the chat-side startGroupWith if wired.
            val groupIdHex = if (chatRepo != null) {
                try {
                    chatRepo.startGroupWith(
                        recipientAccountId = inviterAccountId,
                        recipientSignaturePubkey = inviterSigPubkey,
                        recipientReach = linkReach,
                    ).also { hex ->
                        // docs/ux/client-redesign.md section 3 — an accepted invite
                        // creates a 1x1, which lists under Chats.
                        ConversationKindStore.set(chatRepo.context, hex, ConversationKind.Chat)
                    }
                } catch (_: Throwable) {
                    null
                }
            } else {
                null
            }

            // Append the new contact to the accepted list so
            // the user sees the result + can immediately Open
            // Chat. Construct a synthetic IncomingContactRequest
            // shell — the invite flow bypassed the stranger
            // inbox, but the AcceptedContact UI shape expects
            // one. The synthetic request's `messageId` is the
            // DS receipt id from the outbound ContactRequest;
            // `intro` is null.
            val syntheticRequest = IncomingContactRequest(
                messageId = sendResult.messageId,
                fromPubkey = inviterSigPubkey,
                fromAccountId = inviterAccountId,
                fromPseudonym = ByteArray(32),
                validAfterMs = nowMs,
                intro = null,
                createdAt = sendResult.createdAt,
            )
            bindAccount(secrets.accountId)
            val accepted = AcceptedContact(request = syntheticRequest, groupIdHex = groupIdHex)
            mutableState.update { state ->
                state.copy(
                    accepted = state.accepted + accepted,
                    // The inviter is a contact now; a request of theirs that
                    // was still pending leaves "Requests" (one row per person).
                    pending = withoutAcceptedSenders(state.pending, listOf(accepted)),
                )
            }
            persistAccepted()
            persistInboxPending()
            AcceptInviteResult.Accepted(
                recipientAccountId = inviterAccountId,
                inviterFingerprint = capability.ownerFingerprint(),
                groupIdHex = groupIdHex,
            )
        }
    }

    /**
     * Generate a fresh capability-based invite link.
     *
     * Builds the canonical [`InviteCapability`](uniffi.lakko_ffi_uniffi.InviteCapability)
     * wire bytes from the current session's [IdentitySecrets] +
     * uploads them to the discovery directory; receives a
     * server-assigned 6-byte `blob_id` back. Returns the
     * `https://lakko.me/i/<blob_id_b64url>` pointer URL the
     * owner shares via copy-to-clipboard or the Android share
     * sheet.
     *
     * 1.15e10 ships with **fixed defaults**: 7-day expiry,
     * single-use (`max_uses = 0`), non-bootstrap-referral
     * (this flow is for connecting existing users, not new
     * signups). A custom-config dialog (user-typed label /
     * expiry / max_uses) layers on as 1.15e10b when the UX
     * pushes back.
     *
     * The new invite is appended to [ContactsState.outstandingInvites]
     * so the UI can render a list of the owner's outstanding
     * links + offer per-row Copy / Share / Revoke affordances.
     *
     * @throws ContactsError on identity-secrets missing, on FFI
     *         compose failure (extremely unlikely for valid
     *         inputs), or on discovery upload failure.
     */
    public suspend fun generateInviteLink(
        label: String = "",
        expiryMillisFromNow: Long = DEFAULT_INVITE_EXPIRY_MS,
        maxUses: UInt = SINGLE_USE,
        bootstrapReferral: Boolean = false,
    ): GeneratedInvite {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(
                IllegalStateException(
                    "no identity secrets — please re-enter your password",
                ),
            )
        val expiryMs = (System.currentTimeMillis() + expiryMillisFromNow).toULong()

        return Identity.fromSeed(secrets.accountIdentitySeed).use { identity ->
            // ADR-0060 §6 — the link carries a `link` grant scoped to this
            // link, registered on the mailbox BEFORE the link is shared, so
            // revoking the link removes exactly its key.
            val manager = reach() ?: throw ContactsError.wrap(IllegalStateException("no session"))
            val linkId = uniffi.lakko_ffi_uniffi.generateLinkId()
            val linkGrant = try {
                manager.mintLink(linkId, expiryMs.toLong().takeIf { it != 0L })
            } catch (e: Throwable) {
                throw ContactsError.wrap(e)
            }
            val mint = try {
                composeInviteCapability(
                    ownerSigningSeed = identity.ed25519Seed(),
                    ownerAccountId = secrets.accountId,
                    ownerX25519Pubkey = identity.x25519PublicKey(),
                    expiryMs = expiryMs,
                    maxUses = maxUses,
                    bootstrapReferral = bootstrapReferral,
                    label = label,
                    linkId = linkId,
                    linkGrant = linkGrant,
                )
            } catch (e: Throwable) {
                throw ContactsError.wrap(e)
            }
            // Sealed links (2026-09-28): the ledger stores only the sealed
            // capability; the secret rides in the fragment. Only a transport
            // failure falls back to the inline form; a refusal is thrown.
            val upload = me.lakko.app.discovery.SealedInviteUpload(
                sealed = mint.sealed,
                issuerPubkey = mint.issuerPubkey,
                expiryMs = expiryMs.toLong(),
                maxUses = maxUses.toLong(),
                label = label,
                bootstrapReferral = bootstrapReferral,
            )
            val blobId: ByteArray? = try {
                authenticatedDiscoveryClient().use { discovery ->
                    discovery.uploadInvite(upload)
                }
            } catch (e: Throwable) {
                if (e is kotlinx.coroutines.CancellationException) {
                    kotlinx.coroutines.withContext(kotlinx.coroutines.NonCancellable) {
                        manager.retireLink(linkId)
                    }
                    throw e
                }
                if (!me.lakko.app.discovery.isInviteUploadTransportFailure(e)) {
                    // ADR-0060 §6 — a refused upload shares no link, so its
                    // key comes off the mailbox again.
                    manager.retireLink(linkId)
                    throw ContactsError.wrap(e)
                }
                // An inline link keeps its key: it is shared, and it is retired
                // at expiry like any other.
                android.util.Log.w("ContactsRepository", "invite upload unreachable; sharing the inline link")
                null
            }
            if (blobId != null) manager.noteLinkBlob(linkId, blobId)

            val slug = base64UrlEncodeNoPad(blobId ?: mint.sealed)
            val url = me.lakko.app.discovery.composeSealedInviteUrl(
                origin = me.lakko.app.config.ApiEndpoints.webOrigin,
                prefix = "/i/",
                blobId = blobId,
                sealed = mint.sealed,
                linkSecret = mint.linkSecret,
            )
            // UX 5.3 — keep the secret of a pointer link (the only kind the
            // server lists) so this device can copy and share it later, after
            // a restart included. An inline link has no ledger row.
            if (blobId != null) {
                inviteSecretStore.put(
                    base64Encode(secrets.accountId),
                    slug,
                    base64UrlEncodeNoPad(mint.linkSecret),
                )
            }
            val invite = GeneratedInvite(
                blobId = blobId,
                blobIdSlug = slug,
                url = url,
                label = label,
                expiryMs = expiryMs.toLong(),
                maxUses = maxUses.toLong(),
                createdAtMs = System.currentTimeMillis(),
            )
            mutableState.update { state ->
                state.copy(outstandingInvites = state.outstandingInvites + invite)
            }
            invite
        }
    }


    /**
     * Revoke an outstanding invite link via the discovery
     * service's owner-signed `DELETE` endpoint + drop the row
     * from [ContactsState.outstandingInvites].
     *
     * Returns the underlying [me.lakko.app.discovery.RevokeInviteResult]
     * so the UI can branch on `Revoked` vs `NotFoundOrNotOwner`.
     * The row is dropped from local state on `Revoked` **only**;
     * `NotFoundOrNotOwner` keeps it so the user can retry (the
     * 404-folding-of-bad-signature path may be transient).
     */
    public suspend fun revokeInviteLink(
        blobId: ByteArray,
    ): me.lakko.app.discovery.RevokeInviteResult {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(
                IllegalStateException(
                    "no identity secrets — please re-enter your password",
                ),
            )
        return Identity.fromSeed(secrets.accountIdentitySeed).use { identity ->
            val signingSeed = identity.ed25519Seed()
            try {
                val result = authenticatedDiscoveryClient().use { discovery ->
                    discovery.revokeInvite(blobId, signingSeed)
                }
                if (result is me.lakko.app.discovery.RevokeInviteResult.Revoked) {
                    // ADR-0060 §6 — the link's key comes off the mailbox.
                    reach()?.retireLinkByBlob(blobId)
                    // UX 5.3 — a revoked link's secret leaves with it.
                    inviteSecretStore.remove(base64Encode(secrets.accountId), base64UrlEncodeNoPad(blobId))
                    val blobIdHex = blobId.toList()
                    mutableState.update { state ->
                        state.copy(
                            outstandingInvites = state.outstandingInvites
                                .filterNot { it.blobId?.toList() == blobIdHex },
                        )
                    }
                }
                result
            } catch (e: Throwable) {
                throw ContactsError.wrap(e)
            } finally {
                signingSeed.fill(0)
            }
        }
    }

    /**
     * Owner-signed list of every live invite this account owns
     * across all devices (Phase 1.18d2b-list). Distinct from
     * [ContactsState.outstandingInvites] which only tracks
     * invites generated in the current session — the server-side
     * list is authoritative across the user's full device fleet.
     *
     * Throws [ContactsError] when the session has no identity
     * secrets (caller should prompt the user to re-enter their
     * password) or when the discovery client surfaces a transport
     * failure.
     */
    public suspend fun listActiveInvites(): me.lakko.app.discovery.ListInvitesResult {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(
                IllegalStateException(
                    "no identity secrets — please re-enter your password",
                ),
            )
        return Identity.fromSeed(secrets.accountIdentitySeed).use { identity ->
            val signingSeed = identity.ed25519Seed()
            try {
                authenticatedDiscoveryClient().use { discovery ->
                    discovery.listInvitesByOwner(secrets.accountId, signingSeed)
                }
            } catch (e: Throwable) {
                throw ContactsError.wrap(e)
            } finally {
                signingSeed.fill(0)
            }
        }
    }

    /**
     * UX 5.3 — the full shareable URL of each listed `/i/` link whose secret
     * this device kept when it created it, keyed by slug. A slug missing from
     * the result is a link created on another device: it can be revoked here,
     * not copied or shared.
     */
    public fun heldInviteLinkUrls(slugs: Collection<String>): Map<String, String> {
        val secrets = authRepo.identitySecrets.value ?: return emptyMap()
        val held = inviteSecretStore.all(base64Encode(secrets.accountId))
        if (held.isEmpty()) return emptyMap()
        val origin = me.lakko.app.config.ApiEndpoints.webOrigin
        return slugs.mapNotNull { slug ->
            me.lakko.app.discovery.heldInviteLinkUrl(origin, "/i/", slug, held[slug])?.let { slug to it }
        }.toMap()
    }

    /** Clear the in-memory state and unbind the account. Called when the
     *  session ends (log out and forget device alike). Nothing persisted is
     *  written or deleted here: the accepted list and the stranger-inbox
     *  cursor + pending list stay for the next login of the SAME account
     *  (a request declined before logging out stays declined) — only
     *  forget device deletes them ([me.lakko.app.account.AccountLocalWipe]).
     *  The sent log does not persist. */
    public fun clear() {
        synchronized(bindLock) {
            boundAccountB64 = null
            // Wipe any decrypted intro plaintext on the way out.
            // Best-effort: the `String` contents may still live in
            // heap if some collector cached them, but our reference
            // drops here.
            mutableState.value = ContactsState()
        }
        // The name directory stops carrying this account's renames, and the
        // names it learned stop resolving.
        pushRenamesToDirectory()
        profileDirectory?.onAccountChanged()
    }

    /**
     * Build a transient [Identity] from the stashed export key,
     * pass it + the live [ContactsClient] into [block], close
     * both on the way out.
     *
     * The Identity build is sub-millisecond (HKDF + Ed25519 +
     * X25519); a fresh handle per operation keeps the lifetime
     * story simple — no reference threading, no global state.
     * MLS state mutations would lose the throwaway provider, but
     * 1.15e6's operations are all stateless (sign + decrypt
     * only); the chat surface in 1.15f wires the canonical
     * Identity through `ChatProvider`.
     */
    private inline fun <T> useClientWith(
        secrets: IdentitySecrets,
        block: (ContactsClient, SenderMaterial) -> T,
    ): T {
        return clientFactory().use { client ->
            // ADR-0017: contact-request sender material is the canonical
            // AccountIdentity — seeded from the phrase seed, not exportKey.
            Identity.fromSeed(secrets.accountIdentitySeed).use { identity ->
                val sender = SenderMaterial(
                    signingSeed = identity.ed25519Seed(),
                    signaturePubkey = identity.signaturePublicKey(),
                    x25519Secret = identity.x25519SecretKey(),
                    x25519Pubkey = identity.x25519PublicKey(),
                    mailboxSigner = me.lakko.app.ds.FfiDsJournalSigner(
                        uniffi.lakko_ffi_uniffi.dsMailboxSigner(identity),
                    ),
                )
                try {
                    block(client, sender)
                } finally {
                    sender.signingSeed.fill(0)
                    sender.x25519Secret.fill(0)
                }
            }
        }
    }

    /** Carrier for the bytes [ContactsClient] needs from the
     *  current session's Identity. The byte arrays live on the
     *  heap for the duration of one repository call; the wipe in
     *  [useClientWith]'s finally narrows the window. The X25519
     *  public key (1.9b1 — carried into the ContactAck so the
     *  original sender can seal future replies without a fresh
     *  discovery round-trip) is public material so it is not
     *  zeroized. */
    private data class SenderMaterial(
        val signingSeed: ByteArray,
        val signaturePubkey: ByteArray,
        val x25519Secret: ByteArray,
        val x25519Pubkey: ByteArray,
        /** ADR-0053 / ADR-0060 — the identity proof of this account's mailbox. */
        val mailboxSigner: me.lakko.app.ds.DsJournalSigner,
    )
}

// =============================================================================
// State shape
// =============================================================================

/** Inline inbox error shown when a Welcome from a peer could not be joined. */
private const val TAG = "ContactsRepository"

/** Shown in place of a transport error when the inbox poll fails. */
internal const val INBOX_FETCH_FAILURE_MESSAGE: String =
    "Couldn't check for new contact requests. Trying again."

internal const val WELCOME_JOIN_FAILED_MESSAGE: String =
    "Someone started a chat with you, but it could not be opened on this " +
        "device. Ask them to add you again."

/** Snapshot of every field the [me.lakko.app.ui.contacts.ContactsScreen]
 *  renders. */
public data class ContactsState(
    /** Verified incoming requests waiting for Accept / Decline. */
    val pending: List<IncomingContactRequest> = emptyList(),
    /** Locally-accepted requests — in 1.15e6 these are inert
     *  (no MLS group has been created). 1.15f turns them into
     *  real chat sessions. */
    val accepted: List<AcceptedContact> = emptyList(),
    /** Outgoing requests we've sent in this session. Survives
     *  process death — there's no on-server "sent" mirror to
     *  rehydrate from, so a fresh process starts with an empty
     *  list. */
    val sent: List<SentContactRequest> = emptyList(),
    /** Owner-side invite links we've generated this session.
     *  Like [sent], not persisted across process death — the
     *  authoritative state lives on the discovery server, and
     *  rehydrating this list would require a per-account
     *  /discovery/invite/by-owner endpoint that doesn't exist
     *  in Phase 1 (deliberate: surfaces the "share the URL right
     *  now" UX, not a "manage outstanding invites" admin panel).
     *  Cleared on logout / forgetDevice. */
    val outstandingInvites: List<GeneratedInvite> = emptyList(),
    /** Cursor for the stranger-inbox poll. `0` = catch-up
     *  fetch on first refresh. */
    val cursor: Long = 0,
    /** Inbox refresh in progress — UI shows a spinner. */
    val refreshing: Boolean = false,
    /** Set when [AuthRepository.identitySecrets] is `null` —
     *  the screen surfaces a "re-enter your password" panel
     *  instead of the normal contacts list. */
    val requiresReauth: Boolean = false,
    /** Last error from a refresh / send attempt, for inline
     *  display. `null` clears on the next successful call. */
    val lastError: String? = null,
)

/**
 * Discriminated outcome of [ContactsRepository.acceptInviteLink].
 *
 * Each variant maps to a distinct user-visible state on the
 * [InviteLandingScreen] (1.15e11b):
 *
 *   * [Accepted] — every step succeeded; the inviter has a
 *     fresh ContactRequest in their stranger inbox, the
 *     MLS group is up (when `groupIdHex` is non-null —
 *     `ChatRepository` was wired + `startGroupWith` worked).
 *   * [AlreadyAccepted] — the server's per-joiner dedup tag
 *     already had a row for `(blob_id, joiner_account_id)`.
 *     The user landed on this URL before; the UI surfaces a
 *     "you've already accepted this invite" hint + an "Open
 *     chat" affordance pointing at the prior thread.
 *   * [Expired] — the capability's `expiry_ms` is in the past
 *     (local clock check); no server round-trip happened, no
 *     state mutated.
 *   * [Exhausted] — the server's `max_uses` ceiling was hit
 *     by other joiners before our consume could land
 *     (410 Gone).
 *   * [NotFound] — server returned 404 (revoked / expired /
 *     never-issued; the server folds these into one status to
 *     deny status-code probing).
 *   * [MalformedSlug] — the URL slug is not the 8-character
 *     base64url form of a 6-byte blob_id.
 *   * [RateLimited] — the discovery service answered `429` on
 *     fetch or consume. The link may be fine; try again later.
 *   * [RequiresReauth] — [AuthRepository.identitySecrets] is
 *     `null`. The UI surfaces a "please re-enter your
 *     password" panel + a Login deep-link.
 *   * [Failed] — any other underlying failure (transport,
 *     OPAQUE / OPRF wire-format mismatch, no consume token,
 *     MLS group creation throwing). Carries the inner error
 *     message for diagnostics.
 */
public sealed interface AcceptInviteResult {
    /** The link's fragment is missing or malformed, or the link does not
     *  open under it (sealed links, 2026-09-28). */
    public data object Damaged : AcceptInviteResult

    public data class Accepted(
        val recipientAccountId: ByteArray,
        /** 71-char Signal-style safety number derived from
         *  the inviter's Ed25519 pubkey. UI surfaces this on
         *  the success screen so the user can verify out-of-
         *  band whom they just connected to — defence-in-
         *  depth against link-substitution attacks
         *  (ROADMAP 3c3 / calls.md C-28). */
        val inviterFingerprint: String,
        /** Non-null when the chat surface was wired AND the
         *  MLS handshake succeeded; the UI surfaces an "Open
         *  chat" button pointing at the new thread. */
        val groupIdHex: String?,
    ) : AcceptInviteResult {
        override fun equals(other: Any?): Boolean =
            other is Accepted &&
                recipientAccountId.contentEquals(other.recipientAccountId) &&
                inviterFingerprint == other.inviterFingerprint &&
                groupIdHex == other.groupIdHex
        override fun hashCode(): Int {
            var h = recipientAccountId.contentHashCode()
            h = h * 31 + inviterFingerprint.hashCode()
            h = h * 31 + (groupIdHex?.hashCode() ?: 0)
            return h
        }
    }

    public data object AlreadyAccepted : AcceptInviteResult
    public data object Expired : AcceptInviteResult
    public data object Exhausted : AcceptInviteResult
    public data object NotFound : AcceptInviteResult
    public data object MalformedSlug : AcceptInviteResult
    public data object RateLimited : AcceptInviteResult

    /** ADR-0054 - today's allowance of invite-consume tokens is used up. */
    public data object AllowanceUsed : AcceptInviteResult
    public data object RequiresReauth : AcceptInviteResult

    public data class Failed(val message: String) : AcceptInviteResult
}

/** An invite link the owner has just generated this session. */
public data class GeneratedInvite(
    /** Raw 6-byte server-assigned ledger id (the same value
     *  `revokeInviteLink` takes), or `null` for an inline link (the upload
     *  failed in transport; there is no ledger row to revoke). */
    val blobId: ByteArray?,
    /** The slug portion of [url]: base64url of [blobId], or of the sealed
     *  bytes for an inline link. */
    val blobIdSlug: String,
    /** `https://lakko.me/i/<slug>#<secret>` — the URL the owner shares. */
    val url: String,
    /** Human-visible label (empty when the user didn't set one). */
    val label: String,
    /** UNIX millis when this invite stops being honoured. `0` is
     *  the never-expires sentinel. */
    val expiryMs: Long,
    /** Server-enforced consume ceiling. `0` = single-use,
     *  `0xFFFFFFFF` = unlimited. */
    val maxUses: Long,
    /** UNIX millis when the row was generated (local clock). */
    val createdAtMs: Long,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is GeneratedInvite) return false
        return blobId.contentEquals(other.blobId) &&
            blobIdSlug == other.blobIdSlug &&
            url == other.url &&
            label == other.label &&
            expiryMs == other.expiryMs &&
            maxUses == other.maxUses &&
            createdAtMs == other.createdAtMs
    }

    override fun hashCode(): Int {
        var result = blobId.contentHashCode()
        result = 31 * result + blobIdSlug.hashCode()
        result = 31 * result + url.hashCode()
        result = 31 * result + label.hashCode()
        result = 31 * result + expiryMs.hashCode()
        result = 31 * result + maxUses.hashCode()
        result = 31 * result + createdAtMs.hashCode()
        return result
    }
}

/** Default invite expiry — 7 days from generation. Mirrors the
 *  web side's `7 * 86_400_000` constant. */
private const val DEFAULT_INVITE_EXPIRY_MS: Long = 7L * 24 * 60 * 60 * 1000

/** The sealed bytes of an inline `/i/` slug (base64url of the sealed link),
 *  or `null` when it does not decode or is too short to be one. */
internal fun inlineSealedInviteOrNull(slug: String): ByteArray? {
    val bytes = try {
        me.lakko.app.auth.base64UrlDecodeNoPad(slug)
    } catch (_: IllegalArgumentException) {
        return null
    }
    return bytes.takeIf { it.size >= INLINE_SEALED_INVITE_MIN_LEN }
}

/** Floor on an inline sealed `/i/` slug: far above the 6-byte pointer and
 *  below any real sealed contact capability (a loose discriminator, like the
 *  `/g/` and `/c/` inline floors). */
private const val INLINE_SEALED_INVITE_MIN_LEN: Int = 100

/** Server-side sentinel for `max_uses = single-use`. */
private val SINGLE_USE: UInt = 0u

/** URL prefix Lakko shares its invite links under: the web origin of the
 *  stack this build talks to plus the web `composePointerInviteUrl` path,
 *  so a link generated on either side opens on the other. */
internal val INVITE_URL_PREFIX: String
    get() = me.lakko.app.config.ApiEndpoints.webOrigin + "/i/"

/** A request the user has accepted locally.
 *
 *  [groupIdHex] is populated when the Accept flow has just
 *  driven `ChatRepository.startGroupWith` for this peer
 *  (1.15f4c bridge). When non-null, the contacts UI surfaces
 *  an "Open chat" affordance that navigates the user to the
 *  per-thread chat screen for the new group. `null` indicates
 *  either a pre-1.15f4c accept that was never wired to chat,
 *  or an Accept that ran while [ChatRepository] wasn't
 *  bootstrapped — the UI surfaces a degraded "chat creation
 *  failed" hint in that case. */
public data class AcceptedContact(
    val request: IncomingContactRequest,
    val groupIdHex: String? = null,
    /** The viewer's own name for this contact (a local rename, ADR-0021
     *  follow-up; never sent anywhere). `null` until the user renames the
     *  contact. Every surface resolves the label through
     *  `me.lakko.app.profile.ProfileDirectory` (ADR-0048), which ranks this
     *  rename above the person's own profile name, their verified username and
     *  the fingerprint. Persisted optionally, so a contact stored before this
     *  field existed loads with `displayName == null`. */
    val displayName: String? = null,
)

/** Outgoing contact request, surfaced on the UI as an "I asked
 *  @<name> to connect" row. */
public data class SentContactRequest(
    val recipientUsername: String,
    val recipientAccountIdB64: String,
    val messageId: Long,
    val createdAt: String,
)
