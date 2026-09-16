// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.auth.base64Encode
import me.lakko.app.profile.toFfi
import me.lakko.app.auth.base64UrlDecodeNoPad
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
 * **No persistence yet** — pending requests + sent log live in
 * memory only. They drop on process death; on next launch the
 * receiver re-polls the stranger inbox from cursor `0` and
 * rebuilds the list. Room / DataStore-backed persistence lands
 * with 1.15f's chat surface (which needs MLS-state storage
 * anyway, so the two layers can share the same DB).
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
    private val clientFactory: () -> ContactsClient = {
        ContactsClient(
            // Non-null by construction (ContactsClient requires a store):
            // persistent PeerPinStore in production, ephemeral
            // InMemoryPinStore when no Context is supplied (tests). The
            // verify gate is therefore never silently disabled.
            pinStore = appContext?.let { PeerPinStore(it) } ?: InMemoryPinStore(),
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
        // Rehydrate the persisted accepted list (ADR-0021); only the
        // accepted list is persisted, pending re-polls from cursor 0.
        MutableStateFlow(
            ContactsState(
                accepted = appContext?.let { AcceptedContactsStore.load(it) } ?: emptyList(),
            ),
        )

    public val state: StateFlow<ContactsState> = mutableState.asStateFlow()

    init {
        // ADR-0050 section 2.9 — a pairing state bundle's contacts land in the
        // live accepted list (and from there the store), never beside it: a
        // store-only write would be overwritten by the next [persistAccepted].
        if (appContext != null) {
            me.lakko.app.devices.StateBundle.contactsSink = { incoming -> mergeAccepted(incoming) }
        }
    }

    /** Add the [incoming] contacts not already accepted (by signature key) and
     *  persist. Returns how many were added. */
    public fun mergeAccepted(incoming: List<AcceptedContact>): Int {
        var added = 0
        mutableState.update { state ->
            val known = state.accepted.map { it.request.fromPubkey.toList() }.toHashSet()
            val fresh = incoming.filter { known.add(it.request.fromPubkey.toList()) }
            added = fresh.size
            if (fresh.isEmpty()) state else state.copy(accepted = state.accepted + fresh)
        }
        if (added > 0) persistAccepted()
        return added
    }

    /** Persist the current accepted list (best-effort; no-op in tests
     *  where no Context was supplied). Called after every accept. */
    private fun persistAccepted() {
        appCtx?.let { AcceptedContactsStore.save(it, mutableState.value.accepted) }
        pushRenamesToDirectory()
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

    /** This account's own profile in wire form, or `null` when none is stored. */
    private fun ownProfileForWire(accountId: ByteArray): uniffi.lakko_ffi_uniffi.FfiProfile? {
        val ctx = appCtx ?: return null
        return me.lakko.app.profile.OwnProfileStore.load(ctx, accountId)?.toFfi()
    }

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
        val isManualRefresh = waitSecs == 0
        if (isManualRefresh) {
            mutableState.update { it.copy(refreshing = true, lastError = null) }
        }
        try {
            useClientWith(secrets) { client, sender ->
                val result = client.pollStrangerInbox(
                    mySignaturePubkey = sender.signaturePubkey,
                    myX25519Secret = sender.x25519Secret,
                    after = mutableState.value.cursor,
                    waitSecs = waitSecs,
                )
                val newIncoming = result.events.filterIsInstance<
                    IncomingMailboxEvent.ContactRequest>(
                ).map { it.request }
                val welcomes = result.events.filterIsInstance<
                    IncomingMailboxEvent.Welcome>(
                )
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
                            chatRepo.joinGroupFromWelcome(welcome.welcomeBytes)
                        } catch (e: kotlinx.coroutines.CancellationException) {
                            throw e
                        } catch (e: Throwable) {
                            // A dropped Welcome is a chat someone started
                            // with this user that will never appear, so it
                            // must not vanish silently. The detail goes to
                            // the log; the inline error tells the user.
                            android.util.Log.w(
                                "ContactsRepository",
                                "joining a group from a Welcome failed",
                                e,
                            )
                            joinFailure = WELCOME_JOIN_FAILED_MESSAGE
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
                mutableState.update { state ->
                    state.copy(
                        pending = state.pending + newIncoming,
                        cursor = result.cursor,
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
            }
        } catch (e: ContactsError) {
            // Background long-polls surface failures on the
            // `lastError` slot too — the UI renders a small
            // inline hint without flipping `refreshing`. The
            // caller (a `LaunchedEffect` loop) will retry on
            // the next iteration.
            mutableState.update {
                it.copy(refreshing = false, lastError = e.message)
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
        mutableState.update { state ->
            val (accepted, remaining) = state.pending.partition { it.messageId == messageId }
            if (accepted.isEmpty()) return@update state
            state.copy(
                pending = remaining,
                accepted = state.accepted + accepted.map {
                    AcceptedContact(request = it, groupIdHex = groupIdHex)
                },
            )
        }
        persistAccepted()
    }

    /**
     * Set (or clear, when blank) the user-chosen display name for an
     * accepted contact (ADR-0021 follow-up). A blank name reverts the UI +
     * the group-call roster to the short pubkey fingerprint. Persists the
     * accepted list so the change survives a relaunch.
     */
    public fun renameContact(messageId: Long, newDisplayName: String) {
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

    /** Drop an incoming request from the pending list without
     *  acknowledging it to the sender. Mirror of the web
     *  Decline semantics — server doesn't get told, the row
     *  just disappears locally. */
    public fun declineContactRequest(messageId: Long) {
        mutableState.update { state ->
            state.copy(pending = state.pending.filterNot { it.messageId == messageId })
        }
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
     *   1. base64url-decode the slug to recover the 32-byte
     *      `blob_id`.
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
        val blobId = try {
            base64UrlDecodeNoPad(slug)
        } catch (_: Throwable) {
            return AcceptInviteResult.MalformedSlug
        }
        if (blobId.size != 32) return AcceptInviteResult.MalformedSlug

        val capabilityBytes = try {
            authenticatedDiscoveryClient().use { discovery ->
                discovery.fetchInvite(blobId)
            }
        } catch (e: Throwable) {
            return AcceptInviteResult.Failed(message = e.message ?: "fetch failed")
        } ?: return AcceptInviteResult.NotFound

        val cap = try {
            InviteCapability.decodeAndVerify(capabilityBytes)
        } catch (_: Throwable) {
            // Signature / wire-format failure → treat as
            // not-found from the user's perspective; the
            // server might also have been compromised, but
            // splitting hairs on that doesn't help the UI.
            return AcceptInviteResult.NotFound
        }
        return cap.use { capability ->
            val nowMs = System.currentTimeMillis().toULong()
            if (!capability.isValidNow(nowMs)) {
                return@use AcceptInviteResult.Expired
            }

            // Snapshot the bundle fields we need post-consume.
            val inviterAccountId = capability.ownerAccountId()
            val inviterSigPubkey = capability.ownerSignaturePubkey()
            val inviterX25519Pubkey = capability.ownerX25519Pubkey()

            // Consume — atomic counter bump + per-joiner dedup.
            val consumeResult = try {
                authenticatedDiscoveryClient().use { discovery ->
                    discovery.consumeInvite(blobId, capabilityBytes, secrets.accountId)
                }
            } catch (e: Throwable) {
                return@use AcceptInviteResult.Failed(
                    message = e.message ?: "consume failed",
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
                    )
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
            mutableState.update { state ->
                state.copy(
                    accepted = state.accepted + AcceptedContact(
                        request = syntheticRequest,
                        groupIdHex = groupIdHex,
                    ),
                )
            }
            persistAccepted()
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
     * server-assigned 32-byte `blob_id` back. Returns the
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
            val capabilityBytes = try {
                composeInviteCapability(
                    ownerSigningSeed = identity.ed25519Seed(),
                    ownerAccountId = secrets.accountId,
                    ownerX25519Pubkey = identity.x25519PublicKey(),
                    expiryMs = expiryMs,
                    maxUses = maxUses,
                    bootstrapReferral = bootstrapReferral,
                    label = label,
                )
            } catch (e: Throwable) {
                throw ContactsError.wrap(e)
            }
            val blobId = try {
                authenticatedDiscoveryClient().use { discovery ->
                    discovery.uploadInvite(capabilityBytes)
                }
            } catch (e: Throwable) {
                throw ContactsError.wrap(e)
            }

            val blobIdSlug = base64UrlEncodeNoPad(blobId)
            val url = "$INVITE_URL_PREFIX$blobIdSlug"
            val invite = GeneratedInvite(
                blobId = blobId,
                blobIdSlug = blobIdSlug,
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
     * ADR-0032 — mint a `/c/<slug>` call link owned by this account.
     *
     * Signs a `CallInviteCapability` under the account identity, uploads it to
     * the discovery ledger (a PoW solve, so this can take seconds) and returns
     * the pointer link. A call link is independent of any chat: whoever opens it
     * lands on the JoinCall screen, where this account is recognised as the
     * owner and founds + hosts the call.
     *
     * @throws ContactsError on missing identity secrets, a compose failure, or
     *   a discovery upload failure.
     */
    public suspend fun createCallLink(
        media: me.lakko.app.calls.CallMediaPreset,
        expiry: me.lakko.app.calls.CallInviteExpiryPreset,
        maxUses: me.lakko.app.calls.CallInviteMaxUsesPreset,
        label: String,
    ): me.lakko.app.calls.CallInviteComposer.Composed {
        val secrets = authRepo.identitySecrets.value
            ?: throw ContactsError.wrap(
                IllegalStateException(
                    "no identity secrets — please re-enter your password",
                ),
            )
        return try {
            Identity.fromSeed(secrets.accountIdentitySeed).use { identity ->
                authenticatedDiscoveryClient().use { discovery ->
                    me.lakko.app.calls.CallInviteComposer(discovery).composeAndUpload(
                        ownerSigningSeed = identity.ed25519Seed(),
                        ownerAccountId = secrets.accountId,
                        identity = identity,
                        media = media,
                        expiryPreset = expiry,
                        maxUsesPreset = maxUses,
                        label = label,
                    )
                }
            }
        } catch (e: Throwable) {
            throw ContactsError.wrap(e)
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
                    val blobIdHex = blobId.toList()
                    mutableState.update { state ->
                        state.copy(
                            outstandingInvites = state.outstandingInvites
                                .filterNot { it.blobId.toList() == blobIdHex },
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

    /** Clear the in-memory state. Called from the AuthRepository
     *  when the session ends — pending requests + sent log do
     *  not persist across logouts. */
    public fun clear() {
        // Wipe any decrypted intro plaintext on the way out.
        // Best-effort: the `String` contents may still live in
        // heap if some collector cached them, but our reference
        // drops here.
        mutableState.value = ContactsState()
        // Wipe the persisted accepted list too — Android contacts state
        // does not survive a logout (ADR-0021 store follows that intent).
        persistAccepted()
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
    )
}

// =============================================================================
// State shape
// =============================================================================

/** Inline inbox error shown when a Welcome from a peer could not be joined. */
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
 *   * [MalformedSlug] — the URL slug is not valid base64url
 *     or doesn't decode to a 32-byte blob_id.
 *   * [RequiresReauth] — [AuthRepository.identitySecrets] is
 *     `null`. The UI surfaces a "please re-enter your
 *     password" panel + a Login deep-link.
 *   * [Failed] — any other underlying failure (transport,
 *     OPAQUE / OPRF wire-format mismatch on the consume PoW,
 *     MLS group creation throwing). Carries the inner error
 *     message for diagnostics.
 */
public sealed interface AcceptInviteResult {
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
    public data object RequiresReauth : AcceptInviteResult

    public data class Failed(val message: String) : AcceptInviteResult
}

/** An invite link the owner has just generated this session. */
public data class GeneratedInvite(
    /** Raw 32-byte content-addressed id (the same value
     *  `revokeInviteLink` takes). */
    val blobId: ByteArray,
    /** base64url-no-pad of [blobId] — the slug portion of [url]. */
    val blobIdSlug: String,
    /** `https://lakko.me/i/<slug>` — the URL the owner shares. */
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

/** URL prefix Lakko shares its invite links under. Should
 *  remain in sync with the web `composePointerInviteUrl` host
 *  + path so a link generated on web works when consumed on
 *  Android and vice versa. */
private const val INVITE_URL_PREFIX: String = "https://lakko.me/i/"

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
