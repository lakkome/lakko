// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.util.Log
import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.withContext
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.launch
import me.lakko.app.launchBound
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.auth.base64Encode
import me.lakko.app.profile.toFfi
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.chat.ChatRepository
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
            // ADR-0074 section 3.5 - a contact's proven rotation makes the
            // key pinned until now a proven predecessor of the new one.
            onProvenRotation = { peer, oldKey, newKey ->
                val ctx = appContext
                val owner = authRepo.identitySecrets.value?.let { base64Encode(it.accountId) }
                if (ctx != null && owner != null) {
                    ProvenPredecessors.recordProven(
                        store = SharedPrefsProvenLinkStore(ctx),
                        ownerAccountIdB64 = owner,
                        accountIdB64 = AcceptedContactsStore.canonicalAccountB64(peer),
                        oldKeyB64 = oldKey,
                        newKeyB64 = newKey,
                    )
                }
            },
            // ADR-0074 section 3.5a rule 1 - mailboxes are addressed by the
            // current key of the person (proven steps only).
            mailboxKeyOf = { key ->
                appContext?.let { ctx ->
                    try {
                        me.lakko.app.reach.ReachManager.get(ctx).mailboxAccountKey(key)
                    } catch (_: Throwable) {
                        null
                    }
                } ?: key
            },
            // ADR-0073 R.2 - the reset ledger and the verified bindings are
            // the signed-in account's, like the pins.
            ownerAccountIdB64 = authRepo.identitySecrets.value?.let { base64Encode(it.accountId) }.orEmpty(),
            resetLedger = appContext?.let { SharedPrefsResetLedgerStore(it) } ?: InMemoryResetLedgerStore(),
            verifiedBindings = appContext?.let { SharedPrefsVerifiedBindingStore(it) } ?: InMemoryVerifiedBindingStore(),
            provenPredecessorsOf = { _, pinnedKey ->
                val ctx = appContext
                val owner = authRepo.identitySecrets.value?.let { base64Encode(it.accountId) }
                if (ctx != null && owner != null) {
                    keysOfPerson(pinnedKey, SharedPrefsProvenLinkStore(ctx).load(owner)).filter { it != pinnedKey }
                } else {
                    emptyList()
                }
            },
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
    /** ADR-0078 section 10: the links this device minted, with their
     *  revocation seeds; `null` = the process-wide [OwnLinkStores.current]. */
    private val ownLinkStoreOverride: OwnLinkStore? = null,
    /** ADR-0079 section 4 - the ledger-row deletions this device owes (it
     *  revoked the link and the ledger has not answered yet). */
    private val owedLinkRowDeletionStore: OwedLinkRowDeletionStore =
        appContext?.let { SharedPrefsOwedLinkRowDeletionStore.of(it) } ?: InMemoryOwedLinkRowDeletionStore(),
    /** ADR-0073 R.2 - the accepted generation, the retired keys and the
     *  reset's `signed_at` per peer account (public values only). */
    private val resetLedgerStore: ResetLedgerStore =
        appContext?.let { SharedPrefsResetLedgerStore(it) } ?: InMemoryResetLedgerStore(),
    /** ADR-0073 R.2 - what this device verified for accounts it never
     *  pinned. */
    private val verifiedBindingStore: VerifiedBindingStore =
        appContext?.let { SharedPrefsVerifiedBindingStore(it) } ?: InMemoryVerifiedBindingStore(),
    /** ADR-0074 part 3 - which key's mailbox the kept inbox cursor reads,
     *  and the cursor of each later mailbox of the account. */
    private val inboxMailboxStore: InboxMailboxStore? =
        appContext?.let {
            InboxMailboxStore(me.lakko.app.auth.SharedPrefsMarkerStorage(it, InboxMailboxStore.PREFS_NAME))
        },
    /** ADR-0074 section 3.5 - the proven steps of contacts' key histories
     *  (public keys only). */
    private val provenLinkStore: ProvenLinkStore =
        appContext?.let { SharedPrefsProvenLinkStore(it) } ?: InMemoryProvenLinkStore(),
    /** The SAME persistent per-account pin store the fetch-path gate writes:
     *  read here for the key an account is pinned under now. Built per call
     *  because the owner account may change. */
    private val pinStoreFactory: () -> PinStore? = {
        appContext?.let { ctx ->
            authRepo.identitySecrets.value?.let { PeerPinStore(ctx, base64Encode(it.accountId)) }
        }
    },
) {
    /** ADR-0078 section 10 - where this device keeps its own links. */
    private val ownLinkStore: OwnLinkStore
        get() = ownLinkStoreOverride ?: OwnLinkStores.current

    /** ADR-0079 section 4 - deletes a revoked link's ledger row once. */
    private val linkRowDeleter: LinkRowDeleter = LinkRowDeleter(owedLinkRowDeletionStore)

    // -----------------------------------------------------------------
    // ADR-0073 "Phase 2 revised: silent continuity" - a contact reset their
    // account keys. Nothing here has a surface (R.0).
    // -----------------------------------------------------------------

    /** The ledger rows of the bound account. */
    private fun resetLedgerRows(): Map<String, ResetLedgerRow> =
        boundAccountB64?.let { resetLedgerStore.loadAll(it) }.orEmpty()

    /** The key [accountIdB64] is pinned under now, or `null`. */
    private fun pinnedKeyOf(accountIdB64: String): String? =
        try {
            pinStoreFactory()?.readPin(accountIdB64)?.pinnedSignaturePubkeyB64
        } catch (_: Throwable) {
            null
        }

    /** R.5 / R.6 - every key an accepted reset retired, mapped to the account
     *  id it belonged to (standard base64 on both sides). A row or a claim
     *  from a leaf bound to one of these is dropped. */
    public fun supersededKeys(): Map<String, String> = supersededKeysOf(resetLedgerRows())

    /** R.4 - the account and generation behind [currentKeyB64] when an
     *  ACCEPTED RESET re-pinned this device to it, or `null`. */
    public fun acceptedResetOfKey(currentKeyB64: String): AcceptedResetOfKey? =
        acceptedResetOfKey(currentKeyB64, resetLedgerRows(), ::pinnedKeyOf)

    /** The account id (standard base64) whose pin or retired keys include
     *  [keyB64], or `null`: how a person key is named by account. */
    public fun accountOfKnownKey(keyB64: String): String? {
        val rows = resetLedgerRows()
        supersededKeysOf(rows)[keyB64]?.let { return it }
        for ((accountIdB64, row) in rows) if (keyB64 in row.previousKeysB64) return accountIdB64
        mutableState.value.accepted.firstOrNull { base64Encode(it.request.fromPubkey) == keyB64 }
            ?.request?.fromAccountId?.let { return base64Encode(it) }
        return null
    }

    /**
     * R.7 case 1 / UX 16.6 - the contact this device has ESTABLISHED for an
     * account id ([establishedContactOfAccount]): a row names the account and
     * its own key is named by the pin (current or a proven predecessor) or by
     * a verified binding. A row completed only from a profile is not one.
     */
    public fun establishedContact(accountId: ByteArray): AcceptedContact? {
        val accountIdB64 = base64Encode(accountId)
        val owner = boundAccountB64 ?: return null
        val accepted = mutableState.value.accepted
        val pinned = pinnedKeyOf(accountIdB64)
        val index = establishedContactOfAccount(
            rows = accepted.map {
                ContactRowKeys(it.request.fromAccountId?.let(::base64Encode), base64Encode(it.request.fromPubkey))
            },
            accountIdB64 = accountIdB64,
            pinKeysB64 = pinned?.let { keysOfPerson(it, provenLinks()).let { chain -> chain.take(chain.indexOf(it) + 1) } }
                .orEmpty(),
            bindingKeyB64 = verifiedBindingStore.read(owner, accountIdB64)?.keyB64,
        ) ?: return null
        return accepted[index]
    }

    /**
     * R.2 "The re-pin", the contact half: the gate wrote the pin and the
     * ledger row; the contact ROW follows the person. The same row, its
     * rename, chat link and account id untouched, now holding the new key. A
     * second row already under the new key (a request from the new identity
     * accepted before the reset was noticed) folds into this one. No state is
     * left on the contact: nothing reads as "reset".
     */
    private fun onResetAccepted(event: ResetAccepted) {
        bindAccountIfSignedIn()
        val owner = boundAccountB64 ?: return
        if (event.ownerAccountIdB64.isNotEmpty() && event.ownerAccountIdB64 != owner) return
        val peer = AcceptedContactsStore.canonicalAccountB64(event.accountIdB64)
        val newKey = try {
            java.util.Base64.getDecoder().decode(event.newKeyB64)
        } catch (_: IllegalArgumentException) {
            null
        }
        if (newKey != null) {
            var changed = false
            mutableState.update { state ->
                val plan = rekeyContactRows(
                    rows = state.accepted.map { c ->
                        ContactRowKeys(
                            accountIdB64 = c.request.fromAccountId?.let(::base64Encode),
                            keyB64 = base64Encode(c.request.fromPubkey),
                        )
                    },
                    accountIdB64 = peer,
                    retiredKeysB64 = event.retiredKeysB64 + event.previousKeyB64,
                    newKeyB64 = event.newKeyB64,
                )
                changed = plan != null
                if (plan == null) {
                    state
                } else {
                    val row = state.accepted[plan.rowIndex]
                    val moved = row.copy(request = row.request.copy(fromPubkey = newKey))
                    state.copy(
                        accepted = state.accepted.mapIndexedNotNull { i, c ->
                            when {
                                i == plan.rowIndex -> moved
                                i in plan.foldedIndices -> null
                                else -> c
                            }
                        },
                    )
                }
            }
            if (changed) persistAccepted()
        }
        // ADR-0074 section 3.5 - a RESET ends every proven predecessor the
        // account had: none of its earlier keys is the person any more.
        ProvenPredecessors.dropAccount(provenLinkStore, owner, peer)
        pushAliasesToDirectory()
        Log.i(TAG, "ADR-0073: an attested key reset was accepted")
        try {
            chatRepo?.onResetAccepted(event)
        } catch (cause: Throwable) {
            Log.w(TAG, "ADR-0073: chat layer not told: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * R.2 - the identity check of an account, as one of the rule's triggers
     * (the scheduled per-chat device check, opening a one-to-one, a send
     * whose last check is old). [heldKeyB64] is the roster's account key of
     * a member this device never pinned ([ContactsClient.checkPeerIdentity]).
     * Never throws; nothing is shown for any outcome.
     */
    public suspend fun checkContactIdentity(accountId: ByteArray, heldKeyB64: String? = null): KnownRequestCheck {
        val secrets = authRepo.identitySecrets.value ?: return KnownRequestCheck.INCONCLUSIVE
        return try {
            val served = useClientWith(secrets) { client, _ -> client.checkPeerIdentity(accountId, heldKeyB64) }
            if (served == null) KnownRequestCheck.INCONCLUSIVE else KnownRequestCheck.CURRENT
        } catch (_: SupersessionRefused) {
            KnownRequestCheck.REFUSED
        } catch (_: IdentityCheckInconclusive) {
            KnownRequestCheck.INCONCLUSIVE
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            Log.w(TAG, "ADR-0073: contact identity check failed: ${cause.javaClass.simpleName}")
            KnownRequestCheck.INCONCLUSIVE
        }
    }

    /**
     * ADR-0074 section 3.5a rule 6 - a write into the mailbox of the person
     * known by [accountKey] was refused: fetch their identity bundle through
     * the fail-closed gate (a proven rotation re-pins there and records the
     * predecessor, which is what moves the address), then tell the reach
     * manager the address is known current. `true` when the bundle passed
     * the gate, so the caller re-resolves and retries once; `false` when the
     * account id is not known here, the gate refused, or the fetch failed
     * (the refusal then stands).
     *
     * [accountIdHint] is the account id the caller already holds for the
     * person; without one it is read off the contact row or a pending
     * request of that person.
     */
    public suspend fun recheckAddress(accountKey: ByteArray, accountIdHint: ByteArray? = null): Boolean {
        val secrets = authRepo.identitySecrets.value ?: return false
        val accountId = accountIdHint ?: accountIdOfPerson(accountKey) ?: return false
        if (accountId.contentEquals(secrets.accountId)) return false
        val served = try {
            useClientWith(secrets) { client, _ -> client.checkPeerIdentity(accountId) }
        } catch (_: SupersessionRefused) {
            // An unproven change moves nothing.
            return false
        } catch (_: IdentityCheckInconclusive) {
            return false
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            Log.w(TAG, "ADR-0074 3.5a: address re-check failed: ${cause.javaClass.simpleName}")
            return false
        } ?: return false
        // The bundle must be this person's: the key in hand, or one its
        // proven steps lead to. Anything else is another account's answer.
        if (!samePerson(base64Encode(accountKey), base64Encode(served), provenLinks())) return false
        reach()?.addressKnownCurrent(accountKey)
        return true
    }

    /** The account id held for the person behind [accountKey] (any key of
     *  their proven chain): the contact row's, else a pending request's. */
    private fun accountIdOfPerson(accountKey: ByteArray): ByteArray? {
        val keyB64 = base64Encode(accountKey)
        val links = provenLinks()
        val state = mutableState.value
        return (state.accepted.asSequence().map { it.request } + state.pending.asSequence())
            .firstOrNull { it.fromAccountId != null && samePerson(base64Encode(it.fromPubkey), keyB64, links) }
            ?.fromAccountId
    }

    /**
     * ADR-0074 section 3.5a rule 5 - on the device that rotated the account
     * keys, and on every kept device once it adopted them (over its own
     * links and requests): what was granted before the rotation is spent.
     *
     *  - the `/i/` and `/c/` links this account minted under a previous key
     *    are revoked at the ledger (the revoke is signed by the key that
     *    issued the link, so only links of a PREVIOUS key can be), and leave
     *    this device's lists;
     *  - every unanswered outgoing contact request is sent again from the
     *    new key, with a fresh reply grant for the new mailbox, and replaces
     *    the entry it re-sends.
     *
     * `true` only when nothing is left to do; `false` keeps the rotation's
     * step owed (the ledger or a recipient could not be reached), and
     * leaves the durable marker that retries the round at the next session
     * start. The outgoing requests are stored under the device key
     * ([SentRequestsStore]), so a process that died since the rotation still
     * re-sends them, intro included.
     */
    public suspend fun spendPreRotationGrants(): Boolean = spendLock.withLock {
        val done = spendPreRotationGrantsLocked()
        // The marker the key move left ([ReachManager.grantsSpendOwed]) is
        // what retries this at each session start, on the rotating device
        // and on a kept one alike; it goes once nothing is left to do.
        if (done) reach()?.grantsSpendDone()
        done
    }

    private suspend fun spendPreRotationGrantsLocked(): Boolean {
        val secrets = authRepo.identitySecrets.value ?: return false
        bindAccountIfSignedIn()
        val predecessors = try {
            authRepo.ownPredecessors()
        } catch (_: Throwable) {
            return false
        }
        if (predecessors.isEmpty()) return false
        val adoptedAtMs = predecessors.maxOf { it.supersededAtMs }
        val linksDone = try {
            withContext(Dispatchers.IO) { revokeSpentLinks(secrets) }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            Log.w(TAG, "ADR-0074 3.5a: spent links not revoked: ${cause.javaClass.simpleName}")
            false
        } finally {
            predecessors.forEach { it.zero() }
        }
        // The lists this device shows never offer a link of a previous key,
        // whether or not the ledger answered.
        mutableState.update { state ->
            state.copy(outstandingInvites = state.outstandingInvites.filterNot { it.createdAtMs < adoptedAtMs })
        }
        val requestsDone = withContext(Dispatchers.IO) { resendUnansweredRequests(secrets, adoptedAtMs) }
        return linksDone && requestsDone
    }

    /**
     * Revoke the ledger rows of the links that were live when this account's
     * keys rotated. ADR-0078 section 10: each with its own revocation seed
     * from this device's record; there is no owner list, so a link this
     * device has no record of is another device's to revoke (its reach key
     * goes anyway).
     */
    private suspend fun revokeSpentLinks(secrets: IdentitySecrets): Boolean {
        val manager = reach()
        val accountB64 = base64Encode(secrets.accountId)
        val recorded = manager?.spentLinkBlobsHex().orEmpty().mapNotNull { hex ->
            me.lakko.app.reach.reachUnhex(hex)?.let(::base64UrlEncodeNoPad)
        }
        authenticatedDiscoveryClient().use { discovery ->
            for (slug in spentLinkSlugs(recorded)) {
                val link = ownLinkStore.get(accountB64, OwnLink.KIND_CONTACT, slug)
                    ?: ownLinkStore.get(accountB64, OwnLink.KIND_CALL, slug)
                    ?: continue
                val blobId = link.blobId() ?: continue
                val seed = link.seed() ?: continue
                val result = try {
                    if (link.kind == OwnLink.KIND_CALL) {
                        discovery.revokeCallInvite(blobId, seed)
                    } else {
                        discovery.revokeInvite(blobId, seed)
                    }
                } catch (_: me.lakko.app.discovery.DiscoveryClientArgError) {
                    // Not a ledger id this client can name: nothing to revoke.
                    me.lakko.app.discovery.RevokeInviteResult.NotFoundOrNotOwner
                } finally {
                    seed.fill(0)
                }
                // Revoked, or already gone (used up, expired, revoked
                // elsewhere): either way the record and the row leave.
                ownLinkStore.remove(accountB64, link.kind, slug)
                inviteSecretStore.remove(accountB64, slug)
                if (result is me.lakko.app.discovery.RevokeInviteResult.Revoked) {
                    mutableState.update { state ->
                        state.copy(outstandingInvites = state.outstandingInvites.filterNot { it.blobIdSlug == slug })
                    }
                }
            }
        }
        manager?.spentLinksDone()
        return true
    }

    /** Re-send the unanswered outgoing requests sent before [adoptedAtMs]. */
    private suspend fun resendUnansweredRequests(secrets: IdentitySecrets, adoptedAtMs: Long): Boolean {
        val before = mutableState.value.sent
        val spent = before.filter { it.sentAtMs < adoptedAtMs }
        if (spent.isEmpty()) return true
        val outcome = me.lakko.app.reach.resendPendingRequests(spent) { entry -> resendRequest(secrets, entry) }
        mutableState.update { state ->
            // Entries added while the pass ran stay; the spent ones are
            // replaced by what the pass left of them.
            state.copy(sent = state.sent.filterNot { it in spent } + outcome.pending)
        }
        persistSent()
        return outcome.done
    }

    private suspend fun resendRequest(
        secrets: IdentitySecrets,
        entry: SentContactRequest,
    ): Pair<me.lakko.app.reach.RequestResend, SentContactRequest?> {
        val accountId = try {
            me.lakko.app.auth.base64Decode(entry.recipientAccountIdB64)
        } catch (_: IllegalArgumentException) {
            return me.lakko.app.reach.RequestResend.GONE to null
        }
        return try {
            val result = useClientWith(secrets) { client, sender ->
                client.sendContactRequestToAccount(
                    senderSigningSeed = sender.signingSeed,
                    senderAccountId = secrets.accountId,
                    senderSignaturePubkey = sender.signaturePubkey,
                    recipientAccountId = accountId,
                    introMessage = entry.introMessage,
                    senderProfile = ownProfileForWire(secrets.accountId),
                    mintReply = { key -> reach()?.mintReply(key) },
                    reachFor = { key ->
                        reach()?.writeDestCandidates(key, me.lakko.app.reach.WritePurpose.CONTACT_REQUEST).orEmpty()
                    },
                )
            }
            me.lakko.app.reach.RequestResend.SENT to entry.copy(
                messageId = result.messageId,
                createdAt = result.createdAt,
                sentAtMs = System.currentTimeMillis(),
            )
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            requestResendOutcome(cause) to null
        }
    }

    /**
     * The contact a route key names (base64url of a signature key): by the
     * row's current key, else through a key an accepted reset retired, so a
     * screen opened on the old key still finds the person after the row
     * moved to the new one.
     */
    public fun contactByRouteKey(routeKey: String): AcceptedContact? {
        val accepted = mutableState.value.accepted
        accepted.firstOrNull { base64UrlEncodeNoPad(it.request.fromPubkey) == routeKey }?.let { return it }
        val keyB64 = try {
            base64Encode(me.lakko.app.auth.base64UrlDecodeNoPad(routeKey))
        } catch (_: IllegalArgumentException) {
            return null
        }
        // ADR-0074 section 3.5 - a key of the same person through proven
        // rotations names the row the person already has.
        val links = provenLinks()
        if (links.isNotEmpty()) {
            accepted.firstOrNull { samePerson(base64Encode(it.request.fromPubkey), keyB64, links) }?.let { return it }
        }
        val current = retiredKeyAliases(resetLedgerRows(), ::pinnedKeyOf)[keyB64] ?: return null
        return accepted.firstOrNull { base64Encode(it.request.fromPubkey) == current }
    }

    /** Name resolution falls through between the keys of one person: a key
     *  an accepted reset retired names the row under the current key (old
     *  bubbles keep the person's name), and the current key inherits what was
     *  learned under the key it replaced until a profile arrives for it. */
    private fun pushAliasesToDirectory() {
        val directory = profileDirectory ?: return
        // ADR-0074 section 3.5 - and from a proven successor key to the key
        // the person was first known by: a proven rotation is the same
        // person, with nothing shown.
        directory.setAliases(nameAliases(provenLinks(), resetLedgerRows(), ::pinnedKeyOf))
    }

    /** The proven steps recorded for the bound account. */
    private fun provenLinks(): List<ProvenLink> =
        boundAccountB64?.let { provenLinkStore.load(it) }.orEmpty()

    /** Whether two account keys are one person across proven rotations
     *  (ADR-0074 section 3.7): the rule contact rows are matched by. */
    private fun provenSamePerson(): (ByteArray, ByteArray) -> Boolean {
        val links = provenLinks()
        return { a, b -> samePerson(base64Encode(a), base64Encode(b), links) }
    }

    /**
     * ADR-0074 section 3.5 - whether two account keys are the same person
     * across PROVEN rotations (never across a reset or an unproven change).
     * Standard base64 keys.
     */
    public fun sameProvenPerson(aKeyB64: String, bKeyB64: String): Boolean =
        samePerson(aKeyB64, bKeyB64, provenLinks())

    /** ADR-0074 section 3.5a - whether [foundKeyB64] is a proven
     *  predecessor of [publishedKeyB64] by the steps recorded here. */
    public fun isProvenPredecessor(foundKeyB64: String, publishedKeyB64: String): Boolean =
        keyPrecedes(foundKeyB64, publishedKeyB64, provenLinks())

    /** The key conversation leaves and the contact row of the person behind
     *  [keyB64] are under: the earliest key of its proven chain. */
    public fun conversationKeyOf(keyB64: String): String = earliestKeyOf(keyB64, provenLinks())
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

    /** ADR-0074 3.5a rule 5 - the unanswered outgoing requests, durable
     *  under the device key so a re-send after a rotation of the account
     *  keys survives a process death and carries the intro. `null` in tests
     *  that supply no [Context] (in memory only, as before). */
    private val sentStore: SentRequestsStore? = appContext?.let { SentRequestsFiles.onDevice(it) }

    /** ADR-0079 section 5 — the people added through their invite link whose
     *  app has not answered yet, durable under the device key with what was
     *  written to them. `null` in tests that supply no [Context]. */
    private val pendingContactsStore: PendingContactsStore? =
        appContext?.let { PendingContactsFiles.onDevice(it) }

    /** ADR-0079 section 4 — the synced link-record register: the verdict on a
     *  request's link answer, the use count, revocations, and (reworked
     *  2026-10-17) the claims of link requests. Wired once at app start; the
     *  undecided link requests are decided when its state loads and when a
     *  claim from another device arrives. */
    @Volatile
    public var deviceGroupSync: me.lakko.app.devices.sync.DeviceGroupSyncRepository? = null
        set(value) {
            field = value
            linkDecisionTriggers?.cancel()
            linkDecisionTriggers = value?.let { sync ->
                reachScope.launch {
                    kotlinx.coroutines.flow.combine(sync.ready, sync.linkClaimsVersion) { r, v -> r to v }
                        .collect { decideLinkRequests() }
                }
            }
        }

    @Volatile
    private var linkDecisionTriggers: kotlinx.coroutines.Job? = null

    /** One decision pass at a time (C-62 sub-invariant 5): the read of the
     *  claim record, the decision and the write of the outcome never
     *  interleave with another pass. */
    private val linkDecisionMutex = kotlinx.coroutines.sync.Mutex()

    /** The sealed store of this account's undecided link requests; `null` in
     *  tests that supply no [Context] (the in-memory one is used then). */
    private fun linkRequestInbox(accountB64: String): LinkRequestInbox =
        appCtx?.let { LinkRequestInbox.onDevice(it, accountB64) }
            ?: LinkRequestInbox(accountB64, inMemoryLinkRequests)

    private val inMemoryLinkRequests: me.lakko.app.auth.SealedBlobStorage by lazy {
        object : me.lakko.app.auth.SealedBlobStorage {
            private val rows = HashMap<String, ByteArray>()
            override fun read(name: String): ByteArray? = synchronized(this) { rows[name] }
            override fun write(name: String, plaintext: ByteArray): Unit = synchronized(this) { rows[name] = plaintext }
            override fun delete(name: String): Unit = synchronized(this) { rows.remove(name) }
        }
    }

    private val inMemoryLinkClaims: LinkClaimStorage by lazy { InMemoryLinkClaimStorage() }

    private fun linkClaimRecord(accountB64: String): LinkClaimRecord =
        appCtx?.let { LinkClaimRecord.onDevice(it, accountB64) }
            ?: LinkClaimRecord(accountB64, inMemoryLinkClaims)

    private val mutableInvited: MutableStateFlow<List<PendingContact>> = MutableStateFlow(emptyList())

    /** ADR-0079 section 5 — the inviters added through their link whose app
     *  has not answered yet, for the bound account. */
    public val invited: StateFlow<List<PendingContact>> = mutableInvited.asStateFlow()

    private val mutableHandedOver: MutableStateFlow<Map<String, String>> = MutableStateFlow(emptyMap())

    /** The one-to-one each pending contact moved into this session (account
     *  id hex to group id hex): an open pending chat follows it there. */
    public val handedOver: StateFlow<Map<String, String>> = mutableHandedOver.asStateFlow()

    private val invitedLock = Any()

    /** One spend round at a time: the rotating device's owed step and the
     *  session-start retry would otherwise both re-send the same request. */
    private val spendLock = kotlinx.coroutines.sync.Mutex()

    /** Persist the Sent list of the bound account. Called after every
     *  mutation of [ContactsState.sent] has landed in state. */
    private fun persistSent() {
        val store = sentStore ?: return
        val accountB64 = boundAccountB64 ?: return
        if (!store.save(accountB64, mutableState.value.sent)) {
            Log.w(TAG, "the sent requests were not stored")
        }
    }

    /** The account whose accepted list and inbox snapshot are loaded into
     *  [state]; `null` until [bindAccount] ran, and again after [clear].
     *  Read by the persist helpers, so nothing is written for an unbound
     *  account and never into another account's list. */
    @Volatile
    private var boundAccountB64: String? = null

    /**
     * The binding of this repository to [boundAccountB64]. It ends on [clear]
     * and when [bindAccount] is handed another account. An operation that
     * awaits the network captures it and asks after every await, so a request
     * started for one account never completes into the list, the reach keys
     * or the chats of the next.
     */
    private val sessionBinding = me.lakko.app.BindingEpoch()

    /** The binding current now (a host test reads when it ends). */
    internal fun captureSessionBinding(): me.lakko.app.Binding = sessionBinding.capture()

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
        // ADR-0074 section 3.5 - a proven rotation recorded by the gate
        // reaches name resolution at once.
        ProvenPredecessors.onChanged = { pushAliasesToDirectory() }
        var acceptedGrew = false
        synchronized(bindLock) {
            if (boundAccountB64 == accountB64) return
            val switching = boundAccountB64 != null
            if (switching) sessionBinding.advance()
            val storedAccepted = appCtx?.let { AcceptedContactsStore.load(it, accountB64) }
            removedContactKeys = appCtx?.let { AcceptedContactsStore.loadRemovedKeys(it, accountB64) } ?: emptySet()
            val snapshot = inboxStore?.load(accountB64) ?: ContactsInboxSnapshot()
            val storedSent = sentStore?.load(accountB64)
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
                    sent = if (storedSent == null) state.sent else mergedSentRequests(storedSent, state.sent),
                    cursor = advanceInboxCursor(state.cursor, snapshot.cursor),
                )
            }
            boundAccountB64 = accountB64
            synchronized(waitingKnownRequests) {
                waitingKnownRequests.clear()
                waitingKnownRequests.addAll(snapshot.waiting)
            }
            mutableInvited.value = pendingContactsStore?.load(accountB64).orEmpty()
        }
        // The learned names are per account too: screens re-read them.
        profileDirectory?.onAccountChanged()
        if (acceptedGrew) persistAccepted() else pushRenamesToDirectory()
        // A request sent before the bind joins the stored list.
        if (sentStore != null && mutableState.value.sent.isNotEmpty()) persistSent()
        // The key aliases are the account's too.
        pushAliasesToDirectory()
    }

    // ---- ADR-0073 R.7 case 1 / UX 16.6: a request from an existing contact --

    /** Requests from existing contacts with no verdict yet ("the request
     *  waits, unshown, for the next check"). The mailbox cursor has moved
     *  past them, so they are also kept by the inbox store and read back at
     *  the next bind. Bounded ([MAX_WAITING_REQUESTS]). */
    private val waitingKnownRequests = ArrayList<IncomingContactRequest>()

    private fun persistWaiting() {
        val store = inboxStore ?: return
        val accountB64 = boundAccountB64 ?: return
        store.saveWaiting(accountB64, synchronized(waitingKnownRequests) { waitingKnownRequests.toList() })
    }

    private fun dropWaiting(messageId: Long) {
        val changed = synchronized(waitingKnownRequests) { waitingKnownRequests.removeAll { it.messageId == messageId } }
        if (changed) persistWaiting()
    }

    /**
     * `true` when [request] came from an account that is ALREADY a contact
     * and was dealt with here (answered, dropped or kept waiting): the caller
     * then lists nothing. `false` for a stranger's request.
     *
     * "Already a contact" is [establishedContact], read BEFORE the check
     * below (which may write a first pin): a row whose account id came only
     * from the peer's own profile establishes nothing, so a member who
     * claimed this account's id cannot make its request vanish. On a refusal
     * nothing is sent to the keys that sent the request, and nothing is
     * shown for any outcome.
     */
    private suspend fun handleKnownContactRequest(request: IncomingContactRequest): Boolean {
        val fromAccountId = request.fromAccountId ?: return false
        if (establishedContact(fromAccountId) == null &&
            !isContactKey(
                base64Encode(request.fromPubkey),
                mutableState.value.accepted.map { base64Encode(it.request.fromPubkey) },
            )
        ) {
            return false
        }
        val check = checkContactIdentity(fromAccountId)
        val verdict = knownContactRequestVerdict(
            senderIsContact = true,
            check = check,
            requestKeyB64 = base64Encode(request.fromPubkey),
            trustedKeyB64 = pinnedKeyOf(base64Encode(fromAccountId)),
        )
        when (verdict) {
            KnownRequestVerdict.SHOW -> return false
            KnownRequestVerdict.WAIT -> {
                // Kept on disk first: the cursor is about to move past it.
                val kept = synchronized(waitingKnownRequests) {
                    val next = withWaitingRequest(waitingKnownRequests, request)
                    if (next.kept && next.waiting.size != waitingKnownRequests.size) waitingKnownRequests.add(request)
                    next.kept
                }
                if (kept) persistWaiting()
                return true
            }
            KnownRequestVerdict.ANSWER -> {
                // The ack is the whole of the answer on this path: the reset
                // the check just accepted (or accepted earlier) is what moves
                // the conversation.
                try {
                    sendContactAck(request)
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (cause: Throwable) {
                    Log.w(TAG, "ADR-0073: silent ack to an existing contact failed: ${cause.javaClass.simpleName}")
                }
            }
            KnownRequestVerdict.DROP -> Log.w(TAG, "contacts inbox: a request from a contact's account was dropped")
        }
        // Answered or dropped: it waits no longer.
        dropWaiting(request.messageId)
        return true
    }

    /** Judge again the requests that were waiting for a verdict. A sender who
     *  is no longer a contact by then is an ordinary request and is returned
     *  to be listed. */
    private suspend fun retryWaitingKnownRequests(): List<IncomingContactRequest> {
        val again = synchronized(waitingKnownRequests) { waitingKnownRequests.toList() }
        if (again.isEmpty()) return emptyList()
        val toList = ArrayList<IncomingContactRequest>()
        for (request in again) {
            if (handleKnownContactRequest(request)) continue
            dropWaiting(request.messageId)
            toList += request
        }
        return toList
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
            // State bundle v3 — the pending contacts land in the live list too.
            me.lakko.app.devices.StateBundle.pendingContactsSink = { accountIdB64, incoming ->
                mergePendingFor(accountIdB64, incoming)
            }
            // The chat id each carried contact gets: their one-to-one here.
            if (chatRepo != null) {
                me.lakko.app.devices.StateBundle.contactChat = { accountIdB64, contact ->
                    bindAccountIfSignedIn()
                    if (boundAccountB64 == AcceptedContactsStore.canonicalAccountB64(accountIdB64)) {
                        bundleChatFor(contact)
                    } else {
                        contact.groupIdHex
                    }
                }
            }
        }
    }

    /**
     * ADR-0079 section 5 / state bundle v3 — add the carried pending contacts
     * this device holds neither as an accepted contact nor as a pending
     * contact (by account key or account id) to [accountIdB64]'s live list,
     * through the same entry point Add uses. `null` when this repository is
     * not bound to that account (the caller then writes the store directly).
     */
    public fun mergePendingFor(accountIdB64: String, incoming: List<PendingContact>): Int? {
        bindAccountIfSignedIn()
        val accountB64 = AcceptedContactsStore.canonicalAccountB64(accountIdB64)
        if (boundAccountB64 != accountB64) return null
        val fresh = me.lakko.app.devices.StateBundle.freshPendingContacts(
            incoming,
            mutableState.value.accepted,
            mutableInvited.value,
        )
        if (fresh.isEmpty()) return 0
        updateInvited(accountB64) { list -> fresh.fold(list) { acc, c -> PendingContacts.adding(c, acc) } }
        return fresh.size
    }

    /**
     * Whether [groupIdHex], recorded on [row], is that person's one-to-one on
     * this device ([ChatRepository.chatGroupIdHexFor] keeps a recorded id only
     * then). Without a chat layer (tests, contacts-only) a record is kept.
     */
    private fun chatLinkCounts(row: AcceptedContact, groupIdHex: String): Boolean {
        val chat = chatRepo ?: return true
        return chat.chatGroupIdHexFor(groupIdHex, row.request.fromPubkey)
            ?.equals(groupIdHex, ignoreCase = true) == true
    }

    /** Section 6.4 — the opened group requests of each account, durable
     *  until an inbox pass has listed them. */
    private val groupRequestQueues = HashMap<String, GroupRequestQueue>()

    private fun groupRequestQueue(accountB64: String): GroupRequestQueue? {
        val store = inboxStore ?: return null
        return synchronized(groupRequestQueues) {
            groupRequestQueues.getOrPut(accountB64) { GroupRequestQueue(store, accountB64) }
        }
    }

    /** Write the opened group requests held only in memory, or throw: the
     *  chat repository calls this before a journal cursor moves past them. */
    private fun flushGroupRequestsOrThrow() {
        val accountB64 = boundAccountB64 ?: return
        groupRequestQueue(accountB64)?.flushOrThrow()
    }

    /** The opened group requests waiting for this inbox pass, read as
     *  ordinary requests (their sealed part opened with [x25519Secret]). */
    private fun groupRequestsForPass(accountB64: String, x25519Secret: ByteArray): Pair<List<IncomingContactRequest>, Set<Long>> {
        val stored = groupRequestQueue(accountB64)?.all().orEmpty()
        val requests = stored.mapNotNull { entry ->
            val wire = try {
                java.util.Base64.getDecoder().decode(entry.wireB64)
            } catch (_: IllegalArgumentException) {
                return@mapNotNull null
            }
            incomingContactRequestFromWire(
                wire = wire,
                messageId = entry.messageId,
                createdAt = entry.createdAt,
                myX25519Secret = x25519Secret,
                viaGroupIdHex = entry.groupIdHex,
            )
        }
        return requests to stored.mapTo(HashSet()) { it.messageId }
    }

    /**
     * `docs/ux/who-can-find-me.md` section 6.4 — a contact request a member
     * posted inside [groupIdHex]. Opened with this account's key; one that is
     * not for this account is dropped without a trace, and so is one the
     * person's own setting hides from that group's members (unless the sender
     * is already a contact). Kept, it joins the next inbox pass as an ordinary
     * request carrying the group it came in.
     */
    private fun receiveGroupContactRequest(
        groupIdHex: String,
        senderKeyB64: String,
        envelope: ByteArray,
        dsMessageId: Long,
        createdAt: String,
    ) {
        val secrets = authRepo.identitySecrets.value ?: return
        val senderKey = try {
            me.lakko.app.auth.base64Decode(senderKeyB64)
        } catch (_: IllegalArgumentException) {
            return
        }
        var openedWire: ByteArray? = null
        val request = try {
            me.lakko.app.auth.SessionSecretWipe.identity(secrets.accountIdentitySeed).use { identity ->
                val secret = identity.x25519SecretKey()
                try {
                    openGroupContactRequest(
                        envelope = envelope,
                        senderKey = senderKey,
                        check = { env, key -> uniffi.lakko_ffi_uniffi.checkGroupContactRequest(env, secret, key) },
                        toRequest = { wire ->
                            openedWire = wire
                            incomingContactRequestFromWire(
                                wire = wire,
                                messageId = groupRequestMessageId(groupIdHex, dsMessageId),
                                createdAt = createdAt,
                                myX25519Secret = secret,
                                viaGroupIdHex = groupIdHex,
                            )
                        },
                    )
                } finally {
                    secret.fill(0)
                }
            }
        } catch (e: Throwable) {
            // The code only: nothing that names anyone.
            Log.w(TAG, "group contact request dropped: ${groupRequestErrorCode(e)}")
            null
        } ?: return
        val onePerson = provenSamePerson()
        val senderIsContact = mutableState.value.accepted.any { onePerson(it.request.fromPubkey, senderKey) }
        if (!groupRequestShown(allowsGroupScope(groupIdHex), senderIsContact)) return
        val wire = openedWire ?: return
        // Durable before this returns, so before the group cursor moves past
        // the row; a write that does not land is retried by the cursor gate
        // ([flushGroupRequestsOrThrow]), which holds the cursor meanwhile.
        val accountB64 = boundAccountB64 ?: return
        val stored = groupRequestQueue(accountB64)?.add(
            StoredGroupRequest(
                messageId = request.messageId,
                groupIdHex = groupIdHex,
                wireB64 = java.util.Base64.getEncoder().encodeToString(wire),
                createdAt = createdAt,
            ),
        )
        if (stored == false) Log.w(TAG, "group contact request kept in memory; stored before the cursor moves")
        reachScope.launch {
            try {
                refreshInbox()
            } catch (e: Throwable) {
                me.lakko.app.rethrowIfCancelled(e)
                Log.w(TAG, "group contact request: inbox pass failed (${e.javaClass.simpleName})")
            }
        }
    }

    /** Whether this person's "Who can find me" setting lets the members of
     *  [groupIdHex] find them (the default setting when none is held, as the
     *  reach keys use it). */
    private fun allowsGroupScope(groupIdHex: String): Boolean {
        val id = me.lakko.app.reach.reachUnhex(groupIdHex) ?: return false
        val setting = reach()?.currentVisibility() ?: uniffi.lakko_ffi_uniffi.defaultVisibilitySetting()
        return try {
            uniffi.lakko_ffi_uniffi.allowsScope(
                setting,
                uniffi.lakko_ffi_uniffi.FfiScopeId(uniffi.lakko_ffi_uniffi.FfiScopeKind.GROUP, id),
            )
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * The contact's chat on this device: the person's one-to-one
     * ([ChatRepository.chatGroupIdHexFor]), never a recorded id that is a
     * group, a one-to-one with someone else or gone. Every screen that opens
     * "their chat" asks this. Without a chat layer (tests) the record.
     */
    public fun chatOf(contact: AcceptedContact): String? {
        val chat = chatRepo ?: return contact.groupIdHex
        return chat.chatGroupIdHexFor(contact.groupIdHex, contact.request.fromPubkey)
    }

    /**
     * A person in contacts can always be messaged (maintainer, 2026-11-05):
     * with no chat on this device, Message on the card and a pick in New chat
     * found a new one-to-one ([foundOneToOneWith] over
     * `ChatRepository.startGroupWith`, linked onto the row with
     * [linkOneToOne]). One starter for both, so a founding and its progress
     * are shared and a second tap anywhere founds nothing.
     */
    private val chatStarter: me.lakko.app.ui.contacts.ContactChatStarter? by lazy {
        if (chatRepo == null) return@lazy null
        me.lakko.app.ui.contacts.ContactChatStarter(found = { contact -> messageKnownPerson(contact) })
    }

    /**
     * `docs/ux/who-can-find-me.md` section 6.4 — Message on a known person with
     * no chat on this device, in the one order:
     *
     * 0. A request to them is already waiting in Sent: nothing new is sent.
     * 1. A grant this device holds still reaches them: the one-to-one is
     *    founded (its Welcome goes to their inbox) and opened.
     * 2. Else both are current members of a group (not a one-to-one): the
     *    contact request goes inside it, sealed to them; no chat is created
     *    and the request stands in Sent as Waiting.
     * 3. Else nothing is sent.
     *
     * A failed founding throws; a request that could not be sent is
     * [MessageOutcome.RequestFailed].
     */
    public suspend fun messageKnownPerson(contact: AcceptedContact): MessageOutcome {
        val chat = chatRepo ?: throw IllegalStateException("no chat layer")
        val accountId = contact.request.fromAccountId ?: throw ContactHasNoAccountId()
        val secrets = authRepo.identitySecrets.value ?: throw IllegalStateException("no session")
        val peerKey = contact.request.fromPubkey
        return runMessageOrder(
            waiting = requestWaiting(mutableState.value.sent, base64Encode(accountId)),
            grantReaches = {
                val candidates = reach()?.reachAuthCandidates(peerKey, me.lakko.app.reach.WritePurpose.WELCOME).orEmpty()
                useClientWith(secrets) { client, _ ->
                    me.lakko.app.reach.anyCandidateOpens(candidates) { dest: MailboxDest -> client.checkReach(dest) }
                }
            },
            sharedGroup = { chat.sharedGroupWith(peerKey) },
            found = {
                foundOneToOneWith(
                    contact = contact,
                    // ADR-0062 — a contact's conversation is a one-to-one for good.
                    startOneToOne = { id, signatureKey ->
                        chat.startGroupWith(
                            recipientAccountId = id,
                            recipientSignaturePubkey = signatureKey,
                            kind = uniffi.lakko_ffi_uniffi.GroupKind.ONE_TO_ONE,
                        )
                    },
                    link = { signatureKey, groupIdHex -> linkOneToOne(signatureKey, groupIdHex) },
                )
            },
            sendInGroup = { groupIdHex ->
                sendFoundRequestInGroup(accountId, groupIdHex, null, contact.displayName, secrets) != null
            },
            noteFailure = { what -> Log.w(TAG, "message: $what") },
        )
    }



    /** Runs the founding past the screen that asked; results land on main. */
    private val startChatScope by lazy {
        kotlinx.coroutines.CoroutineScope(kotlinx.coroutines.SupervisorJob() + Dispatchers.Main.immediate)
    }

    private val noStarts = kotlinx.coroutines.flow.MutableStateFlow<Set<String>>(emptySet())

    /** Contacts (route keys) whose founding is running. */
    public val startingChat: kotlinx.coroutines.flow.StateFlow<Set<String>>
        get() = chatStarter?.starting ?: noStarts

    private val noNotices =
        kotlinx.coroutines.flow.MutableStateFlow<Map<String, me.lakko.app.ui.contacts.MessageNotice>>(emptyMap())

    /** The line each contact's last Message left (route key -> notice). */
    public val messageNotices: kotlinx.coroutines.flow.StateFlow<Map<String, me.lakko.app.ui.contacts.MessageNotice>>
        get() = chatStarter?.notices ?: noNotices

    /** Section 6.4 — whether a request to [contact] is waiting in Sent. */
    public fun requestWaitingFor(contact: AcceptedContact): Boolean {
        val accountId = contact.request.fromAccountId ?: return false
        return requestWaiting(mutableState.value.sent, base64Encode(accountId))
    }

    /** Found the one-to-one with [contact], then [onFounded] with its id. */
    public fun startChatWith(routeKey: String, contact: AcceptedContact, onFounded: (String) -> Unit) {
        val starter = chatStarter ?: return
        startChatScope.launch { starter.start(routeKey, contact)?.let(onFounded) }
    }

    /** The chat id a pairing bundle carries for [contact]: the person's
     *  one-to-one on this device, never a stored id that is a group or gone. */
    private fun bundleChatFor(contact: AcceptedContact): String? =
        chatRepo?.chatGroupIdHexFor(contact.groupIdHex, contact.request.fromPubkey)

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
            // A removed person who is a contact again was added on purpose
            // (the adoption rule never adds a removed key).
            val present = mutableState.value.accepted.mapTo(HashSet()) { keyHex(it.request.fromPubkey) }
            val still = removedContactKeys.filterNot { it in present }.toSet()
            if (still.size != removedContactKeys.size) {
                removedContactKeys = still
                AcceptedContactsStore.saveRemovedKeys(ctx, accountB64, still)
            }
        }
        pushRenamesToDirectory()
    }

    /** Account keys (lower-case hex) the person removed from their contacts;
     *  the one-to-one adoption rule skips them. */
    @Volatile
    private var removedContactKeys: Set<String> = emptySet()

    private fun keyHex(key: ByteArray): String = key.joinToString("") { "%02x".format(it) }

    private fun keyOfHex(hex: String): ByteArray =
        hex.chunked(2).map { it.toInt(16).toByte() }.toByteArray()

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
        // A person removed from the list stays removed although the
        // conversation remains (seen on web 2026-10-24: a removed contact
        // came back on the next load).
        val onePerson = provenSamePerson()
        if (removedContactKeys.any { onePerson(keyOfHex(it), signatureKey) }) return false
        var changed = false
        var sentChanged = false
        mutableState.update { state ->
            // `update` may re-run the lambda on a lost CAS; the verdict is the
            // last attempt's, so reset before deciding.
            changed = false
            sentChanged = false
            // The joined one-to-one answers a request sent to this person.
            val sent = sentAfterOneToOne(state.sent, signatureKey, accountId)
            sentChanged = sent.size != state.sent.size
            val next = adoptingOneToOnePeer(
                accepted = state.accepted,
                signatureKey = signatureKey,
                accountId = accountId,
                groupIdHex = groupIdHex,
                nowMs = nowMs,
                createdAt = createdAt,
                samePerson = onePerson,
                linkCounts = ::chatLinkCounts,
            )
            if (next == null && !sentChanged) return@update state
            changed = next != null
            state.copy(accepted = next ?: state.accepted, sent = sent)
        }
        if (changed) persistAccepted()
        if (sentChanged) persistSent()
        return changed
    }

    /**
     * Put a person the viewer removed back on the contact list, with their
     * one-to-one (`docs/ux/conversation-info.md` section 2.1, amended
     * 2026-10-24): the removal is forgotten and the one-to-one rule applied.
     * Local; sends nothing. Returns whether anything changed; a second call
     * for a person already back changes nothing.
     */
    public fun restoreRemovedContact(
        signatureKey: ByteArray,
        accountId: ByteArray?,
        groupIdHex: String,
        nowMs: ULong,
        createdAt: String,
    ): Boolean {
        bindAccountIfSignedIn()
        val accountB64 = boundAccountB64 ?: return false
        val hex = keyHex(signatureKey)
        val wasRemoved = hex in removedContactKeys
        if (wasRemoved) {
            removedContactKeys = removedContactKeys - hex
            appCtx?.let { AcceptedContactsStore.saveRemovedKeys(it, accountB64, removedContactKeys) }
        }
        val adopted = adoptOneToOnePeer(signatureKey, accountId, groupIdHex, nowMs, createdAt)
        return wasRemoved || adopted
    }

    /** Whether the viewer removed the person holding [signatureKey] from
     *  their contact list. */
    public fun isRemovedContact(signatureKey: ByteArray): Boolean = keyHex(signatureKey) in removedContactKeys

    /**
     * Record [groupIdHex] as the one-to-one with the contact holding
     * [signatureKey]: the conversation that was just founded for them
     * ([foundOneToOneWith]). Unlike [adoptOneToOnePeer] this replaces a
     * recorded id: the caller founded the conversation because the recorded
     * one is gone from this device. No-op for a key that is not a contact.
     * Returns whether anything changed; a changed list is persisted.
     */
    public fun linkOneToOne(signatureKey: ByteArray, groupIdHex: String): Boolean {
        bindAccountIfSignedIn()
        if (boundAccountB64 == null) return false
        var changed = false
        mutableState.update { state ->
            changed = false
            val next = state.accepted.map { contact ->
                if (contact.request.fromPubkey.contentEquals(signatureKey) && contact.groupIdHex != groupIdHex) {
                    changed = true
                    contact.copy(groupIdHex = groupIdHex)
                } else {
                    contact
                }
            }
            if (changed) state.copy(accepted = next) else state
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
        // ADR-0073 R.2 - the scheduled device-list arm checks an account's
        // published identity through this repository's fail-closed gate,
        // which is what accepts an attested reset.
        chatRepo?.contactIdentityCheck = { accountId, heldKeyB64 -> checkContactIdentity(accountId, heldKeyB64) }
        // The one seam between the gate and everything that follows an
        // accepted reset (R.5 to R.7).
        ResetEvents.subscribeAs("contacts-repository", ::onResetAccepted)
        // R.4 - a Welcome to a person whose reset this device accepted may be
        // signed with the continuity key of their new mailbox.
        reach()?.acceptedResetOf = { ownerKey -> acceptedResetOfKey(base64Encode(ownerKey)) }
        // ADR-0074 section 3.5a rule 6 - a refused mailbox write re-checks
        // the person's address through the same gate, once.
        chatRepo?.addressRecheck = { accountKey, accountId -> recheckAddress(accountKey, accountId) }
        // Rule 5 - the rotating device's owed step, and the round a kept
        // device runs over its own links and requests after adopting.
        chatRepo?.spendPreRotationGrants = { spendPreRotationGrants() }
        chatRepo?.contactAccountId = { peerKey -> contactAccountId(peerKey) }
        // A contact's chat is a one-to-one with that PERSON, across a proven
        // rotation of their key ([conversationWithContact]).
        chatRepo?.samePersonKey = { a, b -> sameProvenPerson(a, b) }
        // `docs/ux/who-can-find-me.md` 6.4 — a request sent inside a group.
        chatRepo?.groupContactRequestIntake = { groupIdHex, senderKey, envelope, messageId, createdAt ->
            receiveGroupContactRequest(groupIdHex, senderKey, envelope, messageId, createdAt)
        }
        chatRepo?.groupContactRequestsFlush = { flushGroupRequestsOrThrow() }
        // ADR-0074 (a device paired after a rotation) - a pairing state
        // bundle that moved the conversation key rebuilds a chat layer that
        // was already loaded under the current key.
        if (appContext != null) {
            me.lakko.app.devices.StateBundle.conversationKeySink = { accountIdB64 ->
                val secrets = authRepo.identitySecrets.value
                if (secrets != null &&
                    base64Encode(secrets.accountId) == AcceptedContactsStore.canonicalAccountB64(accountIdB64)
                ) {
                    chatRepo?.rebindConversationIdentity(secrets)
                }
            }
        }
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
        pushPendingToDirectory()
    }

    /** ADR-0079 — hand the pending contacts' link-stated names to the
     *  directory, so a roster names a pending contact as the lists do. */
    private fun pushPendingToDirectory() {
        profileDirectory?.setPendingContacts(mutableInvited.value)
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

        val bound = sessionBinding.capture()
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
                    reach()?.writeDestCandidates(key, me.lakko.app.reach.WritePurpose.CONTACT_REQUEST).orEmpty()
                },
            )
            // The request left; the "sent" row is the sending account's.
            if (!bound.isCurrent) return@useClientWith result
            mutableState.update { state ->
                state.copy(
                    sent = state.sent + SentContactRequest(
                        recipientUsername = result.recipientUsername,
                        recipientAccountIdB64 = base64Encode(result.recipientAccountId),
                        messageId = result.messageId,
                        createdAt = result.createdAt,
                        introMessage = introMessage.toByteArray(Charsets.UTF_8),
                        recipientKeyB64 = result.recipientSignaturePubkey?.let { base64Encode(it) },
                    ),
                )
            }
            persistSent()
            result
        }
    }

    /**
     * ADR-0060 — a contact request to a person a search found by their
     * username (they are in Everyone). [grantWire] is the grant the search
     * found them under (`encodeReachGrant`), used for this one request and
     * never kept; without one the write takes the grant this device holds
     * for them, or goes proofless into their open mailbox, sealed to the
     * key [mailboxBinding] (their public record's) names (ADR-0078). Either
     * is verified against the recipient's account key before anything is
     * written.
     */
    public suspend fun sendFoundContactRequestByUsername(
        recipientUsername: String,
        introMessage: ByteArray,
        grantWire: ByteArray?,
        mailboxBinding: ByteArray? = null,
        /** The name the row showed; the Sent list keeps it. */
        shownName: String? = null,
    ): SendResult {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(IllegalStateException("no identity secrets — please re-enter your password"))
        val bound = sessionBinding.capture()
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
                    val found = foundRequestDest(
                        grantWire = grantWire,
                        mailboxBinding = mailboxBinding,
                        recipientKey = reach()?.mailboxAccountKey(key) ?: key,
                        crypto = mailboxCrypto,
                        authOf = { seed -> me.lakko.app.ds.DsReachAuth(me.lakko.app.ds.reachSigner(seed)) },
                        decodeGrant = { uniffi.lakko_ffi_uniffi.decodeReachGrant(it) },
                    )
                    reach()?.writeDestCandidates(key, me.lakko.app.reach.WritePurpose.CONTACT_REQUEST, first = found)
                        ?: listOfNotNull(found?.let { held -> { held } })
                },
            )
            if (bound.isCurrent) recordSent(result.recipientUsername, shownName, result, introMessage)
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
        /** ADR-0078 - the mailbox binding a public record carried. */
        mailboxBinding: ByteArray? = null,
        /** The name the row showed; the Sent list keeps it. */
        shownName: String? = null,
    ): SendResult {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(IllegalStateException("no identity secrets — please re-enter your password"))
        val explicit = foundRequestDest(
            grantWire = grantWire,
            mailboxBinding = mailboxBinding,
            recipientKey = reach()?.mailboxAccountKey(recipientSignaturePubkey) ?: recipientSignaturePubkey,
            crypto = mailboxCrypto,
            authOf = { seed -> me.lakko.app.ds.DsReachAuth(me.lakko.app.ds.reachSigner(seed)) },
            decodeGrant = { uniffi.lakko_ffi_uniffi.decodeReachGrant(it) },
        )
        val replyGrant = reach()?.mintReply(recipientSignaturePubkey)
        val bound = sessionBinding.capture()
        return try {
            useClientWith(secrets) { client, sender ->
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
                    reachCandidates = reach()?.writeDestCandidates(
                        recipientSignaturePubkey,
                        me.lakko.app.reach.WritePurpose.CONTACT_REQUEST,
                        first = explicit,
                    ) ?: listOfNotNull(explicit?.let { held -> { held } }),
                )
                // A person found by name alone has no username: the Sent row
                // shows the name, and it is what makes a search row read
                // Request sent.
                if (bound.isCurrent) recordSent(shownUsername.orEmpty(), shownName, result, introMessage)
                result
            }
        } catch (e: Throwable) {
            me.lakko.app.rethrowIfCancelled(e)
            // `docs/ux/who-can-find-me.md` 6.4 — the same order everywhere: no
            // grant that reaches them, so a group both are in carries it.
            if (!noGrantReaches(e)) throw e
            val shared = chatRepo?.sharedGroupWith(recipientSignaturePubkey) ?: throw e
            sendFoundRequestInGroup(recipientAccountId, shared, shownUsername, shownName, secrets) ?: throw e
        }
    }

    /** Whether [e] says no grant this device holds reaches the person (a
     *  `401` on every candidate, or no mailbox key known at all). */
    private fun noGrantReaches(e: Throwable): Boolean =
        isMailboxRefusal(e) ||
            ((e as? ContactsError)?.variant as? ContactsError.Variant.Wrapped)?.cause is MailboxUnknown

    /** Section 6.4 for a search row: the request inside [groupIdHex], then
     *  Sent. `null` when it could not be sent. */
    private suspend fun sendFoundRequestInGroup(
        recipientAccountId: ByteArray,
        groupIdHex: String,
        shownUsername: String?,
        shownName: String?,
        secrets: IdentitySecrets,
    ): SendResult? {
        val chat = chatRepo ?: return null
        val bound = sessionBinding.capture()
        return try {
            val composed = useClientWith(secrets) { client, sender ->
                client.composeGroupContactRequest(
                    senderSigningSeed = sender.signingSeed,
                    senderAccountId = secrets.accountId,
                    recipientAccountId = recipientAccountId,
                    introMessage = ByteArray(0),
                    senderProfile = ownProfileForWire(secrets.accountId),
                    mintReply = { key -> reach()?.mintReply(key) },
                )
            }
            val sent = chat.sendGroupContactRequest(groupIdHex, composed.envelope)
            val result = SendResult(
                recipientUsername = shownUsername.orEmpty(),
                recipientAccountId = recipientAccountId,
                messageId = sent.messageId,
                createdAt = sent.createdAt,
                recipientSignaturePubkey = composed.recipientSignaturePubkey,
            )
            if (bound.isCurrent) recordSent(shownUsername.orEmpty(), shownName, result, ByteArray(0))
            result
        } catch (e: Throwable) {
            me.lakko.app.rethrowIfCancelled(e)
            Log.w(TAG, "search: request inside a group not sent (${e.javaClass.simpleName})")
            null
        }
    }

    private fun recordSent(username: String, name: String?, result: SendResult, introMessage: ByteArray) {
        mutableState.update { state ->
            state.copy(
                sent = state.sent + SentContactRequest(
                    recipientUsername = username,
                    recipientName = name,
                    recipientAccountIdB64 = base64Encode(result.recipientAccountId),
                    messageId = result.messageId,
                    createdAt = result.createdAt,
                    introMessage = introMessage,
                    recipientKeyB64 = result.recipientSignaturePubkey?.let { base64Encode(it) },
                ),
            )
        }
        persistSent()
    }

    /** ADR-0078 - the binding check and sealing of mailbox rows; a test
     *  injects a fake (the host JVM loads no native library). */
    internal var mailboxCrypto: MailboxCrypto = NativeMailboxCrypto

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
        val bound = sessionBinding.capture()
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
            // ADR-0074 part 3, Stage A - after a rotation of the account
            // keys the device bindings that exist still name the previous
            // key, so a Welcome for one of them arrives in the PREVIOUS
            // identity's mailbox. Read it first (no long poll).
            try {
                useClientWith(secrets) { client, sender ->
                    // A mailbox's ids are its own: after a rotation the kept
                    // cursor belongs to the previous key's mailbox.
                    val currentKeyB64 = base64Encode(sender.signaturePubkey)
                    val mailboxes = inboxMailboxStore
                    val read = if (mailboxes == null) {
                        InboxRead(mutableState.value.cursor, usesKeptCursor = true, keptCursorOwnerB64 = null)
                    } else {
                        val owner = mailboxes.ownerKey(accountB64)
                        if (owner == null) mailboxes.setOwnerKey(accountB64, currentKeyB64)
                        inboxReadFor(
                            ownerKeyB64 = owner,
                            currentKeyB64 = currentKeyB64,
                            keptCursor = mutableState.value.cursor,
                            laterCursor = mailboxes.cursor(accountB64, currentKeyB64),
                        )
                    }
                    read.keptCursorOwnerB64?.let { owner ->
                        // The previous identity's mailbox continues from the
                        // kept cursor (forward-only, so repeating is harmless).
                        try {
                            authRepo.advancePredecessorMailboxCursor(me.lakko.app.auth.base64Decode(owner), mutableState.value.cursor)
                        } catch (_: IllegalArgumentException) {
                            // A damaged row: the old mailbox is read from 0.
                        }
                    }
                    pollPredecessorWelcomes()
                    val result = client.pollStrangerInbox(
                        own = sender.ownMailbox,
                        myX25519Secret = sender.x25519Secret,
                        after = read.after,
                        waitSecs = waitSecs,
                        mailboxSigner = sender.mailboxSigner,
                    )
                    // The session ended (or switched) during the poll: nothing
                    // of this batch reaches the reach keys, the chats or the
                    // name directory, which by now belong to nobody or to the
                    // next account.
                    if (!bound.isCurrent) return@useClientWith
                    // `docs/ux/who-can-find-me.md` 6.4 — requests that came
                    // inside a shared group run through the same steps.
                    val (groupRequests, groupRequestIds) = groupRequestsForPass(accountB64, sender.x25519Secret)
                    val fetchedRequests = result.events.filterIsInstance<
                        IncomingMailboxEvent.ContactRequest>(
                    ).map { it.request } + groupRequests
                    // ADR-0048 §3 — a request's profile is bound to its
                    // signature-verified `from_pubkey`; store it (strictly newer
                    // only) for EVERY fetched request, before any is set aside.
                    // A request through one of this account's invite links is
                    // kept off the request list and accepted with no card
                    // (ADR-0079), so a store limited to the listed requests
                    // dropped its name, and the new contact read "Member" until
                    // the person's profile arrived in the one-to-one. Web and
                    // iOS store it at this point too.
                    profileDirectory?.let { directory ->
                        for (req in fetchedRequests) {
                            val profile = req.profile ?: continue
                            directory.onProfileReceived(
                                signatureKeyB64 = java.util.Base64.getEncoder()
                                    .encodeToString(req.fromPubkey),
                                profile = profile,
                            )
                        }
                    }
                    // ADR-0073 R.7 case 1 / UX 16.6 — a request from an
                    // account that is ALREADY a contact is never a request
                    // row: the R.2 check runs on the sender's account and the
                    // request is answered silently or dropped. The ones that
                    // waited for a verdict are judged again first.
                    val newIncoming = triageLinkAnswers(
                        retryWaitingKnownRequests() +
                            // Section 6.4 — a contact asking for a chat
                            // inside a group is a request row, not a silent
                            // answer.
                            fetchedRequests.filterNot {
                                !askedForChatThroughGroup(it, mutableState.value.accepted) &&
                                    handleKnownContactRequest(it)
                            },
                    )
                    val welcomes = result.events.filterIsInstance<
                        IncomingMailboxEvent.Welcome>(
                    )
                    val acks = result.events.filterIsInstance<
                        IncomingMailboxEvent.ContactAck>(
                    ).map { it.ack }
                    // ADR-0079 amendment of 2026-10-19 — refusals of this
                    // account's link requests: the pending contact goes,
                    // silently, when the refusal verifies.
                    for (refusal in result.events.filterIsInstance<IncomingMailboxEvent.LinkRefusal>()) {
                        applyLinkRefusal(refusal.refusal, sender.signaturePubkey)
                    }
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
                    var joinFailure: InboxError? = null
                    // ADR-0087 section 9 — a Welcome that could not be kept
                    // stays behind the cursor, or it is lost.
                    var welcomeNotKept = false
                    val answeredBy = HashMap<String, String>()
                    if (welcomes.isNotEmpty()) {
                        Log.i(TAG, "inbox: ${welcomes.size} Welcome row(s) received")
                    }
                    for (welcome in welcomes) {
                        if (!bound.isCurrent) return@useClientWith
                        if (chatRepo != null) {
                            try {
                                val joinedHex = chatRepo.joinGroupFromWelcome(welcome.welcomeBytes)
                                Log.i(TAG, "inbox: Welcome ${me.lakko.app.chat.WelcomeOutcome.JOINED}")
                                // ADR-0060 §6 — the answer to a request of this
                                // device's arrived: retire its reply key for
                                // that person.
                                chatRepo.oneToOnePeerKey(joinedHex)?.let { peer ->
                                    reach()?.retireRepliesTo(peer)
                                    answeredBy[base64Encode(peer)] = joinedHex
                                }
                                // ADR-0079 section 5 — a one-to-one with a
                                // person added through their link: what was
                                // written to them goes out now, in order.
                                handOverJoinedPendingContacts()
                            } catch (e: kotlinx.coroutines.CancellationException) {
                                throw e
                            } catch (e: Throwable) {
                                Log.i(
                                    TAG,
                                    "inbox: Welcome ${me.lakko.app.chat.welcomeOutcomeOf(e)} (${e.javaClass.simpleName})",
                                )
                                // ADR-0060 — every device of the account reads
                                // the one account mailbox, so a Welcome sealed
                                // to another of its devices lands here too and
                                // fails to open: not a failure of this device.
                                if (me.lakko.app.chat.isWelcomeForAnotherDevice(e)) continue
                                // ADR-0087 section 9 — the core refused it for
                                // a group this device still holds as active:
                                // the removing commit may be behind it in the
                                // journal. Kept, the journal is read, the join
                                // is tried again; the cursor must not take it.
                                val stillActive = me.lakko.app.chat.groupStillActiveHex(e)
                                if (stillActive != null) {
                                    if (!chatRepo.holdWelcome(stillActive, welcome.welcomeBytes)) welcomeNotKept = true
                                    continue
                                }
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
                                    joinFailure = InboxError.WELCOME_JOIN_FAILED
                                }
                            }
                        }
                    }
                    // The session ended (or switched) during the poll: this batch
                    // belongs to the previous account's inbox. `clear()` already
                    // reset the state; do not land the rows or the cursor in it.
                    if (!bound.isCurrent || boundAccountB64 != accountB64) return@useClientWith
                    if (!read.usesKeptCursor && !welcomeNotKept) {
                        inboxMailboxStore?.advance(accountB64, currentKeyB64, result.cursor)
                    }

                    // ADR-0074 3.5a rule 5 - a request sent again from a
                    // rotated key replaces the row it re-sends: by the steps
                    // the gate recorded, or by the proof the directory serves
                    // with the new key (checked here, off the state update).
                    val links = provenLinks()
                    val provenNow = successorCandidates(mutableState.value.pending, newIncoming)
                        .filterNot { (_, newKey, oldKey) -> keyPrecedes(base64Encode(oldKey), base64Encode(newKey), links) }
                        .filter { (accountId, newKey, oldKey) -> client.provesSuccessor(accountId, oldKey, newKey) }
                    val supersedes: (ByteArray, ByteArray) -> Boolean = { newKey, oldKey ->
                        keyPrecedes(base64Encode(oldKey), base64Encode(newKey), links) ||
                            provenNow.any { it.second.contentEquals(newKey) && it.third.contentEquals(oldKey) }
                    }
                    // A person who answered is a contact from here on and
                    // their profile comes after: the name they were found
                    // under stands in for it, filed before the row appears
                    // and while the Sent record still holds it.
                    profileDirectory?.let { directory ->
                        for (found in foundNamesOfAnswers(mutableState.value.sent, acks, answeredBy.keys)) {
                            directory.onFoundPersonAnswered(found.signatureKeyB64, found.displayName, found.verifiedUsername)
                        }
                    }
                    var dropped: List<IncomingContactRequest> = emptyList()
                    var acceptedChanged = false
                    val ackerIsPerson = provenSamePerson()
                    mutableState.update { state ->
                        // Someone accepted one of this device's requests: they are
                        // a contact here too, with their account id.
                        val applied = applyContactAcks(state.accepted, state.sent, acks, ackerIsPerson)
                        // A Welcome from the person asked answers the
                        // request; the ack may never come. They become a
                        // contact in the step that retires the Sent entry.
                        val answered = applyAnsweringWelcomes(
                            accepted = applied.accepted,
                            sent = applied.sent,
                            answered = answeredBy,
                            nowMs = System.currentTimeMillis().toULong(),
                            createdAt = java.time.Instant.now().toString(),
                        )
                        acceptedChanged = applied.acceptedChanged || answered.acceptedChanged
                        // Dedupe: a row already pending (same DS messageId) or a
                        // sender already accepted is not a new request.
                        val merged = mergeFetchedRequests(
                            pending = state.pending,
                            accepted = answered.accepted,
                            fetched = newIncoming,
                            supersedes = supersedes,
                        )
                        dropped = merged.dropped
                        state.copy(
                            accepted = answered.accepted,
                            sent = answered.sent,
                            pending = withoutAcceptedSenders(merged.pending, answered.accepted),
                            // Never backwards: a stale batch cannot re-arm a
                            // re-fetch of rows this device already handled.
                            // A later mailbox's cursor is kept apart.
                            cursor = if (read.usesKeptCursor) {
                                advanceInboxCursor(state.cursor, result.cursor)
                            } else {
                                state.cursor
                            },
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
                                    !isManualRefresh && it == InboxError.WELCOME_JOIN_FAILED
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
                    // Section 6.4 — listed (or dealt with) and persisted: the
                    // group requests of this pass wait no longer.
                    groupRequestQueue(accountB64)?.remove(groupRequestIds)
                    // An answered request leaves the durable Sent list.
                    if (acks.isNotEmpty() || answeredBy.isNotEmpty()) persistSent()
                    persistInboxCursor()
                }
                // ADR-0079 — a pending contact whose one-to-one exists already,
                // a link revoked on another device of the account, and the
                // undecided link requests.
                handOverJoinedPendingContacts()
                revokeLinksRevokedElsewhere()
                decideLinkRequests()
                retryOwedAccepts()
            } catch (e: ContactsError) {
                // Background long-polls surface failures on the `lastError`
                // slot too — the UI renders one plain sentence, never the
                // transport's own message (endpoint paths and pseudonyms are
                // not user copy). The raw error goes to the log; the caller
                // (a `LaunchedEffect` loop) retries on the next iteration.
                Log.w(TAG, "contacts inbox: poll failed (${e.javaClass.simpleName})", e)
                mutableState.update {
                    it.copy(refreshing = false, lastError = InboxError.POLL_FAILED)
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
            val onePerson = provenSamePerson()
            state.copy(
                pending = remaining,
                accepted = acceptingRequests(state.accepted, accepted, groupIdHex, onePerson, ::chatLinkCounts),
                sent = sentAfterAccepting(state.sent, accepted),
            )
        }
        persistAccepted()
        persistInboxPending()
        persistSent()
    }

    /** One accept at a time: the retry of an owed one must not run beside the
     *  first attempt, which holds the same record. */
    private val acceptMutex = kotlinx.coroutines.sync.Mutex()

    /**
     * Accept the pending request [messageId] ([runAccept]): the chat exists,
     * and its Welcome was delivered or is owed; or nothing stands and the
     * request stays listed. Never throws (a cancellation is let through).
     */
    public suspend fun acceptRequest(messageId: Long): AcceptOutcome {
        bindAccountIfSignedIn()
        val request = mutableState.value.pending.firstOrNull { it.messageId == messageId }
            ?: return AcceptOutcome.DONE
        val accountB64 = boundAccountB64 ?: return AcceptOutcome.FAILED
        return acceptMutex.withLock { answerRequest(request, accountB64) }
    }

    private suspend fun answerRequest(request: IncomingContactRequest, accountB64: String): AcceptOutcome {
        val chat = chatRepo
        val fromAccountId = request.fromAccountId
        if (chat == null || fromAccountId == null) {
            Log.w(TAG, "accept failed: no-chat-or-account")
            return AcceptOutcome.FAILED
        }
        val key = LinkClaims.hex(request.fromPubkey)
        val claims = linkClaimRecord(accountB64)
        return runAccept(
            object : AcceptDeps {
                override suspend fun answerable(): Boolean = answerReachable(request)

                override fun noteFailure(reason: String) {
                    Log.w(TAG, "accept failed: $reason")
                }

                override fun founded(): LinkClaims.Founded? = claims.owedAccept(key)

                override fun note(): LinkClaims.HeldNote? = null

                override suspend fun prepare(): LinkClaims.Founded =
                    chat.prepareLinkOneToOne(fromAccountId, request.fromPubkey)

                override fun record(founded: LinkClaims.Founded): Boolean {
                    val recorded = claims.setOwedAccept(key, founded)
                    if (!recorded && claims.owedAccept(key) == null) chat.abandonPreparedOneToOne(founded.groupIdHex)
                    return recorded
                }

                // The person pressed Accept on this device: no other device
                // of the account is answering, so there is nothing to tell them.
                override suspend fun postNote(founded: LinkClaims.Founded): Boolean = true

                override suspend fun hold(founded: LinkClaims.Founded): Boolean {
                    if (!chat.holdLinkOneToOne(founded.groupIdHex)) return false
                    // The contact and its chat stand from here, whatever
                    // becomes of the Welcome.
                    acceptContactRequest(request.messageId, groupIdHex = founded.groupIdHex)
                    return true
                }

                override suspend fun deliver(founded: LinkClaims.Founded) {
                    chat.deliverLinkWelcome(founded, fromAccountId)
                }

                override suspend fun forward(note: LinkClaims.HeldNote): String? = null

                override suspend fun finish(groupIdHex: String?) {
                    try {
                        sendContactAck(request)
                    } catch (e: Throwable) {
                        me.lakko.app.rethrowIfCancelled(e)
                        // The Welcome is the answer; a requester that already
                        // joined has retired the reply key this would need.
                        Log.w(TAG, "accept: ack not sent")
                    }
                    claims.forgetOwedAccept(key)
                }
            },
        )
    }

    /** Whether a grant the answer may use opens the requester's mailbox. */
    private suspend fun answerReachable(request: IncomingContactRequest): Boolean {
        val secrets = authRepo.identitySecrets.value ?: return false
        val candidates = reach()
            ?.reachAuthCandidates(request.fromPubkey, me.lakko.app.reach.WritePurpose.REPLY)
            .orEmpty()
        return useClientWith(secrets) { client, _ ->
            me.lakko.app.reach.anyCandidateOpens(candidates) { dest: MailboxDest -> client.checkReach(dest) }
        }
    }

    /** Carry on every accept whose Welcome is owed: the inbox pass, so at
     *  launch and after every poll. */
    private suspend fun retryOwedAccepts() {
        val accountB64 = boundAccountB64 ?: return
        val claims = linkClaimRecord(accountB64)
        val owed = claims.owedAcceptKeys()
        if (owed.isEmpty() || !acceptMutex.tryLock()) return
        try {
            for (key in owed) {
                val state = mutableState.value
                val request = (state.accepted.map { it.request } + state.pending)
                    .firstOrNull { LinkClaims.hex(it.fromPubkey) == key }
                if (request == null) {
                    // Removed or declined meanwhile: nobody is waiting on it.
                    claims.forgetOwedAccept(key)
                    continue
                }
                answerRequest(request, accountB64)
            }
        } finally {
            acceptMutex.unlock()
        }
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
     * Written after the Welcome, under the request's reply grant, then a held
     * `contact` grant, and nothing else: with neither it throws [NoAnswerGrant].
     * The caller treats it as best effort: the Welcome is the answer.
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
            // ADR-0074 3.5a rule 6 - a refused ack re-checks the requester's
            // address once. Each attempt re-resolves the mailbox (inside the
            // client) and tries the reply key, then a contact key, each one
            // kept beside a newer grant included.
            me.lakko.app.reach.writeRecheckingAddress(
                write = {
                    val candidates = reach()
                        ?.reachAuthCandidates(request.fromPubkey, me.lakko.app.reach.WritePurpose.REPLY)
                        .orEmpty()
                    val send: suspend (MailboxDest) -> PublishedAck = { dest ->
                        client.sendContactAck(
                            accepterSigningSeed = sender.signingSeed,
                            accepterAccountId = secrets.accountId,
                            accepterX25519Pubkey = sender.x25519Pubkey,
                            originalRequest = request,
                            // ADR-0060 §6 — the requester's reply key opens their
                            // mailbox.
                            reach = dest,
                        )
                    }
                    when (val outcome = me.lakko.app.reach.firstAcceptedWrite(candidates, send, ::isMailboxRefusal)) {
                        is me.lakko.app.reach.SignedWriteOutcome.Written -> outcome.result
                        // Never unsigned: a closed mailbox would drop it and
                        // the answer would read as sent.
                        is me.lakko.app.reach.SignedWriteOutcome.Exhausted -> throw outcome.lastRefusal ?: NoAnswerGrant()
                    }
                },
                isRefusal = ::isMailboxRefusal,
                recheck = { recheckAddress(request.fromPubkey, request.fromAccountId) },
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
        return me.lakko.app.auth.SessionSecretWipe.identity(secrets.accountIdentitySeed).use { identity ->
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
        var removedAccountId: ByteArray? = null
        mutableState.update { state ->
            val remaining = state.accepted.filterNot { it.request.messageId == messageId }
            changed = remaining.size != state.accepted.size
            val removed = state.accepted.firstOrNull { it.request.messageId == messageId }
            removedKey = removed?.request?.fromPubkey
            removedAccountId = removed?.request?.fromAccountId
            if (changed) state.copy(accepted = remaining) else state
        }
        if (changed) {
            val ctx = appCtx
            val accountB64 = boundAccountB64
            removedKey?.let { removedContactKeys = removedContactKeys + keyHex(it) }
            if (ctx != null && accountB64 != null) {
                AcceptedContactsStore.saveRemovedKeys(ctx, accountB64, removedContactKeys)
            }
            persistAccepted()
        }
        // ADR-0060 §1 — the removed person held this account's `contact` key
        // (and, through their contacts, its `fof` key): both rotate, the old
        // ones come off the mailbox at once, and the remaining one-to-ones get
        // the new contact grant in a republished profile. Their grants for
        // this device are dropped too.
        val manager = reach() ?: return
        if (!changed) return
        reachScope.launchBound(sessionBinding) {
            try {
                removedKey?.let { manager.forgetHeld(it) }
                // UX who-can-find-me 4.1: adding them back brings them in off.
                removedAccountId?.let { manager.forgetChosenContact(it) }
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

    /** The account id of the contact holding [peerKey], when one is known. */
    private fun contactAccountId(peerKey: ByteArray): ByteArray? =
        mutableState.value.accepted
            .firstOrNull { it.request.fromPubkey.contentEquals(peerKey) }
            ?.request?.fromAccountId
            ?.takeIf { it.size == CONTACT_ACCOUNT_ID_LEN }

    /**
     * Contacts > Sent, **Dismiss**: the person leaves this device's outgoing
     * list, with what it kept to send the request again. Nothing is sent: a
     * decline never reaches the requester, so this is how a request that will
     * not be answered goes away, and search can offer the person again.
     */
    public fun dismissSentRequest(request: SentContactRequest) {
        bindAccountIfSignedIn()
        mutableState.update { state ->
            state.copy(sent = withoutSentTo(state.sent, request.recipientAccountIdB64))
        }
        persistSent()
    }

    // ---- ADR-0079: the invite page (B) and the pending contact --------------

    /**
     * The signed-out invite page (`docs/ux/contact-invite-link.md` section 3,
     * "Signed out", amended 2026-10-26; ADR-0079 amendment of 2026-10-26):
     * open the link with no session, only to say who invites. The ledger
     * serves the sealed blob without a session and the name is sealed under
     * the link's secret, so the card is built on the device. Nothing is
     * consumed and nothing is sent; signing in or creating an account then
     * opens the parked link again through [openInviteLink].
     */
    public suspend fun previewInviteLink(slug: String): InvitePreviewResult {
        val link = me.lakko.app.discovery.InviteLinkArgument.decode(slug.trim())
        val secret = me.lakko.app.discovery.inviteLinkSecretOf(link.fragment) ?: return InvitePreviewResult.Dead
        // Every link is a ledger pointer; any other slug is a dead link.
        val blobId = me.lakko.app.discovery.decodeInviteBlobId(link.slug) ?: return InvitePreviewResult.Dead
        val row = try {
            // Deliberately the bare client: no session token is attached.
            discoveryClientFactory().use { discovery -> discovery.fetchInviteRow(blobId) }
        } catch (e: Throwable) {
            me.lakko.app.rethrowIfCancelled(e)
            if (me.lakko.app.discovery.isDiscoveryRateLimited(e)) return InvitePreviewResult.RateLimited
            return InvitePreviewResult.Failed
        } ?: return InvitePreviewResult.Dead
        // No session here, so nobody this link already made a contact: every
        // use spent reads as a dead link. A signed-in visitor goes through
        // consume, which tells "already used by you" from "used up".
        if (row.usedUp) return InvitePreviewResult.Dead
        val sealed: ByteArray = row.sealed
        val cap = try {
            me.lakko.app.discovery.openSealedInviteLink(sealed, secret) { bytes, key ->
                InviteCapability.openLink(bytes, key)
            }
        } catch (_: me.lakko.app.discovery.InviteLinkDamaged) {
            return InvitePreviewResult.Dead
        }
        return cap.use { capability ->
            if (!capability.isValidNow(System.currentTimeMillis().toULong())) return@use InvitePreviewResult.Dead
            InvitePreviewResult.Preview(
                displayName = capability.inviterDisplayName(),
                username = capability.inviterUsername(),
                label = capability.label(),
                ownerAccountId = capability.ownerAccountId(),
            )
        }
    }

    /**
     * Open an `/i/<slug>#<secret>` link for the invite page
     * (`docs/ux/contact-invite-link.md` section 3): fetch the sealed blob,
     * open it under the fragment's secret, and decide the page's state. No
     * use of the link is spent and nothing is sent here.
     */
    public suspend fun openInviteLink(slug: String): InviteOpenResult {
        val secrets = authRepo.identitySecrets.value ?: return InviteOpenResult.RequiresReauth
        val bound = sessionBinding.capture()
        // Sealed links (2026-09-28): the argument is `<slug>#<secret>`; a
        // missing or malformed secret is a damaged link, shown exactly like
        // one that no longer works.
        val link = me.lakko.app.discovery.InviteLinkArgument.decode(slug.trim())
        val secret = me.lakko.app.discovery.inviteLinkSecretOf(link.fragment) ?: return InviteOpenResult.Dead
        // Every link is a ledger pointer; any other slug (an inline sealed
        // blob, the form removed on 2026-10-19, included) is a dead link.
        val blobId = me.lakko.app.discovery.decodeInviteBlobId(link.slug) ?: return InviteOpenResult.Dead
        val sealed: ByteArray = try {
            authenticatedDiscoveryClient().use { discovery -> discovery.fetchInvite(blobId) }
        } catch (e: Throwable) {
            me.lakko.app.rethrowIfCancelled(e)
            if (me.lakko.app.discovery.isDiscoveryRateLimited(e)) return InviteOpenResult.RateLimited
            return InviteOpenResult.Failed(message = e.message ?: "fetch failed")
        } ?: return InviteOpenResult.Dead
        if (!bound.isCurrent) return InviteOpenResult.RequiresReauth
        val cap = try {
            me.lakko.app.discovery.openSealedInviteLink(sealed, secret) { bytes, key ->
                InviteCapability.openLink(bytes, key)
            }
        } catch (_: me.lakko.app.discovery.InviteLinkDamaged) {
            return InviteOpenResult.Dead
        }
        return cap.use { capability ->
            if (!capability.isValidNow(System.currentTimeMillis().toULong())) return@use InviteOpenResult.Dead
            val ownerAccountId = capability.ownerAccountId()
            if (ownerAccountId.contentEquals(secrets.accountId)) return@use InviteOpenResult.Own
            val opened = OpenedInvite(
                blobId = blobId,
                capabilityBytes = capability.wire(),
                ownerAccountId = ownerAccountId,
                ownerSignaturePubkey = capability.ownerSignaturePubkey(),
                ownerX25519Pubkey = capability.ownerX25519Pubkey(),
                linkId = capability.linkId(),
                linkSecret = secret,
                linkGrant = capability.linkGrant(),
                displayName = capability.inviterDisplayName(),
                username = capability.inviterUsername(),
                label = capability.label(),
            )
            classifyOpenedInvite(opened)
        }
    }

    /**
     * The page an opened link shows now ([invitePageKind]): the own link,
     * an established contact, a pending one, or the ordinary page. Local
     * reads only, so the page asks again whenever the contact list or the
     * pending contacts change while it is open (the inviter's answer to an
     * earlier Add landing).
     */
    public fun classifyOpenedInvite(invite: OpenedInvite): InviteOpenResult {
        val ownAccountId = authRepo.identitySecrets.value?.accountId
        val isOwn = ownAccountId != null && invite.ownerAccountId.contentEquals(ownAccountId)
        // ADR-0073 R.7 case 1 / UX 16.6 — "already a contact" is the pin or
        // a verified binding naming the row's own key.
        val established = if (isOwn) null else establishedContact(invite.ownerAccountId)
        val ownerHex = me.lakko.app.ds.hexOf(invite.ownerAccountId)
        return when (invitePageKind(isOwn, established != null, pendingContact(ownerHex) != null)) {
            InvitePageKind.OWN -> InviteOpenResult.Own
            InvitePageKind.ALREADY -> {
                val existing = established ?: return InviteOpenResult.Opened(invite)
                InviteOpenResult.KnownContact(
                    routeKey = base64UrlEncodeNoPad(existing.request.fromPubkey),
                    groupIdHex = chatOf(existing),
                    statedName = invite.statedName,
                    invite = invite,
                )
            }
            InvitePageKind.PENDING -> InviteOpenResult.PendingInviter(
                accountIdHex = ownerHex,
                statedName = invite.statedName,
                invite = invite,
            )
            InvitePageKind.DEFAULT -> InviteOpenResult.Opened(invite)
        }
    }

    /**
     * **Add** on the invite page (ADR-0079 sections 2 and 5, amended
     * 2026-10-19), in the order [InviteAddOrder] pins: the link's ledger use
     * is spent first; a link used up or gone since the page opened keeps
     * NOTHING (no pending contact, no held message) and the page goes to
     * Contacts. Only then the inviter is recorded as a pending contact, BEFORE
     * the request carrying the link answer is sent (an answer that comes back
     * at once always finds it), and removed again when the send fails. This
     * device founds no conversation: the inviter's device does, and its
     * Welcome lands here.
     */
    public suspend fun addThroughInviteLink(invite: OpenedInvite): InviteAddResult {
        val secrets = authRepo.identitySecrets.value ?: return InviteAddResult.RequiresReauth
        val bound = sessionBinding.capture()
        // ADR-0016 rule 2 / ADR-0073 R.2 — the capability's inline bundle never
        // went through the fetch gate: the pin question is asked of it before
        // anything is spent or sealed (an attested reset is accepted there).
        try {
            useClientWith(secrets) { client, _ ->
                client.gateBundleRecipient(
                    recipientAccountId = invite.ownerAccountId,
                    recipientSignaturePubkey = invite.ownerSignaturePubkey,
                )
            }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (e: Throwable) {
            return InviteAddResult.Failed(message = e.message ?: "inviter identity refused")
        }
        if (!bound.isCurrent) return InviteAddResult.RequiresReauth
        val accountB64 = boundAccountB64 ?: return InviteAddResult.RequiresReauth
        val contact = PendingContact(
            accountIdHex = me.lakko.app.ds.hexOf(invite.ownerAccountId),
            signaturePubkeyB64 = base64Encode(invite.ownerSignaturePubkey),
            x25519PubkeyB64 = base64Encode(invite.ownerX25519Pubkey),
            displayName = invite.displayName.orEmpty(),
            username = invite.username.orEmpty(),
            addedAtMs = System.currentTimeMillis(),
            linkIdHex = me.lakko.app.ds.hexOf(invite.linkId),
        )
        // A live one-to-one with the inviter, when the visitor removed them
        // and the conversation remains (amended 2026-10-24): the inviter's
        // devices answer such a request by restoring, never by founding, so
        // nothing is pending; the inviter is put back once the request is
        // sent, and the page opens that chat.
        val liveGroupIdHex = withContext(Dispatchers.Default) { chatRepo?.liveOneToOneWith(invite.ownerSignaturePubkey) }
        if (liveGroupIdHex != null) {
            val restored = try {
                InviteRestoreOrder.run(
                    consumeLive = {
                        val consumed = try {
                            consumeInviteUse(invite, secrets.accountId)
                        } catch (e: Throwable) {
                            me.lakko.app.rethrowIfCancelled(e)
                            if (me.lakko.app.discovery.isDiscoveryRateLimited(e)) return InviteAddResult.RateLimited
                            if (me.lakko.app.anon.isAnonTokenQuotaExhausted(e)) return InviteAddResult.RateLimited
                            return InviteAddResult.Failed(message = e.message ?: "consume failed")
                        }
                        consumed !is me.lakko.app.discovery.ConsumeInviteResult.Exhausted &&
                            consumed !is me.lakko.app.discovery.ConsumeInviteResult.NotFound
                    },
                    sendRequest = {
                        if (!bound.isCurrent) return InviteAddResult.RequiresReauth
                        sendLinkAnswer(invite, secrets)
                    },
                    restore = {
                        if (!bound.isCurrent) return InviteAddResult.RequiresReauth
                        restoreRemovedContact(
                            signatureKey = invite.ownerSignaturePubkey,
                            accountId = invite.ownerAccountId,
                            groupIdHex = liveGroupIdHex,
                            nowMs = System.currentTimeMillis().toULong(),
                            createdAt = java.time.Instant.now().toString(),
                        )
                    },
                )
            } catch (e: Throwable) {
                me.lakko.app.rethrowIfCancelled(e)
                return InviteAddResult.Failed(message = e.message ?: "request send failed")
            }
            return when (restored) {
                InviteRestoreOrder.Outcome.DEAD -> InviteAddResult.Dead
                InviteRestoreOrder.Outcome.RESTORED -> InviteAddResult.Restored(liveGroupIdHex)
            }
        }
        val outcome = try {
            InviteAddOrder.run(
                // The ledger's soft use count. Used before by this account:
                // sent again, the inviter's device answers it either way.
                consumeLive = {
                    val consumed = try {
                        consumeInviteUse(invite, secrets.accountId)
                    } catch (e: Throwable) {
                        me.lakko.app.rethrowIfCancelled(e)
                        if (me.lakko.app.discovery.isDiscoveryRateLimited(e)) return InviteAddResult.RateLimited
                        if (me.lakko.app.anon.isAnonTokenQuotaExhausted(e)) return InviteAddResult.RateLimited
                        return InviteAddResult.Failed(message = e.message ?: "consume failed")
                    }
                    consumed !is me.lakko.app.discovery.ConsumeInviteResult.Exhausted &&
                        consumed !is me.lakko.app.discovery.ConsumeInviteResult.NotFound
                },
                recordPending = {
                    if (!bound.isCurrent) return InviteAddResult.RequiresReauth
                    updateInvited(accountB64) { PendingContacts.adding(contact, it) }
                },
                sendRequest = { sendLinkAnswer(invite, secrets) },
                forgetPending = {
                    updateInvited(accountB64) { list -> list.filterNot { it.accountIdHex == contact.accountIdHex } }
                },
            )
        } catch (e: Throwable) {
            me.lakko.app.rethrowIfCancelled(e)
            return InviteAddResult.Failed(message = e.message ?: "request send failed")
        }
        if (outcome == InviteAddOrder.Outcome.DEAD) return InviteAddResult.Dead
        return InviteAddResult.Added(contact)
    }

    /**
     * **Open chat** on the page of a person already in the contacts
     * (`docs/ux/contact-invite-link.md` section 3, amended 2026-10-24), in
     * the order [KnownContactAnswerOrder] pins: the link still carries its
     * answer, so an inviter who removed this account and sent a new link gets
     * it back. The ledger consume first; a dead link sends nothing; no
     * pending contact is written, since the inviter already is one. The page
     * opens the chat on [KnownContactAnswerResult.Sent] and
     * [KnownContactAnswerResult.Dead] alike.
     */
    public suspend fun answerAsKnownContact(invite: OpenedInvite): KnownContactAnswerResult {
        val secrets = authRepo.identitySecrets.value ?: return KnownContactAnswerResult.RequiresReauth
        val bound = sessionBinding.capture()
        return try {
            // The same pin question Add asks before anything is spent.
            useClientWith(secrets) { client, _ ->
                client.gateBundleRecipient(
                    recipientAccountId = invite.ownerAccountId,
                    recipientSignaturePubkey = invite.ownerSignaturePubkey,
                )
            }
            if (!bound.isCurrent) return KnownContactAnswerResult.RequiresReauth
            when (
                KnownContactAnswerOrder.run(
                    consumeLive = {
                        val consumed = consumeInviteUse(invite, secrets.accountId)
                        consumed !is me.lakko.app.discovery.ConsumeInviteResult.Exhausted &&
                            consumed !is me.lakko.app.discovery.ConsumeInviteResult.NotFound
                    },
                    sendRequest = {
                        if (!bound.isCurrent) return KnownContactAnswerResult.RequiresReauth
                        sendLinkAnswer(invite, secrets)
                    },
                )
            ) {
                KnownContactAnswerOrder.Outcome.SENT -> KnownContactAnswerResult.Sent
                KnownContactAnswerOrder.Outcome.DEAD -> KnownContactAnswerResult.Dead
            }
        } catch (e: Throwable) {
            me.lakko.app.rethrowIfCancelled(e)
            if (me.lakko.app.discovery.isDiscoveryRateLimited(e) || me.lakko.app.anon.isAnonTokenQuotaExhausted(e)) {
                KnownContactAnswerResult.RateLimited
            } else {
                KnownContactAnswerResult.Failed(message = e.message ?: "answer failed")
            }
        }
    }

    /** The link's ledger consume for this account (ADR-0079 section 5). */
    private suspend fun consumeInviteUse(
        invite: OpenedInvite,
        accountId: ByteArray,
    ): me.lakko.app.discovery.ConsumeInviteResult =
        authenticatedDiscoveryClient().use { discovery ->
            discovery.consumeInvite(invite.blobId, invite.capabilityBytes, accountId)
        }

    /**
     * The request carrying the link answer (ADR-0079 section 2): written to
     * the owner's closed mailbox under the link's grant (ADR-0060 section 6),
     * whose mailbox binding is verified against the key the request is sealed
     * to (ADR-0078). The inviter's device takes it by itself.
     */
    private suspend fun sendLinkAnswer(invite: OpenedInvite, secrets: me.lakko.app.auth.IdentitySecrets) {
        val linkReach = try {
            verifiedGrantDest(invite.linkGrant, invite.ownerSignaturePubkey, mailboxCrypto) { seed ->
                me.lakko.app.ds.DsReachAuth(me.lakko.app.ds.reachSigner(seed))
            }
        } catch (_: Throwable) {
            null
        }
        useClientWith(secrets) { client, sender ->
            client.sendContactRequestToBundle(
                senderSigningSeed = sender.signingSeed,
                senderAccountId = secrets.accountId,
                senderSignaturePubkey = sender.signaturePubkey,
                recipientAccountId = invite.ownerAccountId,
                recipientSignaturePubkey = invite.ownerSignaturePubkey,
                recipientX25519Pubkey = invite.ownerX25519Pubkey,
                introMessage = ByteArray(0),
                senderProfile = ownProfileForWire(secrets.accountId),
                replyGrant = reach()?.mintReply(invite.ownerSignaturePubkey),
                reach = linkReach,
                // ADR-0079 section 2 — the link answer.
                linkAnswer = uniffi.lakko_ffi_uniffi.FfiLinkAnswer(invite.linkId, invite.linkSecret),
            )
        }
    }

    /** Replace the bound account's pending contacts and store them. */
    private fun updateInvited(accountB64: String, change: (List<PendingContact>) -> List<PendingContact>) {
        synchronized(invitedLock) {
            if (boundAccountB64 != accountB64) return
            val next = change(mutableInvited.value)
            mutableInvited.value = next
            pendingContactsStore?.save(accountB64, next)
            pushPendingToDirectory()
        }
    }

    /** The pending contact [accountIdHex], while one is held. */
    public fun pendingContact(accountIdHex: String): PendingContact? =
        mutableInvited.value.firstOrNull { it.accountIdHex == accountIdHex }

    /** Keep one message written to a pending contact, in the held-send queue
     *  under its queue key. `false` when there is no such contact or it could
     *  not be kept (the caller keeps the words). */
    public fun queuePendingInvitedMessage(accountIdHex: String, text: String): Boolean {
        if (boundAccountB64 == null) return false
        val contact = pendingContact(accountIdHex) ?: return false
        return chatRepo?.queuePendingInvitedMessage(contact.queueKey, text) ?: false
    }

    /** What was written to the pending contact [accountIdHex], in order. */
    public fun pendingInvitedMessages(accountIdHex: String): List<me.lakko.app.chat.PersistedHeldSend> {
        val contact = pendingContact(accountIdHex) ?: return emptyList()
        return chatRepo?.pendingInvitedMessages(contact.queueKey).orEmpty()
    }

    /** B deletes a pending contact (UX section 7 item 4): the record goes,
     *  and so do the messages kept for it in the held-send queue. `false`
     *  when no such contact is held. */
    public fun removePendingContact(accountIdHex: String): Boolean {
        val accountB64 = boundAccountB64 ?: return false
        val contact = pendingContact(accountIdHex) ?: return false
        chatRepo?.clearPendingInvitedMessages(contact.queueKey)
        updateInvited(accountB64) { list -> list.filterNot { it.accountIdHex == accountIdHex } }
        return true
    }

    /**
     * ADR-0079 section 5 — the pending contacts whose one-to-one now exists
     * on this device: their messages move from the contact's held-send slot
     * into the queue of the one-to-one, in order, which sends them (a failed
     * send keeps the rest for the next attempt), and the record goes. Run
     * after every poll; the same hand-over as web and iOS.
     */
    public fun handOverJoinedPendingContacts() {
        val chat = chatRepo ?: return
        val accountB64 = boundAccountB64 ?: return
        if (mutableInvited.value.isEmpty()) return
        for (groupIdHex in chat.heldGroupIds()) {
            val people = chat.oneToOnePersonKeys(groupIdHex) ?: continue
            val contact = PendingContacts.forConversation(mutableInvited.value, people) ?: continue
            if (!chat.handOverPendingInvitedMessages(contact.queueKey, groupIdHex)) continue
            updateInvited(accountB64) { list -> list.filterNot { it.accountIdHex == contact.accountIdHex } }
            mutableHandedOver.value = mutableHandedOver.value + (contact.accountIdHex to groupIdHex)
        }
    }

    // ---- ADR-0079: the inviter's device (A) ---------------------------------

    /**
     * ADR-0079 section 3 (reworked 2026-10-17) - a fetched request carrying
     * a link answer is kept undecided (sealed on the device, not shown) and
     * decided after the batch by [decideLinkRequests]. Returns the requests
     * that answer no link, which go on as before.
     */
    private fun triageLinkAnswers(requests: List<IncomingContactRequest>): List<IncomingContactRequest> {
        if (requests.none { it.linkAnswer != null }) return requests
        val accountB64 = boundAccountB64 ?: return requests.filter { it.linkAnswer == null }
        val inbox = linkRequestInbox(accountB64)
        val out = ArrayList<IncomingContactRequest>(requests.size)
        for (req in requests) {
            val answer = req.linkAnswer
            if (answer == null) {
                out += req
                continue
            }
            // ADR-0060 section 6 - the requester's reply key opens their
            // mailbox for the winner's Welcome and ack: kept by the reach
            // manager now, so the stored row carries nothing secret but the
            // link answer, which is sealed.
            req.replyGrant?.let { grant ->
                if (grant.`class` == uniffi.lakko_ffi_uniffi.FfiReachClass.REPLY) {
                    reach()?.keepHeld(req.fromPubkey, grant, scopeHex = "")
                }
            }
            inbox.add(UndecidedLinkRequest(req.copy(linkAnswer = null, replyGrant = null), answer.linkId, answer.linkSecret))
        }
        return out
    }

    /**
     * ADR-0079 (reworked 2026-10-17) - decide every undecided request that
     * answers one of the account's invite links ([LinkClaims.step]): no
     * timer; a request waits, kept and unshown, until the synced state is
     * loaded, and only the device whose claim is first in the DeviceGroup
     * journal builds the one-to-one. One pass at a time.
     */
    public suspend fun decideLinkRequests() {
        if (!linkDecisionMutex.tryLock()) {
            // A pass is running; it re-reads the store and the record at
            // every row, so this trigger's change is seen by it or the next.
            linkPassAgain = true
            return
        }
        try {
            do {
                linkPassAgain = false
                decideLinkRequestsOnce()
            } while (linkPassAgain)
        } finally {
            linkDecisionMutex.unlock()
        }
    }

    @Volatile
    private var linkPassAgain: Boolean = false

    private suspend fun decideLinkRequestsOnce() {
        val accountB64 = boundAccountB64 ?: return
        val claims = linkClaimRecord(accountB64)
        val inbox = linkRequestInbox(accountB64)
        // Whether a device in the CURRENT roster claimed the request (a claim
        // only from a device since removed answers nothing). `false` while
        // the roster is not known.
        fun answeredElsewhere(key: String): Boolean {
            val seen = claims.seen(key)
            if (seen.isEmpty()) return false
            val roster = deviceGroupSync?.linkClaimRoster() ?: return false
            return runCatching {
                uniffi.lakko_ffi_uniffi.linkClaimAnsweredElsewhere(ffiClaims(seen), roster.second)
            }.getOrDefault(false)
        }
        for (row in inbox.rows()) {
            if (boundAccountB64 != accountB64) return
            val req = row.request
            val key = LinkClaims.requestKeyHex(row.linkId, req.fromPubkey)
            // ADR-0079 amendment of 2026-10-19 — the moment the request was
            // written decides the link's expiry: the time the DS stamped on
            // the row, kept with the request across a restart. A stamp that
            // does not read counts as written after any expiry (fail-closed).
            val pressedAtMs = req.createdAtMs?.toULong() ?: ULong.MAX_VALUE
            val verdict = deviceGroupSync?.linkAnswerVerdictAt(
                row.linkId, row.linkSecret, req.fromPubkey, pressedAtMs,
            )?.let {
                when (it) {
                    uniffi.lakko_ffi_uniffi.LinkAnswerVerdict.ACCEPT -> LinkClaims.Verdict.ACCEPT
                    uniffi.lakko_ffi_uniffi.LinkAnswerVerdict.ANSWERED -> LinkClaims.Verdict.ANSWERED
                    uniffi.lakko_ffi_uniffi.LinkAnswerVerdict.DEAD -> LinkClaims.Verdict.DEAD
                    uniffi.lakko_ffi_uniffi.LinkAnswerVerdict.UNKNOWN -> LinkClaims.Verdict.UNKNOWN
                }
            }
            // The live one-to-one this device holds with the requester, if
            // any: a requester who has one is put back on the contact list
            // with it rather than given a second conversation (amended
            // 2026-10-24).
            val liveGroupIdHex = chatRepo?.liveOneToOneWith(req.fromPubkey)
            var step = LinkClaims.step(
                claims.own(key),
                answeredElsewhere(key),
                verdict,
                claims.note(key) != null,
                liveOneToOne = liveGroupIdHex != null,
            )
            when (step) {
                LinkClaims.Step.WAIT -> continue
                LinkClaims.Step.RESTORE -> {
                    if (liveGroupIdHex == null) continue
                    // A person removed and invited again: back on the list
                    // with the conversation they already have. Nothing is
                    // founded or sent; a live link records its use, as the
                    // requester's consume did.
                    try {
                        restoreRemovedContact(
                            signatureKey = req.fromPubkey,
                            accountId = req.fromAccountId,
                            groupIdHex = liveGroupIdHex,
                            nowMs = System.currentTimeMillis().toULong(),
                            createdAt = java.time.Instant.now().toString(),
                        )
                        if (verdict == LinkClaims.Verdict.ACCEPT) {
                            deviceGroupSync?.recordLinkUse(row.linkId, req.fromPubkey)
                        }
                    } catch (e: Throwable) {
                        me.lakko.app.rethrowIfCancelled(e)
                        Log.w(TAG, "ADR-0079: link request not restored this pass (${e.javaClass.simpleName})")
                        continue
                    }
                    if (boundAccountB64 != accountB64) return
                    claims.forgetOwn(key)
                    inbox.remove(req.messageId)
                    continue
                }
                LinkClaims.Step.DROP -> {
                    claims.forgetOwn(key)
                    inbox.remove(req.messageId)
                    continue
                }
                LinkClaims.Step.REFUSE -> {
                    // The link does not count: no card. The requester is
                    // answered with a refusal and the request goes; a failed
                    // write keeps it for the next pass.
                    if (!refuseLinkRequest(req, row.linkId)) continue
                    if (boundAccountB64 != accountB64) return
                    claims.forgetOwn(key)
                    inbox.remove(req.messageId)
                    continue
                }
                else -> Unit
            }
            val sync = deviceGroupSync ?: continue
            try {
                if (step == LinkClaims.Step.POST_CLAIM) {
                    // The sealed claim is recorded before the publish:
                    // whether or not the publish answers, the receive loop
                    // recognises its row by those bytes and judges it at its
                    // position (it counts only when sealed at the journal's
                    // epoch there; a straddled claim is dropped and claimed
                    // again on the next pass).
                    sync.postLinkClaim(
                        row.linkId,
                        req.fromPubkey,
                        beforePublish = { claims.addOwnWire(key, it) },
                    )
                    step = LinkClaims.Step.DECIDE
                }
                if (step == LinkClaims.Step.DECIDE || step == LinkClaims.Step.RECHECK) {
                    // Every row up to (and past) this device's claims is
                    // applied once a poll that started after them read the
                    // journal to its head. A wait that times out (60 s) keeps
                    // the request undecided for the next pass. A re-check
                    // reads no journal: every claim of this device is known
                    // and only the roster can change.
                    if (step == LinkClaims.Step.DECIDE &&
                        (!sync.awaitJournalReadPastNow() || boundAccountB64 != accountB64)
                    ) {
                        continue
                    }
                    val roster = sync.linkClaimRoster() ?: continue
                    // A sealed claim no row carried did not land.
                    claims.dropUnlanded(key)
                    val decide = LinkClaims.claimsToDecide(
                        claims.own(key),
                        claims.seen(key),
                        LinkClaims.hex(roster.first),
                    )
                    when (
                        val outcome = uniffi.lakko_ffi_uniffi.linkClaimOutcome(
                            roster.first,
                            ffiClaims(decide),
                            roster.second,
                        )
                    ) {
                        is uniffi.lakko_ffi_uniffi.FfiLinkClaimOutcome.Lost -> {
                            // Another device's claim is first: it answers.
                            // Kept, unshown, until the register says it was
                            // answered; if that device is removed first, a
                            // re-check hands it on.
                            claims.setSettled(key)
                            continue
                        }
                        is uniffi.lakko_ffi_uniffi.FfiLinkClaimOutcome.NoWinner -> {
                            // No claim from the roster landed: post again
                            // next pass.
                            claims.forgetOwn(key)
                            continue
                        }
                        is uniffi.lakko_ffi_uniffi.FfiLinkClaimOutcome.Won -> {
                            claims.setWon(key, outcome.messageId.toLong())
                            step = LinkClaims.Step.BUILD
                        }
                    }
                }
                if (step == LinkClaims.Step.REFUSE) {
                    // Won, but the link was revoked before this device
                    // answered: the revocation wins (ADR-0079 amendment of
                    // 2026-10-19).
                    if (refuseLinkRequest(req, row.linkId) && boundAccountB64 == accountB64) {
                        claims.forgetOwn(key)
                        inbox.remove(req.messageId)
                    }
                    continue
                }
                if (step == LinkClaims.Step.BUILD) {
                    val answer = uniffi.lakko_ffi_uniffi.FfiLinkAnswer(row.linkId, row.linkSecret)
                    if (acceptThroughLink(req, answer, key, claims) && boundAccountB64 == accountB64) {
                        inbox.remove(req.messageId)
                        claims.forgetOwn(key)
                    }
                }
            } catch (e: Throwable) {
                me.lakko.app.rethrowIfCancelled(e)
                // A publish or a write that failed: the request stays
                // undecided and the next pass picks it up where its record
                // says.
                Log.w(TAG, "ADR-0079: link request not decided this pass (${e.javaClass.simpleName})")
            }
        }
    }

    /**
     * ADR-0079 amendment of 2026-10-19 — refuse a request whose invite link
     * no longer counts: a signed refusal to the requester's mailbox through
     * the reply grant the request carried (the ack's path). `true` when it
     * was written; `false` keeps the request for the next pass.
     */
    private suspend fun refuseLinkRequest(request: IncomingContactRequest, linkId: ByteArray): Boolean {
        val secrets = authRepo.identitySecrets.value ?: return false
        return try {
            useClientWith(secrets) { client, sender ->
                me.lakko.app.reach.writeRecheckingAddress(
                    write = {
                        val candidates = reach()
                            ?.reachAuthCandidates(request.fromPubkey, me.lakko.app.reach.WritePurpose.REPLY)
                            .orEmpty()
                        val send: suspend (MailboxDest?) -> Unit = { dest ->
                            client.sendLinkRefusal(
                                inviterSigningSeed = sender.signingSeed,
                                linkId = linkId,
                                originalRequest = request,
                                reach = dest ?: reach()?.mailboxPkFor(request.fromPubkey)?.let { MailboxDest(it, null) },
                            )
                        }
                        when (
                            val outcome = me.lakko.app.reach.firstAcceptedWrite(
                                candidates,
                                { dest: MailboxDest -> send(dest) },
                                ::isMailboxRefusal,
                            )
                        ) {
                            is me.lakko.app.reach.SignedWriteOutcome.Written -> Unit
                            is me.lakko.app.reach.SignedWriteOutcome.Exhausted ->
                                outcome.lastRefusal?.let { throw it } ?: send(null)
                        }
                    },
                    isRefusal = ::isMailboxRefusal,
                    recheck = { recheckAddress(request.fromPubkey, request.fromAccountId) },
                )
            }
            true
        } catch (e: Throwable) {
            me.lakko.app.rethrowIfCancelled(e)
            Log.w(TAG, "ADR-0079: link refusal not sent this pass (${e.javaClass.simpleName})")
            false
        }
    }

    /**
     * ADR-0079 amendment of 2026-10-19 — the inviter's app refused the
     * request this account sent through their link: the pending contact it
     * names and the messages held for it go, silently. Only a refusal signed
     * by the key the link named, over that link and this account
     * ([ownAccountKey]), removes anything. `true` when one was removed.
     */
    internal fun applyLinkRefusal(refusal: ByteArray, ownAccountKey: ByteArray): Boolean {
        val refused = PendingContacts.refusedBy(mutableInvited.value, refusal, ownAccountKey) { w, k, l, o ->
            uniffi.lakko_ffi_uniffi.verifyLinkRefusal(w, k, l, o)
        } ?: return false
        removePendingContact(refused.accountIdHex)
        runCatching { java.util.Base64.getDecoder().decode(refused.signaturePubkeyB64) }.getOrNull()?.let { key ->
            reach()?.retireRepliesTo(key)
        }
        return true
    }

    /** The claim decision's inputs in the binding's shape; an author whose
     *  hex does not decode is left out. */
    private fun ffiClaims(claims: List<LinkClaims.Observed>): List<uniffi.lakko_ffi_uniffi.FfiObservedLinkClaim> =
        claims.mapNotNull { claim ->
            val author = LinkClaims.bytes(claim.author) ?: return@mapNotNull null
            if (claim.id <= 0) return@mapNotNull null
            uniffi.lakko_ffi_uniffi.FfiObservedLinkClaim(claim.id.toULong(), author)
        }

    /**
     * The winning device's accept of a request sent through one of this
     * account's live invite links (ADR-0079 section 3, order of 2026-10-17,
     * [LinkClaims.runBuild]): its own founding carried on; else a
     * link-founded note from another device answered by writing the note's
     * Welcome to the requester (never founding); else found in ONE commit
     * with every other device of this account, record the founding, publish
     * the note, and only then write the Welcome to this account's own mailbox
     * and then the requester's. `false` when it did not complete; every
     * record stays, and the next pass takes it up where it stopped.
     */
    private suspend fun acceptThroughLink(
        request: IncomingContactRequest,
        answer: uniffi.lakko_ffi_uniffi.FfiLinkAnswer,
        key: String,
        claims: LinkClaimRecord,
    ): Boolean {
        val chat = chatRepo ?: return false
        val sync = deviceGroupSync ?: return false
        val fromAccountId = request.fromAccountId ?: return false
        // ADR-0060 §6 — the requester's reply key opens their mailbox for the
        // Welcome and the ack; kept before anything is written there.
        request.replyGrant?.let { grant ->
            if (grant.`class` == uniffi.lakko_ffi_uniffi.FfiReachClass.REPLY) {
                reach()?.keepHeld(request.fromPubkey, grant, scopeHex = "")
            }
        }
        val deps = object : LinkClaims.BuildDeps {
            override fun founded(): LinkClaims.Founded? = claims.own(key)?.founded

            override fun note(): LinkClaims.HeldNote? = claims.note(key)

            override suspend fun prepare(): LinkClaims.Founded =
                chat.prepareLinkOneToOne(fromAccountId, request.fromPubkey)

            override fun record(founded: LinkClaims.Founded): Boolean = claims.setFounded(key, founded)

            override suspend fun postNote(founded: LinkClaims.Founded): Boolean {
                // Published once (a sealed note already recorded is not
                // published again), then judged at its row: it counts only
                // when sealed at the journal's epoch there.
                val own = claims.own(key)
                if (own?.noteConfirmed == true) return true
                if (own?.noteWire == null) {
                    val groupId = LinkClaims.bytes(founded.groupIdHex) ?: error("the founding is malformed")
                    sync.postLinkFounded(
                        answer.linkId,
                        request.fromPubkey,
                        groupId,
                        java.util.Base64.getDecoder().decode(founded.welcomeB64),
                        beforePublish = { claims.setNoteWire(key, it) },
                    )
                }
                if (!sync.awaitJournalReadPastNow()) return false
                if (claims.own(key)?.noteConfirmed == true) return true
                // Read past it and it did not count (straddled by a commit)
                // or no row carried it: posted again at the next pass.
                claims.dropUnlandedNote(key)
                return false
            }

            override suspend fun hold(founded: LinkClaims.Founded): Boolean =
                chat.holdLinkOneToOne(founded.groupIdHex)

            override suspend fun deliver(founded: LinkClaims.Founded) {
                chat.deliverLinkWelcome(founded, fromAccountId)
            }

            override suspend fun forward(note: LinkClaims.HeldNote): String? =
                chat.forwardLinkWelcome(note, request.fromPubkey, fromAccountId)

            override suspend fun finish(groupIdHex: String?) {
                var added = false
                mutableState.update { state ->
                    added = false
                    if (state.accepted.any { it.request.fromPubkey.contentEquals(request.fromPubkey) }) return@update state
                    added = true
                    val accepted = AcceptedContact(request = request.copy(linkAnswer = null), groupIdHex = groupIdHex)
                    state.copy(
                        accepted = state.accepted + accepted,
                        pending = withoutAcceptedSenders(state.pending, listOf(accepted)),
                    )
                }
                if (added) {
                    persistAccepted()
                    persistInboxPending()
                }
                try {
                    sendContactAck(request)
                } catch (e: Throwable) {
                    me.lakko.app.rethrowIfCancelled(e)
                    // Best effort, as on a manual accept.
                }
                sync.recordLinkUse(answer.linkId, request.fromPubkey)
            }
        }
        return try {
            LinkClaims.runBuild(deps)
        } catch (e: Throwable) {
            me.lakko.app.rethrowIfCancelled(e)
            Log.w(TAG, "ADR-0079 invite-link accept did not complete; retried at the next pass")
            false
        }
    }

    /**
     * ADR-0079 section 4 — after every poll: retry the ledger-row deletions
     * this device owes (it revoked the link and the ledger was unreachable,
     * or the app stopped in between), and drop this device's local traces of
     * the links another device of the account revoked. The revoking device
     * deletes the row itself, so this sends the ledger nothing for those; the
     * one exception is a synced record with no revocation seed (written
     * before the 2026-10-17 rework), whose row this device deletes with the
     * seed of its own record. See [LinkRowSweep].
     */
    public suspend fun revokeLinksRevokedElsewhere() {
        val accountB64 = boundAccountB64 ?: return
        val revoked = deviceGroupSync?.linkRecords?.value.orEmpty()
            .filter { it.revoked && it.kind == uniffi.lakko_ffi_uniffi.LinkRecordKind.CONTACT && it.ledgerId.isNotEmpty() }
            .map { LinkRowSweep.RevokedLink(it.ledgerId.copyOf(), it.revocationSeed.copyOf()) }
        if (revoked.isEmpty() && linkRowDeleter.owed.slugs(accountB64).isEmpty()) return
        val report = LinkRowSweep.run(
            accountIdB64 = accountB64,
            revoked = revoked,
            ownSeed = { slug -> ownLinkStore.get(accountB64, OwnLink.KIND_CONTACT, slug)?.seed() },
            deleter = linkRowDeleter,
            delete = ::deleteLinkLedgerRow,
            forget = { blobId -> forgetLinkTraces(accountB64, blobId) },
        )
        for (link in revoked) link.recordSeed.fill(0)
        if (report.unreachable > 0) {
            Log.w(TAG, "invite links: ${report.unreachable} ledger row deletion(s) still owed")
        }
    }

    /** The ledger call of a contact link's row deletion. */
    private suspend fun deleteLinkLedgerRow(
        blobId: ByteArray,
        seed: ByteArray,
    ): me.lakko.app.discovery.RevokeInviteResult =
        authenticatedDiscoveryClient().use { discovery -> discovery.revokeInvite(blobId, seed) }

    /** Drop this device's local traces of a contact link whose ledger row is
     *  dealt with: its own record (seed), the link's reach key, its secret
     *  and its session row. Idempotent; touches nothing that is not there. */
    private suspend fun forgetLinkTraces(accountB64: String, blobId: ByteArray) {
        val slug = base64UrlEncodeNoPad(blobId)
        if (ownLinkStore.get(accountB64, OwnLink.KIND_CONTACT, slug) != null) {
            ownLinkStore.remove(accountB64, OwnLink.KIND_CONTACT, slug)
        }
        // ADR-0060 §6 — the link's key comes off the mailbox (no-op when this
        // device never registered it).
        reach()?.retireLinkByBlob(blobId)
        // UX 5.3 — a revoked link's secret leaves with it.
        if (inviteSecretStore.get(accountB64, slug) != null) inviteSecretStore.remove(accountB64, slug)
        val blobIdList = blobId.toList()
        if (mutableState.value.outstandingInvites.any { it.blobId?.toList() == blobIdList }) {
            mutableState.update { state ->
                state.copy(outstandingInvites = state.outstandingInvites.filterNot { it.blobId?.toList() == blobIdList })
            }
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
     * The defaults are 7-day expiry and single-use (`max_uses = 0`);
     * the Invite links sheet's form passes the owner's choice of label,
     * expiry and uses. [expiryMillisFromNow] `null` means the link never
     * expires: the wire expiry is then the `NEVER_EXPIRES` sentinel `0`,
     * in the composed capability and in the ledger upload alike. Always
     * non-bootstrap-referral (this flow connects existing users).
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
        expiryMillisFromNow: Long? = DEFAULT_INVITE_EXPIRY_MS,
        maxUses: UInt = SINGLE_USE,
        bootstrapReferral: Boolean = false,
    ): GeneratedInvite {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(
                IllegalStateException(
                    "no identity secrets — please re-enter your password",
                ),
            )
        // `0` is the NEVER_EXPIRES sentinel (a never-expiring link).
        val expiryMs = (
            if (expiryMillisFromNow == null) 0L else System.currentTimeMillis() + expiryMillisFromNow
            ).toULong()

        val bound = sessionBinding.capture()
        return me.lakko.app.auth.SessionSecretWipe.identity(secrets.accountIdentitySeed).use { identity ->
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
                    // ADR-0079 section 1 — the page the invited person opens
                    // names this account, signed and sealed in the capability.
                    inviterDisplayName = ownProfileForWire(secrets.accountId)?.displayName,
                    inviterUsername = ownProfileForWire(secrets.accountId)?.username,
                )
            } catch (e: Throwable) {
                throw ContactsError.wrap(e)
            }
            // Sealed links (2026-09-28): the ledger stores only the sealed
            // capability; the secret rides in the fragment. Any failed upload,
            // a network one included, is thrown and mints no link (the inline
            // form was removed on 2026-10-19).
            // ADR-0078 section 10: the ledger row carries a key minted for
            // this link alone, never the account key; the seed stays on this
            // device with what the list shows.
            val (revocationSeed, revocationPubkey) = OwnLinks.freshRevocationKey()
            val upload = me.lakko.app.discovery.SealedInviteUpload(
                sealed = mint.sealed,
                revocationPubkey = revocationPubkey,
                expiryMs = expiryMs.toLong(),
                maxUses = maxUses.toLong(),
            )
            val blobId: ByteArray = try {
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
                // ADR-0060 §6 — a failed upload shares no link, so its key
                // comes off the mailbox again.
                manager.retireLink(linkId)
                throw ContactsError.wrap(e)
            }
            // The link exists on the server; the reach keys, the secret store
            // and the list below are the minting account's.
            bound.ensureCurrent()
            manager.noteLinkBlob(linkId, blobId)

            val slug = base64UrlEncodeNoPad(blobId)
            val url = me.lakko.app.discovery.composeSealedInviteUrl(
                origin = me.lakko.app.config.ApiEndpoints.webOrigin,
                prefix = "/i/",
                blobId = blobId,
                linkSecret = mint.linkSecret,
            )
            // UX 5.3 — keep the secret of the link so this device can copy
            // and share it later, after a restart included.
            run {
                inviteSecretStore.put(
                    base64Encode(secrets.accountId),
                    slug,
                    base64UrlEncodeNoPad(mint.linkSecret),
                )
                // This device's record is the only list of the account's
                // links and the only key that revokes this one.
                ownLinkStore.put(
                    base64Encode(secrets.accountId),
                    OwnLinks.record(
                        kind = OwnLink.KIND_CONTACT,
                        blobId = blobId,
                        seed = revocationSeed,
                        label = label,
                        expiryMs = expiryMs.toLong(),
                        maxUses = maxUses.toLong(),
                        bootstrapReferral = bootstrapReferral,
                    ),
                )
            }
            // ADR-0079 (reworked 2026-10-17) — the ledger row's revocation
            // seed rides the synced record, so a revoke on any device of the
            // account deletes the row.
            val recordSeed = revocationSeed.copyOf()
            revocationSeed.fill(0)
            // ADR-0079 section 4 — the link's record goes to the synced
            // register, so every device of the account accepts a request
            // through it, lists it and can revoke it.
            val recorded = deviceGroupSync?.setLinkRecord(
                uniffi.lakko_ffi_uniffi.LinkRecord(
                    hlc = 0uL,
                    linkId = linkId,
                    linkSecret = mint.linkSecret,
                    kind = uniffi.lakko_ffi_uniffi.LinkRecordKind.CONTACT,
                    ledgerId = blobId,
                    revocationSeed = recordSeed,
                    expiryMs = expiryMs,
                    maxUses = maxUses,
                    uses = 0u,
                    revoked = false,
                    mintedAtMs = System.currentTimeMillis().toULong(),
                    label = label,
                    answered = emptyList(),
                    shared = false,
                    editable = false,
                ),
            ) ?: false
            if (!recorded) {
                // A link this device would not remember after a restart is not
                // handed out: every request through it would wait for ever.
                try {
                    revokeInviteLink(blobId)
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (_: Throwable) {
                    // The row stays on the server unanswered: nobody holds its secret.
                }
                throw ContactsError.wrap(IllegalStateException("invite link not recorded"))
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
     * Revoke an outstanding invite link: marked revoked in the synced
     * register, its ledger row deleted with `DELETE /discovery/invite/{id}`
     * (signed by the link's own revocation seed, ADR-0078 section 10), and
     * this device's traces of it dropped.
     *
     * ADR-0079 section 4 — the ledger holds one row per link and only the
     * device that revokes deletes it, once ([LinkRowRevoke]): the deletion is
     * marked owed BEFORE the register changes (cleared again if that fails),
     * a call while the same deletion runs joins it, and an unreachable ledger
     * leaves it owed for [revokeLinksRevokedElsewhere] to retry after every
     * poll. The seed comes from the register's record (any device holds it)
     * or, for a record written before the 2026-10-17 rework, from this
     * device's own record.
     *
     * Returns the ledger's answer; `Revoked` too when another call on this
     * device already got it. Both `Revoked` and `NotFoundOrNotOwner` (no row:
     * gone, expired, deleted earlier) drop the local traces. With no seed at
     * all the link is only revoked in the register and the answer is
     * `NotFoundOrNotOwner`. An unreachable ledger throws [ContactsError] (the
     * row stays listed until the retry gets an answer).
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
        val accountB64 = base64Encode(secrets.accountId)
        val ownSlug = base64UrlEncodeNoPad(blobId)
        val sync = deviceGroupSync
        val record = sync?.linkRecords?.value?.firstOrNull {
            it.kind == uniffi.lakko_ffi_uniffi.LinkRecordKind.CONTACT && it.ledgerId.contentEquals(blobId)
        }
        val recordSeed = record?.revocationSeed?.takeIf { it.size == 32 }?.copyOf()
        val ownSeed = ownLinkStore.get(accountB64, OwnLink.KIND_CONTACT, ownSlug)?.seed()
        val outcome = try {
            LinkRowRevoke.run(
                accountIdB64 = accountB64,
                blobId = blobId,
                recordSeed = recordSeed,
                ownSeed = ownSeed,
                // ADR-0079 section 4 — marked revoked in the synced register:
                // no device of the account takes a request through the link
                // from now on, whatever the ledger answers.
                revokeRegister = if (record != null && sync != null) {
                    { sync.revokeLinkRecord(record.linkId) }
                } else {
                    null
                },
                deleter = linkRowDeleter,
                delete = ::deleteLinkLedgerRow,
                forget = { id -> forgetLinkTraces(accountB64, id) },
            )
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            throw ContactsError.wrap(e)
        } finally {
            recordSeed?.fill(0)
            ownSeed?.fill(0)
        }
        return when (outcome) {
            is LinkRowDeleter.Outcome.Deleted -> outcome.result
            LinkRowDeleter.Outcome.AlreadyDone -> me.lakko.app.discovery.RevokeInviteResult.Revoked
            LinkRowDeleter.Outcome.NoKey -> me.lakko.app.discovery.RevokeInviteResult.NotFoundOrNotOwner
            LinkRowDeleter.Outcome.Unreachable ->
                throw ContactsError.wrap(IllegalStateException("invite ledger unreachable; the deletion is retried"))
        }
    }

    /**
     * The account's synced link-record register (ADR-0079 section 4), every
     * device's links with `shared` / `editable`; empty while the device-group
     * sync is not wired. The Invite links sheet joins its rows with it by
     * ledger id.
     */
    public fun inviteLinkRecords(): kotlinx.coroutines.flow.StateFlow<List<uniffi.lakko_ffi_uniffi.LinkRecord>> =
        deviceGroupSync?.linkRecords ?: kotlinx.coroutines.flow.MutableStateFlow(emptyList())

    /**
     * Copy (ADR-0079 amendment 2026-10-26): mark the link [linkId] copied in
     * the synced state and send its record to the account's other devices,
     * before the text reaches the clipboard. `false` when nothing changed
     * (already copied) or the register is not open.
     */
    public suspend fun markInviteLinkShared(linkId: ByteArray): Boolean =
        deviceGroupSync?.markLinkShared(linkId) ?: false

    /**
     * The live `/i/` links this device minted and keeps a record of
     * (ADR-0078 section 10). No ledger row names its owner, so there is no
     * server list: a link minted on another device is listed there.
     *
     * Throws [ContactsError] when the session has no identity secrets
     * (caller should prompt the user to re-enter their password).
     */
    public suspend fun listActiveInvites(): me.lakko.app.discovery.ListInvitesResult {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(
                IllegalStateException(
                    "no identity secrets — please re-enter your password",
                ),
            )
        val nowMs = System.currentTimeMillis()
        val accountB64 = base64Encode(secrets.accountId)
        // ADR-0079 section 4 — the synced register lists every device's links
        // with their use counts. The link's secret is in the record, so Copy
        // works on every device.
        val sync = deviceGroupSync
        if (sync != null && sync.ready.value) {
            val rows = LinkRecords.listedContactLinks(sync.linkRecords.value, nowMs.toULong()).map { record ->
                val slug = base64UrlEncodeNoPad(record.ledgerId)
                if (inviteSecretStore.all(accountB64)[slug] == null) {
                    inviteSecretStore.put(accountB64, slug, base64UrlEncodeNoPad(record.linkSecret))
                }
                me.lakko.app.discovery.ActiveInvite(
                    blobIdSlug = slug,
                    expiryMs = record.expiryMs.toLong(),
                    maxUses = record.maxUses.toLong(),
                    usedCount = record.uses.toLong(),
                    label = record.label,
                    bootstrapReferral = false,
                    createdAtMs = record.mintedAtMs.toLong(),
                )
            }
            return me.lakko.app.discovery.ListInvitesResult.List(rows)
        }
        val rows = ownLinkStore.all(accountB64)
            .filter { it.kind == OwnLink.KIND_CONTACT && !it.isExpired(nowMs) }
            .map { link ->
                me.lakko.app.discovery.ActiveInvite(
                    blobIdSlug = link.slug,
                    expiryMs = link.expiryMs,
                    maxUses = link.maxUses,
                    usedCount = 0,
                    label = link.label,
                    bootstrapReferral = link.bootstrapReferral,
                    createdAtMs = link.mintedAtMs,
                )
            }
        return me.lakko.app.discovery.ListInvitesResult.List(rows)
    }

    /**
     * Revoke every live link this device keeps a record of, each with its own
     * revocation seed (ADR-0078 section 10). Account deletion runs this
     * first, while the session still works. Best effort; returns how many
     * could not be reached.
     */
    public suspend fun revokeAllOwnLinks(): Int {
        val secrets = authRepo.identitySecrets.value ?: return 0
        return authenticatedDiscoveryClient().use { discovery ->
            OwnLinks.revokeAll(base64Encode(secrets.accountId), discovery, ownLinkStore)
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
            sessionBinding.advance()
            boundAccountB64 = null
            removedContactKeys = emptySet()
            // Wipe any decrypted intro plaintext on the way out.
            // Best-effort: the `String` contents may still live in
            // heap if some collector cached them, but our reference
            // drops here.
            mutableState.value = ContactsState()
            mutableInvited.value = emptyList()
            mutableHandedOver.value = emptyMap()
        }
        // The name directory stops carrying this account's renames, and the
        // names it learned stop resolving.
        pushRenamesToDirectory()
        profileDirectory?.onAccountChanged()
    }

    /**
     * ADR-0074 part 3, Stage A - read the mailbox of each of this account's
     * own previous identities and join the groups its Welcomes invite this
     * device to. Section 3.5a rule 4: senders that observed the rotation
     * address the NEW mailbox, so this only catches what was sent before
     * they did, and only for
     * [me.lakko.app.reach.PREDECESSOR_MAILBOX_READ_MS] after adopting. Only Welcomes are acted on: a contact request sealed to a
     * previous key is left where it is (the sender's client re-fetches the
     * identity bundle and follows the proof). Best-effort; the cursor of
     * each mailbox only moves forward.
     */
    private suspend fun pollPredecessorWelcomes() {
        val chat = chatRepo ?: return
        val predecessors = try {
            authRepo.ownPredecessors()
        } catch (_: Throwable) {
            emptyList()
        }
        if (predecessors.isEmpty()) return
        val nowMs = System.currentTimeMillis()
        try {
            for (previous in predecessors) {
                // ADR-0074 section 3.5a rule 4 - read for 30 days after this
                // device adopted the successor, then left for good. Nothing
                // is ever written or registered there.
                if (!me.lakko.app.reach.predecessorMailboxIsRead(previous.supersededAtMs, nowMs)) continue
                try {
                    clientFactory().use { client ->
                        Identity.fromSeed(previous.phraseSeed).use { identity ->
                            val secret = identity.x25519SecretKey()
                            try {
                                val result = client.pollStrangerInbox(
                                    // ADR-0078: the predecessor's mailbox derives
                                    // from the predecessor's own seed.
                                    own = NativeOwnMailbox.of(identity),
                                    myX25519Secret = secret,
                                    after = previous.mailboxCursor,
                                    waitSecs = 0,
                                    mailboxSigner = me.lakko.app.ds.FfiDsJournalSigner(
                                        uniffi.lakko_ffi_uniffi.dsMailboxSigner(identity),
                                    ),
                                )
                                for (event in result.events) {
                                    val welcome = event as? IncomingMailboxEvent.Welcome ?: continue
                                    try {
                                        chat.joinGroupFromWelcome(welcome.welcomeBytes)
                                    } catch (cancel: kotlinx.coroutines.CancellationException) {
                                        throw cancel
                                    } catch (cause: Throwable) {
                                        if (!me.lakko.app.chat.isWelcomeForAnotherDevice(cause)) {
                                            Log.w(TAG, "a Welcome in a previous mailbox was not joined: ${cause.javaClass.simpleName}")
                                        }
                                    }
                                }
                                authRepo.advancePredecessorMailboxCursor(previous.signaturePubkey, result.cursor)
                            } finally {
                                secret.fill(0)
                            }
                        }
                    }
                } catch (cancel: kotlinx.coroutines.CancellationException) {
                    throw cancel
                } catch (cause: Throwable) {
                    Log.w(TAG, "a previous identity's mailbox was not read: ${cause.javaClass.simpleName}")
                }
            }
        } finally {
            predecessors.forEach { it.zero() }
        }
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
            me.lakko.app.auth.SessionSecretWipe.identity(secrets.accountIdentitySeed).use { identity ->
                val sender = SenderMaterial(
                    signingSeed = identity.ed25519Seed(),
                    signaturePubkey = identity.signaturePublicKey(),
                    x25519Secret = identity.x25519SecretKey(),
                    x25519Pubkey = identity.x25519PublicKey(),
                    mailboxSigner = me.lakko.app.ds.FfiDsJournalSigner(
                        uniffi.lakko_ffi_uniffi.dsMailboxSigner(identity),
                    ),
                    ownMailbox = NativeOwnMailbox.of(identity),
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
        /** ADR-0053 / ADR-0060 — the mailbox-key proof of this account's mailbox. */
        val mailboxSigner: me.lakko.app.ds.DsJournalSigner,
        /** ADR-0078 — this account's own mailbox: its address and opener. */
        val ownMailbox: OwnMailbox,
    )
}

// =============================================================================
// State shape
// =============================================================================

/** Log tag. */
private const val TAG = "ContactsRepository"

/**
 * What went wrong on the inbox, held as a kind so the screen renders the
 * matching sentence in the interface language (ADR-0061 section 6). The two
 * used to share one `String` slot that the screen rendered as the poll
 * failure whatever it held, so a Welcome that could not be joined read
 * "Couldn't check for new contact requests".
 */
public enum class InboxError(
    /** The catalog key the contacts screen renders. */
    public val key: String,
) {
    /** The inbox poll failed; the app retries by itself. */
    POLL_FAILED("contacts.mailbox.poll_failed"),

    /** A Welcome from a peer arrived but could not be joined on this device:
     *  a chat someone started that will never appear unless they add this
     *  user again. */
    WELCOME_JOIN_FAILED("contacts.mailbox.chat_join_failed"),
}

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
    /** Last inbox error, for inline display, as a kind (see
     *  [InboxError]). `null` clears on the next successful call. */
    val lastError: InboxError? = null,
)

/** What opening a link with no session gave the signed-out invite page
 *  (UX section 3, "Signed out", amended 2026-10-26). */
public sealed interface InvitePreviewResult {
    public data class Preview(
        val displayName: String?,
        val username: String?,
        val label: String,
        val ownerAccountId: ByteArray,
    ) : InvitePreviewResult {
        /** The name the link states ([PendingContacts.statedName]). */
        val statedName: String? get() = PendingContacts.statedName(displayName, username)

        override fun equals(other: Any?): Boolean =
            other is Preview && displayName == other.displayName && username == other.username &&
                label == other.label && ownerAccountId.contentEquals(other.ownerAccountId)

        override fun hashCode(): Int = ownerAccountId.contentHashCode()
    }

    /** Revoked, expired, used up, unknown or damaged: one state. */
    public data object Dead : InvitePreviewResult
    public data object RateLimited : InvitePreviewResult
    public data object Failed : InvitePreviewResult
}

/** What opening an `/i/` link gave the invite page (ADR-0079). */
public sealed interface InviteOpenResult {
    public data class Opened(val invite: OpenedInvite) : InviteOpenResult

    /** Revoked, expired, used up, unknown or damaged: one state on the page. */
    public data object Dead : InviteOpenResult

    /** The link is this account's own. */
    public data object Own : InviteOpenResult

    /** The inviter is already an established contact: the page offers Open
     *  chat (the one-to-one when this device holds it, else the contact
     *  [routeKey] names). */
    public data class KnownContact(
        val routeKey: String,
        val groupIdHex: String?,
        val statedName: String?,
        /** The opened link, whose answer Open chat still delivers (amended
         *  2026-10-24); `null` only where no link was opened. */
        val invite: OpenedInvite? = null,
    ) : InviteOpenResult

    /** The visitor already pressed Add on one of the inviter's links and the
     *  answer has not arrived (amended 2026-10-24): "already in your
     *  contacts", and Open chat opens the waiting chat and sends nothing.
     *  [invite] is kept so the page can ask again when the lists change. */
    public data class PendingInviter(
        val accountIdHex: String,
        val statedName: String?,
        val invite: OpenedInvite? = null,
    ) : InviteOpenResult

    public data object RateLimited : InviteOpenResult
    public data object RequiresReauth : InviteOpenResult
    public data class Failed(val message: String) : InviteOpenResult
}

/** What **Add** did. */
public sealed interface InviteAddResult {
    /** The inviter is a pending contact now; the page closes into the chat. */
    public data class Added(val contact: PendingContact) : InviteAddResult

    /** A removed inviter is back on the list with the live one-to-one
     *  [groupIdHex] (amended 2026-10-24); the page closes into that chat. */
    public data class Restored(val groupIdHex: String) : InviteAddResult

    /** Used up or gone since the page opened. */
    public data object Dead : InviteAddResult
    public data object RateLimited : InviteAddResult
    public data object RequiresReauth : InviteAddResult

    /** A network or server failure: the page stays, Add comes back. */
    public data class Failed(val message: String) : InviteAddResult
}

/** What **Open chat** on the page of a known contact did (amended 2026-10-24). */
public sealed interface KnownContactAnswerResult {
    /** The request carrying the link answer is written; the chat opens. */
    public data object Sent : KnownContactAnswerResult

    /** The link no longer works: nothing was sent; the chat opens anyway. */
    public data object Dead : KnownContactAnswerResult
    public data object RateLimited : KnownContactAnswerResult
    public data object RequiresReauth : KnownContactAnswerResult

    /** A network or server failure: the page stays, Open chat comes back. */
    public data class Failed(val message: String) : KnownContactAnswerResult
}

/** An opened `/i/` link: what the page shows and what **Add** needs. */
public class OpenedInvite(
    /** The ledger id. */
    public val blobId: ByteArray,
    public val capabilityBytes: ByteArray,
    public val ownerAccountId: ByteArray,
    public val ownerSignaturePubkey: ByteArray,
    public val ownerX25519Pubkey: ByteArray,
    public val linkId: ByteArray,
    /** SECRET: the link answer's secret. */
    public val linkSecret: ByteArray,
    /** SECRET: the link's reach grant, used for this one write. */
    public val linkGrant: uniffi.lakko_ffi_uniffi.FfiReachGrant,
    public val displayName: String?,
    public val username: String?,
    public val label: String,
) {
    /** The name the link states: the display name, else `@username`, else
     *  `null` (the catalog's "this person"). */
    public val statedName: String? get() = PendingContacts.statedName(displayName, username)

    override fun equals(other: Any?): Boolean =
        other is OpenedInvite && linkId.contentEquals(other.linkId) &&
            ownerAccountId.contentEquals(other.ownerAccountId)

    override fun hashCode(): Int = linkId.contentHashCode()

    override fun toString(): String = "OpenedInvite(<redacted>)"
}

/** An invite link the owner has just generated this session. */
public data class GeneratedInvite(
    /** Raw 6-byte server-assigned ledger id (the same value
     *  `revokeInviteLink` takes). */
    val blobId: ByteArray,
    /** The slug portion of [url]: base64url of [blobId]. */
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
    /** The intro the request carried, kept (stored with the entry) so
     *  the request can be sent again from a rotated account key (ADR-0074
     *  section 3.5a rule 5). */
    val introMessage: ByteArray = ByteArray(0),
    /** When this device sent it (unix ms). Not part of the entry's
     *  identity: the Delivery Service row id is. */
    val sentAtMs: Long = System.currentTimeMillis(),
    /** ADR-0084 — the recipient's signature key (standard base64), so the
     *  Sent row shows their photo when this device holds one. */
    val recipientKeyB64: String? = null,
    /** The name the person was found under. Not part of the entry's identity. */
    val recipientName: String? = null,
) {
    override fun equals(other: Any?): Boolean =
        other is SentContactRequest &&
            recipientUsername == other.recipientUsername &&
            recipientAccountIdB64 == other.recipientAccountIdB64 &&
            messageId == other.messageId &&
            createdAt == other.createdAt &&
            introMessage.contentEquals(other.introMessage)

    override fun hashCode(): Int = 31 * recipientAccountIdB64.hashCode() + messageId.hashCode()
}

/**
 * ADR-0074 section 3.5a rule 5 - what a failed re-send of an unanswered
 * request means: the person's mailbox refused the write, their username or
 * bundle is gone, or their keys changed without a proof - the spent entry is
 * dropped; anything else (the network) keeps it and the step owed.
 */
internal fun requestResendOutcome(cause: Throwable): me.lakko.app.reach.RequestResend = when {
    cause is SupersessionRefused -> me.lakko.app.reach.RequestResend.GONE
    cause is ContactsError && cause.variant !is ContactsError.Variant.Wrapped -> me.lakko.app.reach.RequestResend.GONE
    isMailboxRefusal(cause) -> me.lakko.app.reach.RequestResend.GONE
    else -> me.lakko.app.reach.RequestResend.FAILED
}

/** Whether [cause] is the Delivery Service's `401` to a mailbox write
 *  (ADR-0060 section 2), bare or wrapped by the contacts client. */
internal fun isMailboxRefusal(cause: Throwable): Boolean = when (cause) {
    is me.lakko.app.ds.DsReachRefusedError -> true
    is ContactsError -> when (val variant = cause.variant) {
        ContactsError.Variant.ReachRefused -> true
        is ContactsError.Variant.Wrapped -> variant.cause is me.lakko.app.ds.DsReachRefusedError
        else -> false
    }
    else -> false
}

/**
 * ADR-0074 section 3.5a rule 5 - the ledger ids (base64url slugs) the
 * rotating device tries to revoke: what this device recorded for links it
 * minted, each once. ADR-0078 section 10: there is no owner list to add the
 * other devices' links from.
 */
internal fun spentLinkSlugs(recorded: List<String>): List<String> = recorded.distinct()
