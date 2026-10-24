// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.chat.AllowanceKind
import me.lakko.app.chat.ChatRepository
import me.lakko.app.contacts.ContactsError
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.contacts.ContactsState
import me.lakko.app.contacts.GeneratedInvite
import me.lakko.app.contacts.SupersessionRefused
import me.lakko.app.l10n.UiText

/**
 * State + dispatcher for the [ContactsScreen].
 *
 * Three responsibilities:
 *
 *   1. Surface the [ContactsRepository.state] flow + a small
 *      slice of ephemeral UI state (the search bar's current
 *      input, the intro message draft, busy / error flags
 *      around the send action) as [ContactsUiState].
 *   2. Dispatch UI actions to the repository: send a request,
 *      refresh the inbox, accept / decline a pending row.
 *   3. Map [ContactsError]'s discriminated variants into
 *      user-facing copy.
 */
public class ContactsViewModel(
    private val repo: ContactsRepository,
    /** Optional bridge to the chat surface. When non-null,
     *  [onAcceptClicked] drives the MLS handshake
     *  (`ChatRepository.startGroupWith`) before flipping the
     *  contact row from pending → accepted; the resulting
     *  `groupIdHex` lands on the [me.lakko.app.contacts.AcceptedContact]
     *  so the UI can surface an "Open chat" link. When `null`
     *  (tests / future contacts-only configs), Accept stays a
     *  pure local marker. */
    private val chatRepo: ChatRepository? = null,
    /** ADR-0048 §4 — the core's username rule (`validateUsername`). */
    private val validateUsername: (String) -> String =
        { uniffi.lakko_ffi_uniffi.validateUsername(it) },
) : ViewModel() {

    private val mutableUi: MutableStateFlow<UiSlice> = MutableStateFlow(UiSlice())

    /**
     * Combined state surface for the Compose screen. The UI
     * collects this single flow and renders both the
     * persistent contacts data (from the repository) and the
     * ephemeral form / busy / error state (from the VM-local
     * `mutableUi`).
     */
    public val state: StateFlow<ContactsUiState> = combineState()

    /** ADR-0079 section 4 — the people added through their invite link whose
     *  app has not answered yet. */
    public val invited: StateFlow<List<me.lakko.app.contacts.PendingContact>> = repo.invited

    private fun combineState(): StateFlow<ContactsUiState> {
        // Compose two flows by hand into a single derived
        // StateFlow. kotlinx-coroutines has `combine(...)` for
        // cold flows but no synchronous combine for two
        // StateFlows; doing it manually keeps the public flow
        // hot + lifecycle-friendly.
        val combined = MutableStateFlow(buildUi(repo.state.value, mutableUi.value))
        viewModelScope.launch {
            repo.state.collect { repoState ->
                combined.value = buildUi(repoState, mutableUi.value)
            }
        }
        viewModelScope.launch {
            mutableUi.collect { uiSlice ->
                combined.value = buildUi(repo.state.value, uiSlice)
            }
        }
        return combined.asStateFlow()
    }

    init {
        // A one-to-one conversation IS a contact: whenever the set of
        // conversations this device holds changes (not on every message),
        // every one-to-one peer without a contact row gets one
        // ([ContactsRepository.adoptOneToOnePeer]). A StateFlow replays its
        // current value, so this also runs once at start. Off the main thread
        // because the roster read is a JNA call into the core
        // (`MainThreadGuard`).
        chatRepo?.let { chat ->
            viewModelScope.launch(Dispatchers.Default) {
                chat.state
                    .map { s -> s.threads.map { it.groupIdHex.lowercase() }.toSet() }
                    .distinctUntilChanged()
                    .collect { adoptOneToOnePeers() }
            }
        }
    }

    /**
     * Give every one-to-one peer a contact row (see
     * [ContactsRepository.adoptOneToOnePeer]). The account id, when a received
     * profile has carried one (ADR-0048 v2), rides along so the row can name
     * the person in a group call like an accepted one does.
     */
    public fun adoptOneToOnePeers() {
        val chat = chatRepo ?: return
        val accountIds = me.lakko.app.profile.ProfileDirectory.get(chat.context).accountIdsByKey()
        val nowMs = System.currentTimeMillis().toULong()
        val createdAt = java.time.Instant.now().toString()
        for (thread in chat.state.value.threads) {
            // ADR-0050 — `oneToOnePeerKey` counts PEOPLE, not device leaves.
            val peer = chat.oneToOnePeerKey(thread.groupIdHex) ?: continue
            val peerB64 = java.util.Base64.getEncoder().encodeToString(peer)
            repo.adoptOneToOnePeer(
                signatureKey = peer,
                accountId = accountIds[peerB64]?.let(::hexToBytes),
                groupIdHex = thread.groupIdHex,
                nowMs = nowMs,
                createdAt = createdAt,
            )
        }
    }

    /**
     * `Add to contacts` on the card of a person the viewer removed
     * (docs/ux/conversation-info.md section 2.1, amended 2026-10-24): back on
     * the list with the one-to-one they have. Local; sends nothing.
     */
    public fun onAddContactBack(signatureKey: ByteArray, groupIdHex: String) {
        val accountId = chatRepo?.let { chat ->
            val keyB64 = java.util.Base64.getEncoder().encodeToString(signatureKey)
            me.lakko.app.profile.ProfileDirectory.get(chat.context).accountIdsByKey()[keyB64]?.let(::hexToBytes)
        }
        repo.restoreRemovedContact(
            signatureKey = signatureKey,
            accountId = accountId,
            groupIdHex = groupIdHex,
            nowMs = System.currentTimeMillis().toULong(),
            createdAt = java.time.Instant.now().toString(),
        )
    }

    private fun hexToBytes(hex: String): ByteArray? {
        if (hex.length % 2 != 0) return null
        return try {
            ByteArray(hex.length / 2) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
        } catch (_: NumberFormatException) {
            null
        }
    }

    private fun buildUi(repoState: ContactsState, ui: UiSlice): ContactsUiState =
        ContactsUiState(
            usernameInput = ui.usernameInput,
            usernameError = ui.usernameError,
            introInput = ui.introInput,
            sending = ui.sending,
            sendError = ui.sendError,
            sendSucceeded = ui.sendSucceeded,
            pending = repoState.pending,
            accepted = repoState.accepted,
            sent = repoState.sent,
            outstandingInvites = repoState.outstandingInvites,
            generatingInvite = ui.generatingInvite,
            inviteError = ui.inviteError,
            activeInvitesLoaded = ui.activeInvitesLoaded,
            activeInvites = ui.activeInvites,
            activeInviteUrls = ui.activeInviteUrls,
            loadingActiveInvites = ui.loadingActiveInvites,
            activeInvitesError = ui.activeInvitesError,
            revokingActiveInviteSlug = ui.revokingActiveInviteSlug,
            refreshing = repoState.refreshing,
            refreshError = repoState.lastError,
            requiresReauth = repoState.requiresReauth,
        )

    /** ADR-0048 §4 — the search field applies the username rule as the user
     *  types and shows the reason. */
    public fun onUsernameChange(value: String) {
        val check = me.lakko.app.profile.checkUsername(value, validateUsername)
        mutableUi.update {
            it.copy(
                usernameInput = value,
                usernameError = me.lakko.app.profile.usernameFieldErrorText(check, value),
            )
        }
    }

    public fun onIntroChange(value: String) {
        mutableUi.update { it.copy(introInput = value) }
    }

    /** Send a contact request. Surfaces success by clearing the
     *  form; failures land in [ContactsUiState.sendError]. */
    public fun onSendClicked() {
        val snapshot = mutableUi.value
        if (snapshot.sending) return
        if (snapshot.usernameInput.isBlank()) {
            mutableUi.update { it.copy(sendError = UiText("username.error.empty")) }
            return
        }
        // ADR-0048 §4 — never send a lookup for a name that fails the rule.
        val username = when (
            val decision = me.lakko.app.profile.searchDecision(snapshot.usernameInput, validateUsername)
        ) {
            is me.lakko.app.profile.SearchDecision.Refuse -> {
                mutableUi.update { it.copy(sendError = decision.text) }
                return
            }
            is me.lakko.app.profile.SearchDecision.Lookup -> decision.normalized
        }

        mutableUi.update { it.copy(sending = true, sendError = null) }
        viewModelScope.launch {
            try {
                repo.sendContactRequest(
                    recipientUsername = username,
                    introMessage = snapshot.introInput,
                )
                // Success — clear the form so the user can
                // queue another one; the sheet reads the flag to close.
                mutableUi.update {
                    it.copy(
                        sending = false,
                        sendError = null,
                        sendSucceeded = true,
                        usernameInput = "",
                        introInput = "",
                    )
                }
            } catch (refused: SupersessionRefused) {
                // KT-log S4 — a fail-closed supersession refusal (an
                // unproven CHANGED key) is the moment the reset-vs-
                // compromise distinction matters most. Surface a reset-
                // AWARE message driven by the best-effort log marker the
                // client attached (`loggedReset`). The refusal itself is
                // unchanged — gate 1 threw regardless of the log; this only
                // colours the prompt.
                mutableUi.update {
                    it.copy(
                        sending = false,
                        sendError = supersessionRefusedMessage(
                            refused.loggedReset,
                            atLabel(username),
                        ),
                    )
                }
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableUi.update { it.copy(sending = false) }
                }
                // ADR-0054 - the username lookup spends an anonymous token.
                logFailure("send contact request", err)
                mutableUi.update {
                    it.copy(
                        sending = false,
                        sendError = contactsErrorText(err, "contacts.error.send", AllowanceKind.LOOKUP),
                    )
                }
            }
        }
    }

    /** The add-by-username sheet acknowledged the success (it closed). */
    public fun consumeSendSuccess() {
        mutableUi.update { if (it.sendSucceeded) it.copy(sendSucceeded = false) else it }
    }

    /** The add-by-username sheet was dismissed: drop a stale error so the
     *  next opening starts clean; the typed values are kept. */
    public fun onDismissAddSheet() {
        mutableUi.update { it.copy(sendError = null, sendSucceeded = false) }
    }

    /** Remove an accepted contact from this device (UX 5.3). Local only. */
    public fun onRemoveContactClicked(messageId: Long) {
        repo.removeContact(messageId)
    }

    public fun onRefreshClicked() {
        viewModelScope.launch { repo.refreshInbox() }
    }

    /**
     * Generate a fresh single-use 7-day invite link. Surfaces
     * the resulting [GeneratedInvite] in
     * [ContactsState.outstandingInvites]; failures land in
     * [ContactsUiState.inviteError].
     *
     * Single-button-fire shape for 1.15e10. The custom-config
     * dialog (user-typed label / expiry / max_uses) lands in
     * 1.15e10b when the UX pushes back.
     */
    public fun onGenerateInviteClicked() {
        val snapshot = mutableUi.value
        if (snapshot.generatingInvite) return

        mutableUi.update { it.copy(generatingInvite = true, inviteError = null) }
        viewModelScope.launch {
            try {
                repo.generateInviteLink()
                mutableUi.update {
                    it.copy(generatingInvite = false, inviteError = null)
                }
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableUi.update { it.copy(generatingInvite = false) }
                }
                logFailure("create invite link", err)
                mutableUi.update {
                    it.copy(generatingInvite = false, inviteError = contactsErrorText(err, "chat.error.create_link"))
                }
            }
        }
    }

    /**
     * Revoke an outstanding invite by [blobId]. On success the
     * row is dropped from [ContactsState.outstandingInvites]
     * automatically by [ContactsRepository.revokeInviteLink].
     */
    public fun onRevokeInviteClicked(blobId: ByteArray) {
        viewModelScope.launch {
            try {
                repo.revokeInviteLink(blobId)
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err)
                logFailure("revoke invite link", err)
                mutableUi.update {
                    it.copy(inviteError = contactsErrorText(err, "chat.error.revoke_link"))
                }
            }
        }
    }

    /**
     * Refresh the owner-side active-invites list (Phase
     * 1.18d2b-list). Explicit user gesture only — not
     * auto-loaded so the timing-correlation side-channel ("server
     * sees an owner-list query") stays under user control.
     */
    public fun onRefreshActiveInvitesClicked() {
        val snapshot = mutableUi.value
        if (snapshot.loadingActiveInvites) return
        mutableUi.update {
            it.copy(loadingActiveInvites = true, activeInvitesError = null)
        }
        viewModelScope.launch {
            try {
                val result = repo.listActiveInvites()
                val rows = when (result) {
                    is me.lakko.app.discovery.ListInvitesResult.List -> result.invites
                    me.lakko.app.discovery.ListInvitesResult.NotFoundOrNotOwner -> emptyList()
                }
                val urls = try {
                    repo.heldInviteLinkUrls(rows.map { it.blobIdSlug })
                } catch (_: Throwable) {
                    emptyMap()
                }
                mutableUi.update {
                    it.copy(
                        loadingActiveInvites = false,
                        activeInvitesLoaded = true,
                        activeInvites = rows,
                        activeInviteUrls = urls,
                    )
                }
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableUi.update { it.copy(loadingActiveInvites = false) }
                }
                logFailure("list invite links", err)
                mutableUi.update {
                    it.copy(
                        loadingActiveInvites = false,
                        activeInvitesError = contactsErrorText(err, "invite.links.error.load"),
                    )
                }
            }
        }
    }

    /**
     * Revoke an active invite from the loaded list. Drops the
     * row locally on success or on a 404 (server-side already
     * gone — same terminal state).
     */
    public fun onRevokeActiveInviteClicked(slug: String) {
        val snapshot = mutableUi.value
        if (snapshot.revokingActiveInviteSlug != null) return
        val row = snapshot.activeInvites.firstOrNull { it.blobIdSlug == slug } ?: return
        // `base64UrlDecodeNoPad` returns a NON-null ByteArray and THROWS
        // `IllegalArgumentException` on a character outside the base64url
        // alphabet — it never returns null, so an elvis here would be dead
        // code and the raw exception would escape into the click handler.
        // The slug is server-supplied (it arrives on the active-invites
        // list), so a malformed one must land in the error state rather
        // than crash the screen.
        val blobId = try {
            me.lakko.app.auth.base64UrlDecodeNoPad(row.blobIdSlug)
        } catch (e: IllegalArgumentException) {
            logFailure("revoke invite link: malformed slug", e)
            mutableUi.update {
                it.copy(activeInvitesError = UiText("chat.error.revoke_link"))
            }
            return
        }
        mutableUi.update {
            it.copy(revokingActiveInviteSlug = slug, activeInvitesError = null)
        }
        viewModelScope.launch {
            try {
                repo.revokeInviteLink(blobId)
                mutableUi.update {
                    it.copy(
                        revokingActiveInviteSlug = null,
                        activeInvites = it.activeInvites.filterNot { row ->
                            row.blobIdSlug == slug
                        },
                    )
                }
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableUi.update { it.copy(revokingActiveInviteSlug = null) }
                }
                logFailure("revoke invite link", err)
                mutableUi.update {
                    it.copy(
                        revokingActiveInviteSlug = null,
                        activeInvitesError = contactsErrorText(err, "chat.error.revoke_link"),
                    )
                }
            }
        }
    }

    /**
     * One long-poll tick — used by the composable's
     * `LaunchedEffect(Unit) { while (isActive) { pollOnce(...) } }`
     * loop. Foreground-only continuous polling (1.15e7);
     * WorkManager-driven background fetch + FCM push lands in
     * 1.15e7b/c.
     *
     * Pulled out as a suspend method on the VM (rather than
     * inlined in the composable) so the test seam — once we
     * land an instrumented suite — can drive a single
     * iteration without owning the loop's lifecycle.
     */
    public suspend fun pollOnceLongPoll(waitSecs: Int = DEFAULT_LONG_POLL_SECS) {
        repo.refreshInbox(waitSecs = waitSecs)
    }

    public companion object {
        /** DS long-poll budget for the foreground-poll loop.
         *  The server clamps to its own `MAX_WAIT_SECS`; 30
         *  matches the canonical chat-app cadence. */
        public const val DEFAULT_LONG_POLL_SECS: Int = 30
    }

    /**
     * Accept a pending request:
     *
     *   1. If a [chatRepo] is wired AND the request carries a
     *      `fromAccountId` (the sender's UUIDv7 — present on
     *      every v2 ContactRequest, absent on v1 legacy
     *      envelopes), call
     *      [ChatRepository.startGroupWith] to fetch the
     *      sender's KeyPackage and ship them a Welcome.
     *   2. Move the row from `pending → accepted`, stashing the
     *      resulting `groupIdHex` (or `null` if the bridge is
     *      unwired / the chat handshake failed).
     *
     * Failures during the chat handshake fall through to the
     * "move row to accepted with `groupIdHex = null`" branch —
     * the user still sees the Accept confirmed in their UI; the
     * sender just won't get the Welcome (Phase-1 trade-off the
     * UI surfaces as "chat creation failed, retry"). A future
     * sprint can add an explicit retry affordance.
     */
    public fun onAcceptClicked(messageId: Long) {
        val pending = repo.state.value.pending.firstOrNull { it.messageId == messageId }
        if (pending == null) return

        if (chatRepo == null || pending.fromAccountId == null) {
            // No chat bridge wired OR v1 legacy envelope without
            // a sender account-id — fall back to the pre-1.15f4c
            // "purely local marker" semantics.
            viewModelScope.launch { sendAckBestEffort(pending) }
            repo.acceptContactRequest(messageId)
            return
        }
        viewModelScope.launch {
            val groupIdHex = try {
                chatRepo.startGroupWith(
                    recipientAccountId = pending.fromAccountId,
                    recipientSignaturePubkey = pending.fromPubkey,
                    // ADR-0062 — a contact accept creates a one-to-one, fixed
                    // in the group: it lists under Chats on every device.
                    kind = uniffi.lakko_ffi_uniffi.GroupKind.ONE_TO_ONE,
                )
            } catch (_: Throwable) {
                // chat handshake failed — fall back to the
                // local-marker path. The UI surfaces the
                // missing groupIdHex as a "chat creation
                // failed" hint on the accepted row.
                null
            }
            repo.acceptContactRequest(messageId, groupIdHex = groupIdHex)
            // AFTER the chat's Welcome, never before it: see [sendAckBestEffort].
            sendAckBestEffort(pending)
        }
    }

    /**
     * 1.9b1: ship a ContactAck back to the original sender's mailbox so their
     * UI can flip from "pending" to "accepted". Best-effort — a network
     * failure here doesn't unwind the local accept.
     *
     * Sent once the 1:1 chat's Welcome has been written. Both are written with
     * the requester's `reply` grant (ADR-0060 section 6), and a requester
     * closes that key as soon as the ack arrives: an ack sent first (or in
     * parallel) let an online requester close the key before the Welcome
     * landed, and the Welcome was refused (seen live on web 2026-09-30).
     */
    private suspend fun sendAckBestEffort(pending: me.lakko.app.contacts.IncomingContactRequest) {
        try {
            repo.sendContactAck(pending)
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (_: Throwable) {
            // best-effort
        }
    }

    public fun onDeclineClicked(messageId: Long) {
        repo.declineContactRequest(messageId)
    }

    /** Set (or clear, when blank) the user-chosen display name for an
     *  accepted contact (ADR-0021 follow-up). The repository persists it
     *  and the group-call roster picks it up on the next call. */
    public fun onRenameContactClicked(messageId: Long, name: String) {
        repo.renameContact(messageId, name)
    }

    /** Core, server and transport prose goes to the log only (ADR-0061 section 6). */
    private fun logFailure(action: String, err: Throwable) {
        android.util.Log.w("LakkoContacts", "WARN $action failed: ${err.javaClass.simpleName}: ${err.message}")
    }

    /** VM-internal slice carrying ephemeral form + send +
     *  invite-generation state. Persistent contacts data lives
     *  in the repository. */
    private data class UiSlice(
        val usernameInput: String = "",
        val usernameError: me.lakko.app.l10n.UiText? = null,
        val introInput: String = "",
        val sending: Boolean = false,
        val sendError: UiText? = null,
        /** Set once a request was sent; the sheet closes on it and calls
         *  [consumeSendSuccess]. */
        val sendSucceeded: Boolean = false,
        val generatingInvite: Boolean = false,
        val inviteError: UiText? = null,
        /** Phase 1.18d2b-list — server-authoritative active-
         *  invites slice. `activeInvitesLoaded` distinguishes
         *  "user hasn't tapped Refresh yet" from "Refresh ran
         *  and returned an empty list" — the empty-state copy
         *  differs. */
        val activeInvitesLoaded: Boolean = false,
        val activeInvites: List<me.lakko.app.discovery.ActiveInvite> = emptyList(),
        /** UX 5.3 — the full URL of each listed link whose secret this device
         *  kept, by slug. */
        val activeInviteUrls: Map<String, String> = emptyMap(),
        val loadingActiveInvites: Boolean = false,
        val activeInvitesError: UiText? = null,
        /** `blobIdSlug` of the row currently being revoked, when
         *  any. The screen disables the Revoke button on that
         *  row + shows a spinner. */
        val revokingActiveInviteSlug: String? = null,
    )
}

