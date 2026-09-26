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
 * **The preview is shown before anyone taps, and a missing one is not an
 * error.** The link key rides in the URL fragment; a chat app that strips
 * fragments, a hand-typed URL, a link minted without a preview or a key the
 * preview does not open under all land on the same page without a preview
 * (`You are invited to a group`) — nothing invents a name and the page does
 * not say why. Only a capability that fails to decode, verify or is past its
 * expiry is the invalid state.
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
    /** The 32-byte link key from the URL fragment, or `null` when the tapped
     *  URL had none (or one of the wrong shape) — the page then shows no
     *  preview. */
    private val linkKey: ByteArray?,
) : ViewModel() {

    /** Local FSM: Loading -> Invalid | Ready -> Requesting -> Waiting (poll
     *  loop) -> Joined / Failed. */
    public sealed interface State {
        /** Resolving the slug and opening the preview. */
        public data object Loading : State
        /** The capability does not decode, verify, or has expired. */
        public data object Invalid : State
        /** The card is on screen; nothing has been sent yet. */
        public data object Ready : State
        public data object Requesting : State
        public data object Waiting : State
        public data class Joined(val groupIdHex: String) : State
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
                // ADR-0028 OQ1 — resolve the slug (a 32-byte content-addressed
                // pointer => fetch the capability from the ledger; an inline
                // capability => pass through).
                val wireAndBlob = repository.resolveGroupInviteSlug(slug)
                val card = repository.inspectGroupInvite(wireAndBlob.first, linkKey, nowMs)
                if (!card.valid) {
                    mutableState.value = State.Invalid
                    return@launch
                }
                resolved = wireAndBlob
                mutableCard.value = card
                mutableState.value = State.Ready
            } catch (e: Throwable) {
                // A slug that does not resolve, a ledger row that is gone, a
                // capability that does not verify: all one state for the
                // person (spec section 3, `Invalid or expired link`).
                android.util.Log.w("JoinGroupLanding", "invite link did not open", e)
                mutableState.value = State.Invalid
            }
        }
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
        // Only the first request passes through `Requesting`; a re-request
        // keeps the wait copy on screen.
        if (mutableState.value == State.Ready) mutableState.value = State.Requesting
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
            } catch (e: Throwable) {
                mutableState.value = State.Failed(e.message ?: e.toString())
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
                // The sweep publishes the Welcome twice: to the group journal
                // as a rendezvous, which the poll below reads, and to the
                // joiner's mailbox, which the inbox poll reads (the sweep
                // cannot tell a link joiner from a contact). When the mailbox
                // copy lands first, the group is already ours and the
                // rendezvous can never open again - its KeyPackage is spent -
                // so without this check the landing waited forever on a join
                // that had happened.
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
                } catch (e: Throwable) {
                    mutableState.value = State.Failed(e.message ?: e.toString())
                    return@launch
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
                Text("You joined the group. Find it under Groups.")
                Spacer(Modifier.height(12.dp))
                Button(onClick = onDone) { Text("Done") }
            }
            is JoinGroupLandingViewModel.State.Failed -> {
                Text(s.message, color = MaterialTheme.colorScheme.error, textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                Button(onClick = onDone) { Text("Close") }
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
