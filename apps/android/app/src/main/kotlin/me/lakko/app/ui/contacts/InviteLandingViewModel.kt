// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.contacts.InviteAddResult
import me.lakko.app.contacts.InviteOpenResult
import me.lakko.app.contacts.InvitePreviewResult
import me.lakko.app.contacts.KnownContactAnswerResult
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

    /** The link as last opened, so the page can be decided again without
     *  fetching it again. */
    private var opened: OpenedInvite? = null

    init {
        load()
        // The contact list or the pending contacts can change while the page
        // is open (the inviter's answer to an earlier Add lands): the page's
        // decision is read again then (amended 2026-10-24).
        viewModelScope.launch {
            combine(repo.state.map { it.accepted }.distinctUntilChanged(), repo.invited) { _, _ -> }
                .collect { reevaluate() }
        }
    }

    /** Decide the page again over the link already opened. Never while Add or
     *  Open chat runs: their own writes would flip the page under them. */
    private fun reevaluate() {
        val link = opened ?: return
        val current = mutableState.value
        when (current) {
            is InviteLandingState.Ready -> if (current.adding) return
            is InviteLandingState.AlreadyContacts -> if (current.answering) return
            else -> return
        }
        val next = stateFor(repo.classifyOpenedInvite(link))
        val same = when {
            current is InviteLandingState.Ready && next is InviteLandingState.Ready -> true
            current is InviteLandingState.AlreadyContacts && next is InviteLandingState.AlreadyContacts ->
                current.target == next.target
            else -> false
        }
        if (!same) mutableState.value = next
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
            opened = when (result) {
                is InviteOpenResult.Opened -> result.invite
                is InviteOpenResult.KnownContact -> result.invite
                is InviteOpenResult.PendingInviter -> result.invite
                else -> null
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
            // ADR-0099 step 4 / UX 3 — a link from an account the moderation
            // log confirms suspended adds nothing: Add waits a few seconds for
            // the inviter's standing (nothing is spent yet) and stops on a
            // verified suspension; a slow or failed check proceeds as before.
            if (!me.lakko.app.moderation.inviteAddMayProceed(current.invite.ownerSignaturePubkey)) {
                mutableState.value = current.copy(adding = false, failed = false, inviterSuspended = true)
                return@launch
            }
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
                is InviteAddResult.Restored ->
                    mutableChatToOpen.value = InviteChatTarget.Conversation(result.groupIdHex)
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

    /**
     * **Open chat** on the page of a person already in the contacts
     * (`docs/ux/contact-invite-link.md` section 3, amended 2026-10-24): the
     * link's answer is still delivered, then the chat opens; a dead link
     * sends nothing and the chat opens. A failure keeps the page with a line
     * under the button. Runs to its end even when the screen goes away, like
     * [add]: stopped half way, the ledger use would be spent with nothing
     * sent.
     */
    public fun openChat() {
        val current = mutableState.value as? InviteLandingState.AlreadyContacts ?: return
        if (current.answering) return
        val invite = current.invite
        if (invite == null) {
            mutableChatToOpen.value = current.target
            return
        }
        mutableState.value = current.copy(answering = true, failure = null)
        viewModelScope.launch {
            val result = withContext(NonCancellable) {
                try {
                    repo.answerAsKnownContact(invite)
                } catch (err: Throwable) {
                    KnownContactAnswerResult.Failed(err.message ?: "unexpected error")
                }
            }
            when (result) {
                KnownContactAnswerResult.Sent, KnownContactAnswerResult.Dead -> {
                    mutableState.value = current.copy(answering = false, failure = null)
                    mutableChatToOpen.value = current.target
                }
                KnownContactAnswerResult.RateLimited ->
                    mutableState.value = current.copy(answering = false, failure = AnswerFailure.RATE_LIMITED)
                KnownContactAnswerResult.RequiresReauth -> mutableState.value = InviteLandingState.RequiresReauth
                is KnownContactAnswerResult.Failed -> {
                    android.util.Log.w("LakkoInvite", "WARN answer as known contact failed")
                    mutableState.value = current.copy(answering = false, failure = AnswerFailure.FAILED)
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
                invite = result.invite,
            )
            // The waiting chat; nothing is sent from it (no invite on the
            // state), since a request on a second link could give the two a
            // second one-to-one.
            is InviteOpenResult.PendingInviter -> InviteLandingState.AlreadyContacts(
                target = InviteChatTarget.Pending(result.accountIdHex),
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
    public data class Ready(
        val invite: OpenedInvite,
        val adding: Boolean,
        val failed: Boolean,
        /** ADR-0099 step 4 — Add found the inviter's account suspended. */
        val inviterSuspended: Boolean = false,
    ) : InviteLandingState

    /** The inviter is already a contact. [invite] is the opened link whose
     *  answer Open chat delivers; `answering` while it runs, [failure] after
     *  it failed (amended 2026-10-24). */
    public data class AlreadyContacts(
        val target: InviteChatTarget,
        val statedName: String?,
        val invite: OpenedInvite? = null,
        val answering: Boolean = false,
        val failure: AnswerFailure? = null,
    ) : InviteLandingState

    public data object Own : InviteLandingState

    /** Revoked, expired, used up, unknown or damaged: one state. */
    public data object Dead : InviteLandingState

    public data object RateLimited : InviteLandingState
    public data object OpenFailed : InviteLandingState
    public data object RequiresReauth : InviteLandingState
}

/** Why Open chat's answer did not go out: the line under the button. */
public enum class AnswerFailure {
    /** The ledger refused for now: the existing rate-limited text. */
    RATE_LIMITED,

    /** Any other failure: `invite.contact.page.answer_failed`. */
    FAILED,
}

/**
 * What the signed-out invite page shows (`docs/ux/contact-invite-link.md`
 * section 3, "Signed out", amended 2026-10-26). Rendered by
 * [SignedOutInviteScreen]; decided by [signedOutInviteState].
 */
public sealed interface SignedOutInviteState {
    public data object Loading : SignedOutInviteState

    /**
     * The inviter card. [statedName] is the display name, else `@username`,
     * else `null` (the screen shows `invite.contact.page.unnamed`);
     * [usernameLine] is `@username` when the title does not already say it.
     */
    public data class Live(
        val statedName: String?,
        val usernameLine: String?,
        val label: String,
        val avatarSeed: String,
    ) : SignedOutInviteState

    /** Revoked, expired, used up, unknown or damaged: one state. */
    public data object Dead : SignedOutInviteState

    public data object RateLimited : SignedOutInviteState
    public data object OpenFailed : SignedOutInviteState
}

/** The signed-out page for each result of opening the link with no session. */
public fun signedOutInviteState(result: InvitePreviewResult): SignedOutInviteState = when (result) {
    is InvitePreviewResult.Preview -> {
        val stated = result.statedName
        val username = result.username
        SignedOutInviteState.Live(
            statedName = stated,
            usernameLine = if (!username.isNullOrEmpty() && stated != "@$username") "@$username" else null,
            label = result.label,
            avatarSeed = me.lakko.app.ds.hexOf(result.ownerAccountId),
        )
    }
    InvitePreviewResult.Dead -> SignedOutInviteState.Dead
    InvitePreviewResult.RateLimited -> SignedOutInviteState.RateLimited
    InvitePreviewResult.Failed -> SignedOutInviteState.OpenFailed
}

/** The page offers **Create an account** and **I already have an account**. */
public val SignedOutInviteState.offersAccount: Boolean
    get() = this is SignedOutInviteState.Live || this == SignedOutInviteState.Dead

/**
 * Whether choosing an account keeps the link parked, so the session that
 * lands opens it again: only a live link. A dead one is dropped ("no
 * return").
 */
public val SignedOutInviteState.keepsLinkParked: Boolean
    get() = this is SignedOutInviteState.Live
