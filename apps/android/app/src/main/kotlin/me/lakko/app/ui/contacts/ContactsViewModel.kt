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
 *      slice of ephemeral UI state (invite links, busy / error
 *      flags) as [ContactsUiState].
 *   2. Dispatch UI actions to the repository: refresh the
 *      inbox, accept / decline a pending row. A contact request
 *      is sent from the people search (`search/`), not here.
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
            pending = repoState.pending,
            accepted = repoState.accepted,
            sent = repoState.sent,
            outstandingInvites = repoState.outstandingInvites,
            generatingInvite = ui.generatingInvite,
            inviteError = ui.inviteError,
            inviteNotice = ui.inviteNotice,
            activeInvitesLoaded = ui.activeInvitesLoaded,
            activeInvites = ui.activeInvites,
            activeInviteUrls = ui.activeInviteUrls,
            loadingActiveInvites = ui.loadingActiveInvites,
            activeInvitesError = ui.activeInvitesError,
            revokingActiveInviteSlug = ui.revokingActiveInviteSlug,
            refreshing = repoState.refreshing,
            refreshError = repoState.lastError,
            requiresReauth = repoState.requiresReauth,
            acceptingIds = ui.acceptingIds,
            acceptFailedIds = ui.acceptFailedIds,
        )

    /** Remove an accepted contact from this device (UX 5.3). Local only. */
    public fun onRemoveContactClicked(messageId: Long) {
        repo.removeContact(messageId)
    }

    public fun onRefreshClicked() {
        viewModelScope.launch { repo.refreshInbox() }
    }

    /**
     * Create link (UX `contact-invite-link.md` section 9): mints a link at
     * once with [params], the defaults unless given ([INVITE_DEFAULT_PARAMS]:
     * no label, 7 days, one person). Surfaces the resulting
     * [GeneratedInvite] in [ContactsState.outstandingInvites]; failures land
     * in [ContactsUiState.inviteError].
     */
    public fun onGenerateInviteClicked(params: InviteCreateParams = INVITE_DEFAULT_PARAMS) {
        val snapshot = mutableUi.value
        if (snapshot.generatingInvite) return

        mutableUi.update { it.copy(generatingInvite = true, inviteError = null, inviteNotice = null) }
        viewModelScope.launch {
            try {
                mint(params)
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

    private suspend fun mint(params: InviteCreateParams): GeneratedInvite =
        repo.generateInviteLink(
            label = params.label,
            expiryMillisFromNow = params.expiryMillisFromNow,
            maxUses = params.maxUses,
        )

    /** The account's synced link records (ADR-0079 section 4), which say
     *  whether a row is editable and whether Delete asks first. */
    public fun inviteLinkRecords(): StateFlow<List<uniffi.lakko_ffi_uniffi.LinkRecord>> = repo.inviteLinkRecords()

    /**
     * Copy (UX section 9): mark the link copied in the account's synced
     * state (sent to the other devices like a revoke or a use) FIRST, then
     * [writeClipboard]. A link whose record is not known, or already marked,
     * is copied without a write. Showing the QR code does not come here.
     */
    public fun onCopyInviteClicked(slug: String, url: String, writeClipboard: (String) -> Unit) {
        val sync = inviteLinkSyncBySlug(repo.inviteLinkRecords().value)[slug]
        if (sync == null || sync.shared) {
            writeClipboard(url)
            return
        }
        viewModelScope.launch {
            try {
                repo.markInviteLinkShared(sync.linkId)
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err)
                // The copy still happens: the person asked for the link. The
                // record stays editable on the devices until the next write.
                logFailure("mark invite link copied", err)
            }
            writeClipboard(url)
        }
    }

    /**
     * Delete link (the old Revoke, UX section 9): the existing revoke path.
     * The sheet asks first when [inviteDeleteNeedsConfirmation] says so; this
     * deletes. On success the row leaves both lists and the sheet says
     * `invite.links.deleted`; a failure says `invite.links.error.delete`.
     */
    public fun onDeleteInviteClicked(slug: String) {
        if (mutableUi.value.revokingActiveInviteSlug != null) return
        mutableUi.update {
            it.copy(revokingActiveInviteSlug = slug, inviteError = null, activeInvitesError = null, inviteNotice = null)
        }
        viewModelScope.launch {
            val ok = try {
                deleteBySlug(slug)
                true
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableUi.update { it.copy(revokingActiveInviteSlug = null) }
                }
                logFailure("delete invite link", err)
                false
            }
            mutableUi.update {
                it.copy(
                    revokingActiveInviteSlug = null,
                    inviteNotice = if (ok) UiText("invite.links.deleted") else null,
                    inviteError = if (ok) null else UiText("invite.links.error.delete"),
                )
            }
        }
    }

    /**
     * Save on the edit form (UX section 9): mint a NEW link with [params]
     * first, then delete the old one [slug] by the Delete path. A failure to
     * mint leaves the old link and says `invite.links.error.edit`; a failure
     * to delete the old one (the new one exists) says
     * `invite.links.error.delete`. [onSaved] runs once the new link exists
     * (the sheet closes its form).
     */
    public fun onEditInviteSaved(slug: String, params: InviteCreateParams, onSaved: () -> Unit = {}) {
        val snapshot = mutableUi.value
        if (snapshot.generatingInvite || snapshot.revokingActiveInviteSlug != null) return
        mutableUi.update {
            it.copy(generatingInvite = true, revokingActiveInviteSlug = slug, inviteError = null, inviteNotice = null)
        }
        viewModelScope.launch {
            try {
                mint(params)
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableUi.update { it.copy(generatingInvite = false, revokingActiveInviteSlug = null) }
                }
                logFailure("edit invite link: mint", err)
                mutableUi.update {
                    it.copy(
                        generatingInvite = false,
                        revokingActiveInviteSlug = null,
                        inviteError = UiText("invite.links.error.edit"),
                    )
                }
                return@launch
            }
            onSaved()
            val deleted = try {
                deleteBySlug(slug)
                true
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err) {
                    mutableUi.update { it.copy(generatingInvite = false, revokingActiveInviteSlug = null) }
                }
                logFailure("edit invite link: delete old", err)
                false
            }
            mutableUi.update {
                it.copy(
                    generatingInvite = false,
                    revokingActiveInviteSlug = null,
                    inviteError = if (deleted) null else UiText("invite.links.error.delete"),
                )
            }
        }
    }

    /** Revoke the link [slug] through [ContactsRepository.revokeInviteLink]
     *  (it drops a session row) and drop it from the loaded list. A slug that
     *  is not base64url throws [IllegalArgumentException]. */
    private suspend fun deleteBySlug(slug: String) {
        val blobId = me.lakko.app.auth.base64UrlDecodeNoPad(slug)
        repo.revokeInviteLink(blobId)
        mutableUi.update { ui -> ui.copy(activeInvites = ui.activeInvites.filterNot { it.blobIdSlug == slug }) }
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
     * Accept a pending request ([ContactsRepository.acceptRequest]). A failure
     * is never swallowed: when nothing could be delivered and no group stands,
     * the request stays listed with `contacts.request.accept_failed` and
     * Accept works again. With no [chatRepo] wired (tests, a contacts-only
     * configuration) Accept stays a local marker.
     */
    public fun onAcceptClicked(messageId: Long) {
        val pending = repo.state.value.pending.firstOrNull { it.messageId == messageId }
        if (pending == null) return
        if (chatRepo == null) {
            viewModelScope.launch { sendAckBestEffort(pending) }
            repo.acceptContactRequest(messageId)
            return
        }
        if (messageId in mutableUi.value.acceptingIds) return
        mutableUi.update { it.copy(acceptingIds = it.acceptingIds + messageId, acceptFailedIds = it.acceptFailedIds - messageId) }
        viewModelScope.launch {
            val outcome = try {
                repo.acceptRequest(messageId)
            } catch (cancellation: kotlinx.coroutines.CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                me.lakko.app.contacts.AcceptOutcome.FAILED
            }
            mutableUi.update { ui ->
                ui.copy(
                    acceptingIds = ui.acceptingIds - messageId,
                    acceptFailedIds = if (outcome == me.lakko.app.contacts.AcceptOutcome.FAILED) {
                        ui.acceptFailedIds + messageId
                    } else {
                        ui.acceptFailedIds - messageId
                    },
                )
            }
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
            android.util.Log.w("LakkoContacts", "accept: ack not sent")
        }
    }

    public fun onDeclineClicked(messageId: Long) {
        mutableUi.update { it.copy(acceptFailedIds = it.acceptFailedIds - messageId) }
        repo.declineContactRequest(messageId)
    }

    /** **Dismiss** on a row of Contacts > Sent. */
    public fun onDismissSentClicked(request: me.lakko.app.contacts.SentContactRequest) {
        repo.dismissSentRequest(request)
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

    /** VM-internal slice carrying ephemeral invite-generation
     *  state. Persistent contacts data lives
     *  in the repository. */
    private data class UiSlice(
        /** The requests whose Accept is running. */
        val acceptingIds: Set<Long> = emptySet(),
        /** The requests whose last Accept failed with nothing standing. */
        val acceptFailedIds: Set<Long> = emptySet(),
        val generatingInvite: Boolean = false,
        val inviteError: UiText? = null,
        /** A notice on the Invite links sheet (`invite.links.deleted`). */
        val inviteNotice: UiText? = null,
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
        /** `blobIdSlug` of the row being deleted (or replaced by an
         *  edit), when any. The screen disables Delete and Edit
         *  meanwhile. */
        val revokingActiveInviteSlug: String? = null,
    )
}

/** Combined snapshot the [ContactsScreen] renders. */
public data class ContactsUiState(
    val pending: List<me.lakko.app.contacts.IncomingContactRequest>,
    val accepted: List<me.lakko.app.contacts.AcceptedContact>,
    val sent: List<me.lakko.app.contacts.SentContactRequest>,
    val outstandingInvites: List<GeneratedInvite>,
    val generatingInvite: Boolean,
    val inviteError: UiText?,
    /** A notice on the Invite links sheet (`invite.links.deleted`). */
    val inviteNotice: UiText? = null,
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
    /** The requests whose Accept is running (the row's buttons are off). */
    val acceptingIds: Set<Long> = emptySet(),
    /** The requests whose last Accept failed: the row says so. */
    val acceptFailedIds: Set<Long> = emptySet(),
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

