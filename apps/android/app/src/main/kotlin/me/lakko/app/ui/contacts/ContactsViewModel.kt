// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatRepository
import me.lakko.app.contacts.ContactsError
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.contacts.ContactsState
import me.lakko.app.contacts.GeneratedInvite
import me.lakko.app.contacts.SupersessionRefused

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

    private fun buildUi(repoState: ContactsState, ui: UiSlice): ContactsUiState =
        ContactsUiState(
            usernameInput = ui.usernameInput,
            introInput = ui.introInput,
            sending = ui.sending,
            sendError = ui.sendError,
            pending = repoState.pending,
            accepted = repoState.accepted,
            sent = repoState.sent,
            outstandingInvites = repoState.outstandingInvites,
            generatingInvite = ui.generatingInvite,
            inviteError = ui.inviteError,
            activeInvitesLoaded = ui.activeInvitesLoaded,
            activeInvites = ui.activeInvites,
            loadingActiveInvites = ui.loadingActiveInvites,
            activeInvitesError = ui.activeInvitesError,
            revokingActiveInviteSlug = ui.revokingActiveInviteSlug,
            refreshing = repoState.refreshing,
            refreshError = repoState.lastError,
            requiresReauth = repoState.requiresReauth,
        )

    public fun onUsernameChange(value: String) {
        mutableUi.update { it.copy(usernameInput = value) }
    }

    public fun onIntroChange(value: String) {
        mutableUi.update { it.copy(introInput = value) }
    }

    /** Send a contact request. Surfaces success by clearing the
     *  form; failures land in [ContactsUiState.sendError]. */
    public fun onSendClicked() {
        val snapshot = mutableUi.value
        if (snapshot.sending) return
        val username = snapshot.usernameInput.trim()
        if (username.isEmpty()) {
            mutableUi.update { it.copy(sendError = "Type a username first.") }
            return
        }

        mutableUi.update { it.copy(sending = true, sendError = null) }
        viewModelScope.launch {
            try {
                repo.sendContactRequest(
                    recipientUsername = username,
                    introMessage = snapshot.introInput,
                )
                // Success — clear the form so the user can
                // queue another one.
                mutableUi.update {
                    it.copy(
                        sending = false,
                        sendError = null,
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
            } catch (err: ContactsError) {
                mutableUi.update {
                    it.copy(sending = false, sendError = userFacingError(err))
                }
            } catch (err: Throwable) {
                mutableUi.update {
                    it.copy(
                        sending = false,
                        sendError = err.message ?: "Send failed.",
                    )
                }
            }
        }
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
            } catch (err: ContactsError) {
                mutableUi.update {
                    it.copy(generatingInvite = false, inviteError = userFacingError(err))
                }
            } catch (err: Throwable) {
                mutableUi.update {
                    it.copy(
                        generatingInvite = false,
                        inviteError = err.message ?: "Invite generation failed.",
                    )
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
            } catch (err: ContactsError) {
                mutableUi.update {
                    it.copy(inviteError = userFacingError(err))
                }
            } catch (err: Throwable) {
                mutableUi.update {
                    it.copy(inviteError = err.message ?: "Revoke failed.")
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
                mutableUi.update {
                    it.copy(
                        loadingActiveInvites = false,
                        activeInvitesLoaded = true,
                        activeInvites = rows,
                    )
                }
            } catch (err: ContactsError) {
                mutableUi.update {
                    it.copy(
                        loadingActiveInvites = false,
                        activeInvitesError = userFacingError(err),
                    )
                }
            } catch (err: Throwable) {
                mutableUi.update {
                    it.copy(
                        loadingActiveInvites = false,
                        activeInvitesError = err.message ?: "Could not load invites.",
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
            mutableUi.update {
                it.copy(activeInvitesError = "malformed blob_id slug — cannot revoke")
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
            } catch (err: ContactsError) {
                mutableUi.update {
                    it.copy(
                        revokingActiveInviteSlug = null,
                        activeInvitesError = userFacingError(err),
                    )
                }
            } catch (err: Throwable) {
                mutableUi.update {
                    it.copy(
                        revokingActiveInviteSlug = null,
                        activeInvitesError = err.message ?: "Revoke failed.",
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

        // 1.9b1: ship a ContactAck back to the original sender's
        // mailbox so their UI can flip from "pending" to
        // "accepted" without waiting on the Welcome arrival.
        // Best-effort — a network failure here doesn't unwind
        // the local accept (the user already trusted the peer);
        // we swallow and move on. The signed ack is independent
        // of MLS state, so the sender can light up their address
        // book even if the group-creation step below stalls.
        viewModelScope.launch {
            try {
                repo.sendContactAck(pending)
            } catch (_: Throwable) {
                // best-effort
            }
        }

        if (chatRepo == null || pending.fromAccountId == null) {
            // No chat bridge wired OR v1 legacy envelope without
            // a sender account-id — fall back to the pre-1.15f4c
            // "purely local marker" semantics.
            repo.acceptContactRequest(messageId)
            return
        }
        viewModelScope.launch {
            val groupIdHex = try {
                chatRepo.startGroupWith(
                    recipientAccountId = pending.fromAccountId,
                    recipientSignaturePubkey = pending.fromPubkey,
                )
            } catch (_: Throwable) {
                // chat handshake failed — fall back to the
                // local-marker path. The UI surfaces the
                // missing groupIdHex as a "chat creation
                // failed" hint on the accepted row.
                null
            }
            repo.acceptContactRequest(messageId, groupIdHex = groupIdHex)
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

    /** Map a [ContactsError] to a user-facing message. */
    private fun userFacingError(err: ContactsError): String =
        when (val v = err.variant) {
            is ContactsError.Variant.UsernameUnknown ->
                "@${v.normalisedUsername} is not registered on Lakko."
            is ContactsError.Variant.NoIdentityBundle ->
                "That account has no identity bundle yet — ask them to open the app once."
            is ContactsError.Variant.Wrapped ->
                v.cause.message ?: "Send failed."
        }

    /** VM-internal slice carrying ephemeral form + send +
     *  invite-generation state. Persistent contacts data lives
     *  in the repository. */
    private data class UiSlice(
        val usernameInput: String = "",
        val introInput: String = "",
        val sending: Boolean = false,
        val sendError: String? = null,
        val generatingInvite: Boolean = false,
        val inviteError: String? = null,
        /** Phase 1.18d2b-list — server-authoritative active-
         *  invites slice. `activeInvitesLoaded` distinguishes
         *  "user hasn't tapped Refresh yet" from "Refresh ran
         *  and returned an empty list" — the empty-state copy
         *  differs. */
        val activeInvitesLoaded: Boolean = false,
        val activeInvites: List<me.lakko.app.discovery.ActiveInvite> = emptyList(),
        val loadingActiveInvites: Boolean = false,
        val activeInvitesError: String? = null,
        /** `blobIdSlug` of the row currently being revoked, when
         *  any. The screen disables the Revoke button on that
         *  row + shows a spinner. */
        val revokingActiveInviteSlug: String? = null,
    )
}

/** Combined snapshot the [ContactsScreen] renders. */
public data class ContactsUiState(
    val usernameInput: String,
    val introInput: String,
    val sending: Boolean,
    val sendError: String?,
    val pending: List<me.lakko.app.contacts.IncomingContactRequest>,
    val accepted: List<me.lakko.app.contacts.AcceptedContact>,
    val sent: List<me.lakko.app.contacts.SentContactRequest>,
    val outstandingInvites: List<GeneratedInvite>,
    val generatingInvite: Boolean,
    val inviteError: String?,
    /** Phase 1.18d2b-list. */
    val activeInvitesLoaded: Boolean,
    val activeInvites: List<me.lakko.app.discovery.ActiveInvite>,
    val loadingActiveInvites: Boolean,
    val activeInvitesError: String?,
    val revokingActiveInviteSlug: String?,
    val refreshing: Boolean,
    val refreshError: String?,
    val requiresReauth: Boolean,
)

// =============================================================================
// KT-log S4 — reset-aware supersession-refusal copy (pure + testable)
// =============================================================================

/**
 * Render a typed-username into an `@handle` label for error copy. Trims
 * surrounding whitespace + a leading `@` the user may have typed so we
 * never double it; returns `null` for an empty input (the formatter then
 * falls back to its neutral "This contact" subject). Mirror of the web
 * `atLabel`.
 */
internal fun atLabel(raw: String): String? {
    val trimmed = raw.trim().trimStart('@')
    return if (trimmed.isNotEmpty()) "@$trimmed" else null
}

/**
 * KT-log S4 — the PURE 3-way mapping from `(loggedReset, peerLabel)` to the
 * user-facing string for a fail-closed [SupersessionRefused].
 *
 * A [SupersessionRefused] is thrown when a peer's discovery bundle carries
 * an AccountIdentity key that DIFFERS from the locally-pinned one and the
 * change cannot be authorised by a supersession proof (ADR-0016,
 * fail-closed). KT-log S4 enriches that refusal with a best-effort
 * transparency-log marker ([SupersessionRefused.loggedReset]) so the UI can
 * let a contact DISTINGUISH an owner-initiated identity RESET (the
 * lost-everything fresh-start, ADR-0017 — logged with `is_reset=true`) from
 * an unexplained key change (a possible compromise).
 *
 * The refusal itself is NEVER changed by the log — gate 1 throws regardless
 * of `loggedReset`; this only colours the message. A pure top-level
 * function (not a member) so it is unit-testable without the Compose VM.
 * Mirror of the web `supersessionRefusedMessage`.
 *
 * @param loggedReset the best-effort log marker attached to the refusal:
 *   `true` (confirmed owner reset), `false` (no owner reset → possible
 *   compromise), `null` (off / unreachable / absent / inconclusive).
 * @param peerLabel   the handle the user typed (e.g. `@alice`), rendered
 *                    verbatim as the subject; falls back to "This contact"
 *                    when `null` / empty.
 */
internal fun supersessionRefusedMessage(
    loggedReset: Boolean?,
    peerLabel: String?,
): String {
    val who = if (!peerLabel.isNullOrEmpty()) peerLabel else "This contact"
    return when (loggedReset) {
        true ->
            "$who reset their secure identity (for example, they lost " +
                "their device). The transparency log confirms this was an " +
                "owner-initiated reset. If you trust this is really them, " +
                "re-verify their safety number out of band before continuing."
        false ->
            "$who's secure identity changed, and the transparency log does " +
                "NOT show an owner reset — treat this as a possible account " +
                "compromise. Verify out of band before trusting."
        null ->
            "$who's secure identity changed and could not be confirmed " +
                "against the transparency log. Verify out of band before trusting."
    }
}
