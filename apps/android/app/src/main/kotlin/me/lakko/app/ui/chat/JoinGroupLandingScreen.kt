// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.GROUP_INVITE_INVALID_COPY
import me.lakko.app.chat.GROUP_INVITE_REQUEST_TO_JOIN
import me.lakko.app.chat.GroupInviteCard
import me.lakko.app.chat.groupInvitePageLines
import me.lakko.app.chat.invitedByLine

/**
 * ADR-0028 (G-3) — drives the `/g/<slug>` group join-ticket landing. Decodes +
 * verifies the capability and opens its sealed preview (ADR-0028 v2,
 * `docs/ux/group-invite-page.md`), shows the card, and on `Request to join`
 * posts an external-join proposal (ADR-0027), then polls until the rendezvous
 * Welcome lands.
 *
 * **The preview is shown before anyone taps.** The link secret rides in the
 * URL fragment; a chat app that strips fragments or a hand-typed URL without
 * one lands on the page without a preview (`You are invited to a group`) —
 * nothing invents a name and the page does not say why. A fragment that IS
 * present but malformed, or whose preview does not open, refuses the link as
 * damaged (ADR-0028 amendment of 2026-09-27): the ledger id is no longer a hash
 * of the capability, and a secret that opens nothing is how a capability
 * swapped in under a live id is noticed. A capability that fails to decode,
 * verify or is past its expiry is the invalid state; a `429` from the ledger
 * is "try again later", never "invalid".
 *
 * **The wait is TWO-stage since ADR-0045 S7, and the copy says so.** An admin's
 * approve no longer admits anyone by itself: it publishes an `Add` proposal, and
 * the rendezvous appears only once a committer sweeps that proposal into a
 * commit — a step ADR-0045 decision 2 gives no deadline. So this poll spans both
 * stages, and promising immediacy here would be the optimistic-update lie the UX
 * spec forbids.
 *
 * ## What this side is NOT told (ADR-0045 S7 residual 1)
 *
 * A sweep can exclude the approval-derived `Add` — `narrow_sweep_by_exclusion`
 * drops whatever refuses to build, and the commonest cause needs no adversary at
 * all (an expired KeyPackage, reachable because a request has no deadline). This
 * screen is never told when that happens, and the reason is structural rather
 * than an omission: there is no artifact addressed to a non-member, and the
 * journal is unauthenticated — the DS pseudonym IS the `group_id` and there is
 * no auth on it — so any "your request was not carried" envelope would be a
 * forgeable denial primitive. Inference does not work either: an epoch advancing
 * with no rendezvous for us is byte-for-byte what *no admin has approved yet*
 * looks like, which is the ordinary state.
 *
 * So the wait copy states the truth about the wait, and the screen offers the
 * one repair this side has: `Request again`, user-driven, which mints a fresh
 * KeyPackage. It never claims that is why.
 */
