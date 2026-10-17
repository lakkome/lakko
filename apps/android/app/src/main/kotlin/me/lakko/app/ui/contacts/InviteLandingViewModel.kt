// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.contacts.InviteAddResult
import me.lakko.app.contacts.InviteOpenResult
import me.lakko.app.contacts.OpenedInvite

/**
 * State and actions of the invite page (ADR-0079,
 * `docs/ux/contact-invite-link.md` section 3). Mirror of the web
 * `routes/Invite.tsx` and the iOS `InviteLandingViewModel`.
 *
 * Opening the link spends nothing and sends nothing; **Add** spends one
 * ledger use, records the inviter as a pending contact and sends the request
 * carrying the link answer, then the page closes into the chat with the
 * inviter.
 */
public class InviteLandingViewModel(
    private val repo: ContactsRepository,
    public val slug: String,
) : ViewModel() {

    private val mutableState: MutableStateFlow<InviteLandingState> =
        MutableStateFlow(InviteLandingState.Loading)

    public val state: StateFlow<InviteLandingState> = mutableState.asStateFlow()

    private val mutableChatToOpen: MutableStateFlow<InviteChatTarget?> = MutableStateFlow(null)

    /** Set once Add has recorded the pending contact: the screen closes into
     *  that chat. */
    public val chatToOpen: StateFlow<InviteChatTarget?> = mutableChatToOpen.asStateFlow()

    init {
        load()
    }

    /** Open again after a refusal or a network failure. */
    public fun retryOpen() {
        mutableState.value = InviteLandingState.Loading
        load()
    }

    private fun load() {
        viewModelScope.launch {
            val result = try {
                repo.openInviteLink(slug)
            } catch (err: Throwable) {
                me.lakko.app.rethrowIfCancelled(err)
                android.util.Log.w("LakkoInvite", "WARN open invite threw: ${err.javaClass.simpleName}")
                InviteOpenResult.Failed(err.message ?: "unexpected error")
            }
            if (result is InviteOpenResult.Failed) {
                android.util.Log.w("LakkoInvite", "WARN open invite failed")
            }
            mutableState.value = stateFor(result)
        }
    }

    /** **Add**. Runs to its end even when the screen goes away: stopped half
     *  way, the ledger use would be spent with no request sent. */
    public fun add() {
        val current = mutableState.value as? InviteLandingState.Ready ?: return
        if (current.adding) return
        mutableState.value = current.copy(adding = true, failed = false)
        viewModelScope.launch {
            val result = withContext(NonCancellable) {
                try {
                    repo.addThroughInviteLink(current.invite)
                } catch (err: Throwable) {
                    InviteAddResult.Failed(err.message ?: "unexpected error")
                }
            }
            when (result) {
                is InviteAddResult.Added ->
                    mutableChatToOpen.value = InviteChatTarget.Pending(result.contact.accountIdHex)
                InviteAddResult.Dead -> mutableState.value = InviteLandingState.Dead
                InviteAddResult.RateLimited -> mutableState.value = current.copy(adding = false, failed = true)
                InviteAddResult.RequiresReauth -> mutableState.value = InviteLandingState.RequiresReauth
                is InviteAddResult.Failed -> {
                    android.util.Log.w("LakkoInvite", "WARN add through invite failed")
                    mutableState.value = current.copy(adding = false, failed = true)
                }
            }
        }
    }

    public companion object {
        /** What the page shows for each result of opening the link. */
        public fun stateFor(result: InviteOpenResult): InviteLandingState = when (result) {
            is InviteOpenResult.Opened -> InviteLandingState.Ready(result.invite, adding = false, failed = false)
            InviteOpenResult.Dead -> InviteLandingState.Dead
            InviteOpenResult.Own -> InviteLandingState.Own
            is InviteOpenResult.KnownContact -> InviteLandingState.AlreadyContacts(
                target = result.groupIdHex?.let { InviteChatTarget.Conversation(it) }
                    ?: InviteChatTarget.Contact(result.routeKey),
                statedName = result.statedName,
            )
            InviteOpenResult.RateLimited -> InviteLandingState.RateLimited
            InviteOpenResult.RequiresReauth -> InviteLandingState.RequiresReauth
            is InviteOpenResult.Failed -> InviteLandingState.OpenFailed
        }
    }
}

/** Where the invite page closes into. */
public sealed interface InviteChatTarget {
    /** The chat with a person added through their link whose app has not
     *  answered yet. */
    public data class Pending(val accountIdHex: String) : InviteChatTarget

    /** The one-to-one with an established contact. */
    public data class Conversation(val groupIdHex: String) : InviteChatTarget

    /** An established contact this device holds no conversation with. */
    public data class Contact(val routeKey: String) : InviteChatTarget
}

/** Discriminated state the [InviteLandingScreen] renders. */
public sealed interface InviteLandingState {
    public data object Loading : InviteLandingState

    /** The default page; `adding` while Add runs, `failed` after a network
     *  failure (the line under the button). */
    public data class Ready(val invite: OpenedInvite, val adding: Boolean, val failed: Boolean) : InviteLandingState

    public data class AlreadyContacts(val target: InviteChatTarget, val statedName: String?) : InviteLandingState

    public data object Own : InviteLandingState

    /** Revoked, expired, used up, unknown or damaged: one state. */
    public data object Dead : InviteLandingState

    public data object RateLimited : InviteLandingState
    public data object OpenFailed : InviteLandingState
    public data object RequiresReauth : InviteLandingState
}
