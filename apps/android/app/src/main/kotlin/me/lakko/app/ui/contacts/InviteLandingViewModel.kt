// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import me.lakko.app.contacts.AcceptInviteResult
import me.lakko.app.contacts.ContactsRepository

/**
 * State machine for the [InviteLandingScreen] (1.15e11b).
 *
 * Constructed per-slug — keyed by `invite-${slug}` on the
 * Activity-side `viewModel(key = ...)` factory so re-tapping a
 * different invite URL builds a fresh VM with the right slug
 * binding. The VM auto-runs [ContactsRepository.acceptInviteLink]
 * on `init` so the screen immediately shows a spinner + lands
 * on the result state on its own; no user gesture needed.
 *
 * **Phase 1.9b4 (ADR-0005 §13.4) note:** the auto-run covers
 * every invite, including `bootstrap_referral` referrals — the
 * tap on the deep-link is the consent gesture; requiring a
 * second tap on Accept would be redundant. The web client keeps
 * an explicit Accept button for non-referral invites (lets the
 * user attach an intro) and only auto-accepts
 * `bootstrap_referral=true` invites for parity.
 *
 * The discriminated [InviteLandingState] mirrors the underlying
 * [AcceptInviteResult] verbatim — same eight terminal variants
 * (Accepted / AlreadyAccepted / Expired / Exhausted / NotFound /
 * MalformedSlug / RequiresReauth / Failed) plus a `Loading`
 * initial state. The composable matches on the variant + renders
 * the right copy + the right primary affordance (Open chat /
 * Done / Retry).
 *
 * **Retry** (only meaningful for [InviteLandingState.Failed] —
 * transport-level transient errors) re-runs the full
 * `acceptInviteLink` orchestrator from scratch; the server-side
 * `(blob_id, joiner_tag)` dedup table handles the case where a
 * prior attempt already mutated state on the consume step.
 */
public class InviteLandingViewModel(
    private val repo: ContactsRepository,
    public val slug: String,
) : ViewModel() {

    private val mutableState: MutableStateFlow<InviteLandingState> =
        MutableStateFlow(InviteLandingState.Loading)

    public val state: StateFlow<InviteLandingState> = mutableState.asStateFlow()

    init {
        runAccept()
    }

    /** Re-run the full Accept orchestration. Called from the
     *  Retry button on a [InviteLandingState.Failed] state. */
    public fun retry() {
        if (mutableState.value !is InviteLandingState.Failed) return
        mutableState.value = InviteLandingState.Loading
        runAccept()
    }

    private fun runAccept() {
        viewModelScope.launch {
            val result = try {
                repo.acceptInviteLink(slug)
            } catch (err: Throwable) {
                // ContactsRepository.acceptInviteLink already
                // catches its internal failures into the
                // Failed / NotFound / etc. variants — any throw
                // that bubbles up here is a genuinely
                // unexpected error (programmer bug, OOM). Land
                // it on Failed with the message.
                mutableState.value = InviteLandingState.Failed(
                    message = err.message ?: "unexpected error",
                )
                return@launch
            }
            mutableState.value = mapResult(result)
        }
    }

    private fun mapResult(result: AcceptInviteResult): InviteLandingState =
        when (result) {
            is AcceptInviteResult.Accepted -> InviteLandingState.Accepted(
                recipientAccountId = result.recipientAccountId,
                groupIdHex = result.groupIdHex,
            )
            AcceptInviteResult.AlreadyAccepted -> InviteLandingState.AlreadyAccepted
            AcceptInviteResult.Expired -> InviteLandingState.Expired
            AcceptInviteResult.Exhausted -> InviteLandingState.Exhausted
            AcceptInviteResult.NotFound -> InviteLandingState.NotFound
            AcceptInviteResult.MalformedSlug -> InviteLandingState.MalformedSlug
            AcceptInviteResult.RequiresReauth -> InviteLandingState.RequiresReauth
            is AcceptInviteResult.Failed -> InviteLandingState.Failed(result.message)
        }
}

/** Discriminated state the [InviteLandingScreen] renders. */
public sealed interface InviteLandingState {
    public data object Loading : InviteLandingState

    public data class Accepted(
        val recipientAccountId: ByteArray,
        /** Non-null when the MLS handshake succeeded; the UI
         *  surfaces an `Open chat` affordance pointing at the
         *  new thread. */
        val groupIdHex: String?,
    ) : InviteLandingState {
        override fun equals(other: Any?): Boolean =
            other is Accepted &&
                recipientAccountId.contentEquals(other.recipientAccountId) &&
                groupIdHex == other.groupIdHex
        override fun hashCode(): Int =
            recipientAccountId.contentHashCode() * 31 + (groupIdHex?.hashCode() ?: 0)
    }

    public data object AlreadyAccepted : InviteLandingState
    public data object Expired : InviteLandingState
    public data object Exhausted : InviteLandingState
    public data object NotFound : InviteLandingState
    public data object MalformedSlug : InviteLandingState
    public data object RequiresReauth : InviteLandingState

    public data class Failed(val message: String) : InviteLandingState
}