/** Combined snapshot the [ContactsScreen] renders. */
public data class ContactsUiState(
    val usernameInput: String,
    /** ADR-0048 §4 — the reason the typed username fails, or `null`
     *  (ADR-0061: a catalog message the screen renders). */
    val usernameError: me.lakko.app.l10n.UiText? = null,
    val introInput: String,
    val sending: Boolean,
    /** ADR-0061 — rendered from the failure's kind, never from core prose. */
    val sendError: UiText?,
    /** A request was just sent; consumed by the add sheet. */
    val sendSucceeded: Boolean = false,
    val pending: List<me.lakko.app.contacts.IncomingContactRequest>,
    val accepted: List<me.lakko.app.contacts.AcceptedContact>,
    val sent: List<me.lakko.app.contacts.SentContactRequest>,
    val outstandingInvites: List<GeneratedInvite>,
    val generatingInvite: Boolean,
    val inviteError: UiText?,
    /** Phase 1.18d2b-list. */
    val activeInvitesLoaded: Boolean,
    val activeInvites: List<me.lakko.app.discovery.ActiveInvite>,
    /** UX 5.3 — slug -> full URL for the listed links this device can share;
     *  a listed slug absent here was created on another device. */
    val activeInviteUrls: Map<String, String> = emptyMap(),
    val loadingActiveInvites: Boolean,
    val activeInvitesError: UiText?,
    val revokingActiveInviteSlug: String?,
    val refreshing: Boolean,
    val refreshError: me.lakko.app.contacts.InboxError?,
    val requiresReauth: Boolean,
)

