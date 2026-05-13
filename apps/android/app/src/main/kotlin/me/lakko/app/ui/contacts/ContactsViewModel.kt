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

    /** VM-internal slice carrying ephemeral form + send state.
     *  Persistent contacts data lives in the repository. */
    private data class UiSlice(
        val usernameInput: String = "",
        val introInput: String = "",
        val sending: Boolean = false,
        val sendError: String? = null,
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
    val refreshing: Boolean,
    val refreshError: String?,
    val requiresReauth: Boolean,
)