public class JoinGroupLandingViewModel(
    private val repository: ChatRepository,
    private val slug: String,
    /** The URL fragment: the 12-byte link secret, or absent / malformed (the
     *  link is refused as damaged; nothing opens without the secret). */
    private val fragment: me.lakko.app.discovery.InviteLinkFragment,
) : ViewModel() {

    /** Local FSM: Loading -> Invalid | Ready -> Requesting -> Waiting (poll
     *  loop) -> Joined / Failed. */
    public sealed interface State {
        /** Resolving the slug and opening the preview. */
        public data object Loading : State
        /** The capability does not decode, verify, or has expired. */
        public data object Invalid : State
        /** The link has no usable secret, or the blob does not open under it. */
        public data object Damaged : State
        /** The ledger answered `429`; the link may be fine. */
        public data object RateLimited : State
        /** The card is on screen; nothing has been sent yet. */
        public data object Ready : State
        public data object Requesting : State
        public data object Waiting : State
        public data class Joined(val groupIdHex: String) : State
        /** [message] is one of the fixed sentences [me.lakko.app.chat.joinFailureCopy]
         *  returns — never a raw or protocol message. */
        public data class Failed(val message: String) : State
    }

    private val mutableState: MutableStateFlow<State> = MutableStateFlow(State.Loading)
    public val state: StateFlow<State> = mutableState.asStateFlow()

    private val mutableCard: MutableStateFlow<GroupInviteCard?> = MutableStateFlow(null)

    /** The card's facts once the link is decoded: fingerprint, expiry, the
     *  preview or `null`. Stays on screen through the whole wait. */
    public val card: StateFlow<GroupInviteCard?> = mutableCard.asStateFlow()

    private var started = false

    /** The resolved capability wire + the ledger blob id (pointer form only),
     *  kept so `Request again` does not re-resolve the slug. */
    private var resolved: Pair<ByteArray, ByteArray?>? = null

    private val mutableRequesting: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** In flight, so `Request again` cannot be double-tapped into two journal
     *  rows. Separate from [state], which STAYS `Waiting` across a re-request:
     *  going back through `Requesting` would hide the wait copy and read as a
     *  restart of something that never stopped. */
    public val requesting: StateFlow<Boolean> = mutableRequesting.asStateFlow()

    private var pollJob: kotlinx.coroutines.Job? = null

    /** Resolve + decode the link exactly once and show the card. */
    public fun start() {
        if (started) return
        started = true
        if (fragment !is me.lakko.app.discovery.InviteLinkFragment.Secret) {
            // Refused before any network: no fragment, the old 43-character
            // key, a truncated secret, anything that is not the 16-character
            // secret (sealed-link amendment of 2026-09-28).
            mutableState.value = State.Damaged
            return
        }
        viewModelScope.launch {
            // A tapped link starts (or recreates) the activity, whose shell
            // bootstraps a fresh repository on its own dispatcher while the
            // router already shows this screen. Resolving before that has
            // finished used to surface the repository's own guard - "not
            // bootstrapped" - as the landing's error, so a link tapped from a
            // browser could never join. Wait for the identity instead; the
            // shell's loading state is what the person sees meanwhile.
            repository.state.first { it.bootstrapped }
            val nowMs = System.currentTimeMillis().toULong()
            try {
                // ADR-0028 OQ1 — resolve the slug (a 6-byte ledger-id
                // pointer => fetch the sealed blob from the ledger; an inline
                // blob => pass through) and open it under the secret.
                val wireAndBlob = repository.resolveGroupInviteSlug(slug, fragment)
                val card = repository.inspectGroupInvite(wireAndBlob.first, fragment, nowMs)
                if (!card.valid) {
                    mutableState.value = State.Invalid
                    return@launch
                }
                resolved = wireAndBlob
                mutableCard.value = card
                mutableState.value = State.Ready
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                // A slug that does not resolve, a ledger row that is gone, a
                // capability that does not verify: all one state for the
                // person (spec section 3, `Invalid or expired link`). A
                // damaged fragment and a 429 are their own states.
                android.util.Log.w("JoinGroupLanding", "invite link did not open", e)
                mutableState.value = when (me.lakko.app.chat.groupInviteOpenFailure(e)) {
                    me.lakko.app.chat.GroupInviteOpenFailure.Invalid -> State.Invalid
                    me.lakko.app.chat.GroupInviteOpenFailure.Damaged -> State.Damaged
                    me.lakko.app.chat.GroupInviteOpenFailure.RateLimited -> State.RateLimited
                }
            }
        }
    }

    /** `Try again` after a rate-limited open: resolve the link once more. */
    public fun retryOpen() {
        if (mutableState.value != State.RateLimited) return
        mutableState.value = State.Loading
        started = false
        start()
    }

    /**
     * ADR-0045 S7 residual 1 — send the request. Reached from the card's
     * `Request to join` and again from the `Request again` control, which is
     * the joiner's only repair: a fresh request mints a fresh KeyPackage.
     * Deliberately user-driven and never automatic — each press is a journal
     * row held to the DS's 30-day TTL, and a timer would spin against a group
     * that is merely quiet.
     */
    public fun request() {
        if (mutableRequesting.value) return
        val ticket = resolved ?: return
        mutableRequesting.value = true
        // Only the first request passes through `Requesting` (and a `Try
        // again` after a failure); a re-request keeps the wait copy on screen.
        val before = mutableState.value
        if (before == State.Ready || before is State.Failed) mutableState.value = State.Requesting
        // A re-request replaces the poll rather than adding a second one.
        pollJob?.cancel()
        pollJob = viewModelScope.launch {
            val nowMs = System.currentTimeMillis().toULong()
            val groupId: ByteArray
            val joinerKey: ByteArray
            try {
                // Request to join, registering a consume against the ledger
                // when the link was a pointer.
                val result = repository.requestToJoinFromLink(
                    ticket.first,
                    nowMs,
                    ticket.second,
                )
                groupId = result.first
                joinerKey = result.second
            } catch (e: kotlinx.coroutines.CancellationException) {
                mutableRequesting.value = false
                throw e
            } catch (e: Throwable) {
                // The raw cause goes to the log; the person reads one of the
                // fixed sentences (docs/ux/group-invite-page.md).
                android.util.Log.w("JoinGroupLanding", "join request failed", e)
                mutableState.value = State.Failed(me.lakko.app.chat.joinFailureCopy(e))
                mutableRequesting.value = false
                return@launch
            }
            mutableState.value = State.Waiting
            // Cleared as soon as the request itself is on the wire: the poll
            // below runs for the rest of the wait, and holding the flag across
            // it would disable `Request again` forever.
            mutableRequesting.value = false
            val groupIdHex = me.lakko.app.ds.DsClient.pseudonymHex(groupId)
            while (isActive) {
                // ADR-0053 — the sweep delivers the Welcome to this device's
                // mailbox only; the app-wide mailbox poll may join it before
                // the poll below runs, so the group already being ours is the
                // first thing checked.
                if (repository.state.value.threads.any { it.groupIdHex.equals(groupIdHex, ignoreCase = true) }) {
                    mutableState.value = State.Joined(groupIdHex)
                    return@launch
                }
                try {
                    val hex = repository.pollForJoinWelcome(groupId, joinerKey)
                    if (hex != null) {
                        mutableState.value = State.Joined(hex)
                        return@launch
                    }
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    // One missed mailbox pass is not a failed join: the
                    // request is on the server and the next pass (or the
                    // app-wide mailbox poll) still joins the Welcome.
                    android.util.Log.w("JoinGroupLanding", "mailbox pass failed; waiting on", e)
                }
                delay(3_000)
            }
        }
    }
}