// =============================================================================
// KT-log S4 — reset-aware supersession-refusal copy (pure + testable)
// =============================================================================

/**
 * Render a typed-username into an `@handle` label for error copy. Trims
 * surrounding whitespace + a leading `@` the user may have typed so we
 * never double it; returns `null` for an empty input (the formatter then
 * uses the `_unnamed` message). Mirror of the web
 * `atLabel`.
 */
internal fun atLabel(raw: String): String? {
    val trimmed = raw.trim().trimStart('@')
    return if (trimmed.isNotEmpty()) "@$trimmed" else null
}

/**
 * The copy for a fail-closed identity refusal: a `SupersessionRefused` is
 * thrown when a contact's `signature_pubkey` changed and the change is
 * neither a proven rotation (ADR-0016) nor an attested reset this device
 * accepted (ADR-0073 R.2). An accepted reset never reaches this function: it
 * is silent. So the refusal has two readings, by the best-effort key-log
 * marker the client attached ([SupersessionRefused.loggedReset]):
 *
 * | marker  | meaning                                                  | message       |
 * |---------|----------------------------------------------------------|---------------|
 * | `false` | the verified log shows the served key WITHOUT a reset    | `unexplained` |
 * | other   | the log is off, unreachable, absent or inconclusive      | `unconfirmed` |
 *
 * The refusal itself is NEVER changed by the log; this only colours the
 * message. A pure top-level function so it is unit-testable without the
 * Compose VM. Mirror of the web `supersessionRefusedMessage`.
 *
 * @param peerLabel the handle the user typed (e.g. `@alice`), rendered
 *                  verbatim as the subject; the `_unnamed` message is used
 *                  when `null` / empty.
 */
internal fun supersessionRefusedMessage(
    loggedReset: Boolean?,
    peerLabel: String?,
): UiText {
    val base = if (loggedReset == false) {
        "contacts.identity_changed.unexplained"
    } else {
        "contacts.identity_changed.unconfirmed"
    }
    return if (!peerLabel.isNullOrEmpty()) {
        UiText(base, texts = mapOf("name" to peerLabel))
    } else {
        UiText("${base}_unnamed")
    }
}