/** The landing view for a tapped `/g/<slug>` group join-ticket link. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun JoinGroupLandingScreen(
    viewModel: JoinGroupLandingViewModel,
    onDone: () -> Unit,
    /** Opens the joined group's chat. */
    onOpenChat: (groupIdHex: String) -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val card by viewModel.card.collectAsStateWithLifecycle()
    val requesting by viewModel.requesting.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { viewModel.start() }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Join group") },
                navigationIcon = {
                    IconButton(onClick = onDone) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(padding)
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        when (val s = state) {
            is JoinGroupLandingViewModel.State.Loading -> {
                CircularProgressIndicator()
                Spacer(Modifier.height(8.dp))
                Text("Loading invite…")
            }
            is JoinGroupLandingViewModel.State.Invalid -> {
                Text(GROUP_INVITE_INVALID_COPY, textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                Button(onClick = onDone) { Text("Open Lakko") }
            }
            is JoinGroupLandingViewModel.State.Damaged -> {
                Text(me.lakko.app.chat.GROUP_INVITE_DAMAGED_COPY, textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                Button(onClick = onDone) { Text("Open Lakko") }
            }
            is JoinGroupLandingViewModel.State.RateLimited -> {
                Text(me.lakko.app.discovery.INVITE_RATE_LIMITED_COPY, textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                Button(onClick = { viewModel.retryOpen() }) { Text(me.lakko.app.chat.JOIN_TRY_AGAIN) }
            }
            is JoinGroupLandingViewModel.State.Ready -> {
                InviteHeadline(card)
                Spacer(Modifier.height(16.dp))
                Button(onClick = { viewModel.request() }) { Text(GROUP_INVITE_REQUEST_TO_JOIN) }
                InviteFoot(card)
            }
            is JoinGroupLandingViewModel.State.Requesting -> {
                InviteHeadline(card)
                Spacer(Modifier.height(16.dp))
                CircularProgressIndicator()
                Spacer(Modifier.height(8.dp))
                Text("Sending request…")
                InviteFoot(card)
            }
            is JoinGroupLandingViewModel.State.Waiting -> {
                InviteHeadline(card)
                Spacer(Modifier.height(16.dp))
                CircularProgressIndicator()
                Spacer(Modifier.height(8.dp))
                Text(me.lakko.app.chat.JOIN_WAIT_PRIMARY, textAlign = TextAlign.Center)
                Spacer(Modifier.height(4.dp))
                Text(
                    text = me.lakko.app.chat.JOIN_WAIT_SECONDARY,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
                Spacer(Modifier.height(12.dp))
                Button(
                    onClick = { viewModel.request() },
                    enabled = !requesting,
                ) { Text(me.lakko.app.chat.JOIN_REQUEST_AGAIN) }
                InviteFoot(card)
            }
            is JoinGroupLandingViewModel.State.Joined -> {
                Text(me.lakko.app.chat.JOIN_SUCCESS_COPY, textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                Button(onClick = { onOpenChat(s.groupIdHex) }) { Text(me.lakko.app.chat.JOIN_OPEN_CHAT) }
            }
            is JoinGroupLandingViewModel.State.Failed -> {
                InviteHeadline(card)
                Spacer(Modifier.height(16.dp))
                Text(s.message, color = MaterialTheme.colorScheme.error, textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                // "Try again" only with the general failure; the other two
                // sentences ask the person to wait or fix the clock first.
                if (me.lakko.app.chat.joinFailureOffersRetry(s.message)) {
                    Button(
                        onClick = { viewModel.request() },
                        enabled = !requesting,
                    ) { Text(me.lakko.app.chat.JOIN_TRY_AGAIN) }
                } else {
                    Button(onClick = onDone) { Text("Close") }
                }
                InviteFoot(card)
            }
        }
    }
    }
}

/**
 * The card's headline (spec section 3): `<Inviter> invites you to` /
 * `You are invited to`, the group name (or `a group` folded into the
 * headline), and `1 member` / `N members` — each line only when the preview
 * supplies it.
 */
@Composable
private fun InviteHeadline(card: GroupInviteCard?) {
    val lines = groupInvitePageLines(card?.preview)
    Text(
        text = lines.headline,
        style = MaterialTheme.typography.titleMedium,
        textAlign = TextAlign.Center,
        modifier = Modifier.fillMaxWidth(),
    )
    lines.groupLine?.let { name ->
        Spacer(Modifier.height(4.dp))
        Text(
            text = name,
            style = MaterialTheme.typography.headlineSmall,
            textAlign = TextAlign.Center,
            modifier = Modifier.fillMaxWidth(),
        )
    }
    lines.membersLine?.let { members ->
        Spacer(Modifier.height(4.dp))
        Text(
            text = members,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/** The muted foot: `Invited by <fingerprint>` — the one thing on the page the
 *  inviter cannot fake, kept so digits told out of band can be compared. */
@Composable
private fun InviteFoot(card: GroupInviteCard?) {
    val fingerprint = card?.fingerprint ?: return
    Spacer(Modifier.height(16.dp))
    Text(
        text = invitedByLine(fingerprint),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
        modifier = Modifier.fillMaxWidth(),
    )
}
