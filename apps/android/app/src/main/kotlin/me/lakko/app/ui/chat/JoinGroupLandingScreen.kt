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
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
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
import me.lakko.app.chat.GROUP_INVITE_INVALID_TEXT
import me.lakko.app.chat.GROUP_INVITE_REQUEST_TO_JOIN_TEXT
import me.lakko.app.chat.GroupInviteCard
import me.lakko.app.chat.groupInvitePageLines
import me.lakko.app.chat.invitedByLine
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon

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
 * So the wait copy states the truth about the wait and nothing more (UX
 * `group-and-channel-privacy.md` 6.2, amended 2026-11-12): "Request sent. An
 * admin will see it when they next open Lakko." and no control. A device asks
 * once per group per day: the page opened again within the day finds the
 * recorded request and shows the same line; after a day it shows "No answer
 * yet." and `Ask again`, which sends the same request again exactly as the
 * `Requests sent` list's `Ask again` does ([ChatRepository.askAgain]) and
 * restarts the day. A fresh request mints a fresh KeyPackage, the one repair
 * this side has; the page never claims that is why. When the join lands the
 * page moves straight to the chat.
 */
public class JoinGroupLandingViewModel(
    private val repository: ChatRepository,
    private val slug: String,
    /** The URL fragment: the 12-byte link secret, or absent / malformed (the
     *  link is refused as damaged: sealed links open nothing without it). */
    private val fragment: me.lakko.app.chat.GroupInviteFragment,
) : ViewModel() {

    /** Local FSM: Loading -> Invalid | Ready -> Requesting -> Waiting (poll
     *  loop) -> Joined / Failed. Opened again for a group this device already
     *  asked to join: Loading -> Waiting; for a group it holds: -> Joined. */
    public sealed interface State {
        /** Resolving the slug and opening the preview. */
        public data object Loading : State
        /** The capability does not decode, verify, or has expired. */
        public data object Invalid : State
        /** The link's fragment is missing or malformed, or the link or its
         *  preview does not open under it. */
        public data object Damaged : State
        /** The ledger answered `429`; the link may be fine. */
        public data object RateLimited : State

        /** The ledger could not be asked (the network, a server error): the
         *  link may be fine, the page offers it again. */
        public data object OpenFailed : State
        /** The card is on screen; nothing has been sent yet. */
        public data object Ready : State
        public data object Requesting : State

        /** A request was sent at [sentAtMs] (Unix ms) and the group has not
         *  appeared: the wait line, then after a day `Ask again`
         *  ([joinWaitOffersAskAgain]). */
        public data class Waiting(val sentAtMs: Long) : State

        /** The group is here: the page moves to its chat ([groupToOpenOnJoin]). */
        public data class Joined(val groupIdHex: String) : State
        /** [message] is one of the fixed sentences [me.lakko.app.chat.joinFailureCopy]
         *  returns — never a raw or protocol message. */
        public data class Failed(val message: me.lakko.app.l10n.UiText) : State
    }

    private val mutableState: MutableStateFlow<State> = MutableStateFlow(State.Loading)
    public val state: StateFlow<State> = mutableState.asStateFlow()

    private val mutableCard: MutableStateFlow<GroupInviteCard?> = MutableStateFlow(null)

    /** The card's facts once the link is decoded: fingerprint, expiry, the
     *  preview or `null`. Stays on screen through the whole wait. */
    public val card: StateFlow<GroupInviteCard?> = mutableCard.asStateFlow()

    private var started = false

    /** The OPENED capability's plaintext wire + the ledger blob id (pointer
     *  form only), kept so `Request again` does not re-resolve the slug. */
    private var resolved: Pair<ByteArray, ByteArray>? = null

    private val mutableRequesting: MutableStateFlow<Boolean> = MutableStateFlow(false)

    /** In flight, so `Ask again` cannot be double-tapped into two journal
     *  rows. Separate from [state], which STAYS `Waiting` across an ask
     *  again: going back through `Requesting` would hide the wait copy and
     *  read as a restart of something that never stopped. */
    public val requesting: StateFlow<Boolean> = mutableRequesting.asStateFlow()

    private var pollJob: kotlinx.coroutines.Job? = null

    /** Resolve + decode the link exactly once and show the card. */
    public fun start() {
        if (started) return
        started = true
        if (fragment !is me.lakko.app.chat.GroupInviteFragment.Secret) {
            // Refused before any network: no fragment at all (sealed links
            // open nothing without it), the old 43-character key, a truncated
            // secret, anything that is not the 16-character secret.
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
                // ADR-0028 OQ1 — resolve the slug (a 6-byte ledger id: fetch
                // the sealed link from the ledger; an inline slug, the removed
                // form, is invalid), then open it under the fragment's secret.
                val sealedAndBlob = repository.resolveGroupInviteSlug(slug)
                val card = repository.inspectGroupInvite(sealedAndBlob.first, fragment, nowMs)
                if (!card.valid) {
                    mutableState.value = State.Invalid
                    return@launch
                }
                resolved = Pair(card.capabilityWire, sealedAndBlob.second)
                mutableCard.value = card
                // UX 6.2 — a group this device already asked to join (within
                // the day or not) shows the wait, never a second `Request to
                // join`; a group it holds opens. Channel links are not group
                // requests and keep their own page.
                val groupLink = card.preview?.channelId == null && card.groupIdHex.isNotEmpty()
                val opened = if (groupLink) {
                    joinPageStateOnOpen(
                        holdsGroup = repository.holdsGroup(card.groupIdHex),
                        sentAtMs = repository.openSentRequest(card.groupIdHex)?.sentAtMs,
                        groupIdHex = card.groupIdHex,
                    )
                } else {
                    State.Ready
                }
                mutableState.value = opened
                if (opened is State.Waiting) startPolling(card.groupIdHex, null)
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
                    me.lakko.app.chat.GroupInviteOpenFailure.OpenFailed -> State.OpenFailed
                }
            }
        }
    }

    /** `Try again` after a rate-limited or failed open: resolve the link
     *  once more. */
    public fun retryOpen() {
        if (mutableState.value != State.RateLimited && mutableState.value != State.OpenFailed) return
        mutableState.value = State.Loading
        started = false
        start()
    }

    /**
     * ADR-0045 S7 residual 1 — send the request: the card's `Request to join`
     * and `Try again` after a failure. Recorded under `Requests sent` (UX 6.2)
     * so the list shows it and a reopened page shows the wait. Deliberately
     * user-driven and never automatic — each press is a journal row held to
     * the DS's 30-day TTL, and a timer would spin against a group that is
     * merely quiet.
     */
    public fun request() {
        if (mutableRequesting.value) return
        val ticket = resolved ?: return
        mutableRequesting.value = true
        mutableState.value = State.Requesting
        pollJob?.cancel()
        pollJob = viewModelScope.launch {
            val nowMs = System.currentTimeMillis()
            val result = try {
                repository.requestToJoinFromLinkRecorded(
                    ticket.first,
                    nowMs.toULong(),
                    ticket.second,
                    mutableCard.value?.preview?.groupName,
                )
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
            mutableState.value = State.Waiting(nowMs)
            mutableRequesting.value = false
            pollUntilJoined(me.lakko.app.ds.DsClient.pseudonymHex(result.first), result.first, result.second)
        }
    }

    /**
     * UX 6.2 — `Ask again`, offered a day after the request: the same request
     * again through [ChatRepository.askAgain], exactly as the `Requests sent`
     * list sends it; the day starts over.
     */
    public fun askAgain() {
        if (mutableRequesting.value) return
        val ticket = resolved ?: return
        val groupIdHex = mutableCard.value?.groupIdHex?.takeIf { it.isNotEmpty() } ?: return
        mutableRequesting.value = true
        pollJob?.cancel()
        pollJob = viewModelScope.launch {
            val encoder = java.util.Base64.getEncoder()
            // The stored row, or (when the list already dropped it) the same
            // request rebuilt from the link this page opened.
            val row = repository.openSentRequest(groupIdHex) ?: me.lakko.app.chat.SentRequest(
                idHex = groupIdHex,
                name = mutableCard.value?.preview?.groupName.orEmpty(),
                handle = "",
                sentAtMs = 0L,
                capabilityB64 = encoder.encodeToString(ticket.first),
                blobIdB64 = encoder.encodeToString(ticket.second),
            )
            try {
                repository.askAgain(row)
            } catch (e: kotlinx.coroutines.CancellationException) {
                mutableRequesting.value = false
                throw e
            } catch (e: Throwable) {
                android.util.Log.w("JoinGroupLanding", "ask again failed", e)
                mutableState.value = State.Failed(me.lakko.app.chat.joinFailureCopy(e))
                mutableRequesting.value = false
                return@launch
            }
            mutableState.value = State.Waiting(System.currentTimeMillis())
            mutableRequesting.value = false
            pollUntilJoined(groupIdHex, hexBytesOrNull(groupIdHex), null)
        }
    }

    private fun startPolling(groupIdHex: String, joinerKey: ByteArray?) {
        pollJob?.cancel()
        pollJob = viewModelScope.launch { pollUntilJoined(groupIdHex, hexBytesOrNull(groupIdHex), joinerKey) }
    }

    /** Wait for the group to appear, then [State.Joined]. */
    private suspend fun pollUntilJoined(groupIdHex: String, groupId: ByteArray?, joinerKey: ByteArray?) {
        while (viewModelScope.isActive) {
            // ADR-0053 — the sweep delivers the Welcome to this account's
            // mailbox only; the app-wide mailbox poll may join it before the
            // pass below runs, so the group already being ours is the first
            // thing checked.
            if (repository.state.value.threads.any { it.groupIdHex.equals(groupIdHex, ignoreCase = true) }) {
                mutableState.value = State.Joined(groupIdHex)
                return
            }
            if (groupId != null) {
                try {
                    val hex = repository.pollForJoinWelcome(groupId, joinerKey ?: ByteArray(0))
                    if (hex != null) {
                        mutableState.value = State.Joined(hex)
                        return
                    }
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    // One missed pass is not a failed join: the request is on
                    // the server and the next pass (or the app-wide mailbox
                    // poll) still joins the Welcome.
                    android.util.Log.w("JoinGroupLanding", "mailbox pass failed; waiting on", e)
                }
            }
            delay(3_000)
        }
    }
}

/**
 * UX 6.2 — what the page shows once the link opened, for a group link: the
 * chat when this device holds the group, the wait when it already asked
 * ([sentAtMs] the request's time), else the card with `Request to join`. Pure.
 */
internal fun joinPageStateOnOpen(
    holdsGroup: Boolean,
    sentAtMs: Long?,
    groupIdHex: String,
): JoinGroupLandingViewModel.State = when {
    holdsGroup -> JoinGroupLandingViewModel.State.Joined(groupIdHex)
    sentAtMs != null -> JoinGroupLandingViewModel.State.Waiting(sentAtMs)
    else -> JoinGroupLandingViewModel.State.Ready
}

/** UX 6.2 — whether the wait shows "No answer yet." and `Ask again` (a day
 *  or more since the request) rather than the wait line alone. The rule is
 *  [me.lakko.app.chat.requestStillWaiting], shared with the list. Pure. */
internal fun joinWaitOffersAskAgain(sentAtMs: Long, nowMs: Long): Boolean =
    !me.lakko.app.chat.requestStillWaiting(sentAtMs, nowMs)

/** The chat the page moves to, with no `Open chat` step: the joined group,
 *  else `null`. Pure. */
internal fun groupToOpenOnJoin(state: JoinGroupLandingViewModel.State): String? =
    (state as? JoinGroupLandingViewModel.State.Joined)?.groupIdHex

private fun hexBytesOrNull(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    return try {
        ByteArray(hex.length / 2) { i -> hex.substring(2 * i, 2 * i + 2).toInt(16).toByte() }
    } catch (_: NumberFormatException) {
        null
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
    /** ADR-0087 section 7.2 — Subscribe on a channel's link page. */
    onSubscribeChannel: (preview: me.lakko.app.chat.GroupInvitePreviewUi) -> Unit = {},
    /** Section 15.2 — a co-publisher link opens the join-as-publisher page
     *  (only when [onAcceptPublisher] is not given). */
    onJoinAsPublisher: () -> Unit = {},
    /** Section 15.2 — `Accept` on a publisher link joins right here, with no
     *  further step: progress lines go to the callback; the channel id, or
     *  `null` when the owner's device has not approved within the wait. */
    onAcceptPublisher: (suspend (onStatus: (me.lakko.app.l10n.UiText?) -> Unit) -> String?)? = null,
    /** Section 15.2.2 — when the publisher request this account sent for
     *  the channel [channelIdHex] was sent, `null` when none is pending (a
     *  pending one lands by itself): the page shows the waiting line, then
     *  `Ask again` after a day, never a second `Accept`. */
    publisherRequestSentAtMs: (channelIdHex: String) -> Long? = { null },
    /** This account already publishes in the channel: `Open`. */
    isPublisherOf: (channelIdHex: String) -> Boolean = { false },
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val card by viewModel.card.collectAsStateWithLifecycle()
    val requesting by viewModel.requesting.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { viewModel.start() }
    // UX 6.2 — the join landed: straight to the chat, no `Open chat` step.
    LaunchedEffect(state) { groupToOpenOnJoin(state)?.let(onOpenChat) }
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    // ADR-0087 — a channel's link page says channel.
                    val preview = card?.preview
                    Text(
                        when {
                            preview?.coPublisher == true -> t("channels.join.title")
                            preview?.channelId != null -> t("channels.link_page.title")
                            else -> t("group.join.title")
                        },
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onDone) {
                        LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("common.back"))
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
                Text(t("invite.link.loading"))
            }
            is JoinGroupLandingViewModel.State.Invalid -> {
                Text(GROUP_INVITE_INVALID_TEXT.text(), textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                Button(onClick = onDone) { Text(t("common.open_lakko")) }
            }
            is JoinGroupLandingViewModel.State.Damaged -> {
                Text(me.lakko.app.chat.GROUP_INVITE_DAMAGED_TEXT.text(), textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                Button(onClick = onDone) { Text(t("common.open_lakko")) }
            }
            is JoinGroupLandingViewModel.State.RateLimited -> {
                Text(me.lakko.app.discovery.INVITE_RATE_LIMITED_TEXT.text(), textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                Button(onClick = { viewModel.retryOpen() }) { Text(me.lakko.app.chat.JOIN_TRY_AGAIN_TEXT.text()) }
            }
            is JoinGroupLandingViewModel.State.OpenFailed -> {
                Text(me.lakko.app.chat.INVITE_LINK_OPEN_FAILED_TEXT.text(), textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                Button(onClick = { viewModel.retryOpen() }) { Text(me.lakko.app.chat.JOIN_TRY_AGAIN_TEXT.text()) }
                Spacer(Modifier.height(8.dp))
                TextButton(onClick = onDone) { Text(t("common.open_lakko")) }
            }
            is JoinGroupLandingViewModel.State.Ready -> {
                val preview = card?.preview
                if (preview?.subscriber != null) {
                    // Section 7.2 — `Anna invites you to / Berlin news /
                    // Private channel / [Subscribe]`.
                    val inviter = preview.inviterName?.trim()?.takeIf { it.isNotEmpty() }
                    Text(
                        if (inviter != null) t("invite.group.lead_named", mapOf("name" to inviter)) else t("invite.group.lead_anonymous"),
                        textAlign = TextAlign.Center,
                    )
                    preview.groupName?.let { Text(it, style = MaterialTheme.typography.titleLarge, textAlign = TextAlign.Center) }
                    Text(
                        if (preview.subscriber?.access is uniffi.lakko_ffi_uniffi.FfiChannelAccess.Public) {
                            t("channels.header.public")
                        } else {
                            t("channels.header.private")
                        },
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Spacer(Modifier.height(16.dp))
                    Button(onClick = { onSubscribeChannel(preview) }) { Text(t("channels.subscribe.action")) }
                } else if (preview?.coPublisher == true) {
                    InviteHeadline(card)
                    Spacer(Modifier.height(16.dp))
                    if (onAcceptPublisher != null) {
                        val channelIdHex = preview.channelId?.let { me.lakko.app.channels.ChannelsComposer.bytesToHex(it) }
                        PublisherAccept(
                            onAcceptPublisher,
                            pendingSentAtMs = channelIdHex?.let(publisherRequestSentAtMs),
                            isPublisher = channelIdHex != null && isPublisherOf(channelIdHex),
                        )
                    } else {
                        Button(onClick = onJoinAsPublisher) { Text(GROUP_INVITE_REQUEST_TO_JOIN_TEXT.text()) }
                    }
                } else {
                    InviteHeadline(card)
                    Spacer(Modifier.height(16.dp))
                    Button(onClick = { viewModel.request() }) { Text(GROUP_INVITE_REQUEST_TO_JOIN_TEXT.text()) }
                    InviteFoot(card)
                }
            }
            is JoinGroupLandingViewModel.State.Requesting -> {
                InviteHeadline(card)
                Spacer(Modifier.height(16.dp))
                CircularProgressIndicator()
                Spacer(Modifier.height(8.dp))
                Text(t("group.join.sending"))
                InviteFoot(card)
            }
            is JoinGroupLandingViewModel.State.Waiting -> {
                InviteHeadline(card)
                Spacer(Modifier.height(16.dp))
                if (joinWaitOffersAskAgain(s.sentAtMs, System.currentTimeMillis())) {
                    Text(
                        t("requests_sent.no_answer"),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        textAlign = TextAlign.Center,
                    )
                    Spacer(Modifier.height(12.dp))
                    Button(
                        onClick = { viewModel.askAgain() },
                        enabled = !requesting,
                    ) { Text(t("requests_sent.ask_again")) }
                } else {
                    Text(me.lakko.app.chat.JOIN_WAIT_PRIMARY.text(), textAlign = TextAlign.Center)
                }
                InviteFoot(card)
            }
            is JoinGroupLandingViewModel.State.Joined -> {
                // Shown only for the moment the navigation takes.
                Text(t("group.join.succeeded_opening"), textAlign = TextAlign.Center)
            }
            is JoinGroupLandingViewModel.State.Failed -> {
                InviteHeadline(card)
                Spacer(Modifier.height(16.dp))
                Text(s.message.text(), color = MaterialTheme.colorScheme.error, textAlign = TextAlign.Center)
                Spacer(Modifier.height(12.dp))
                // "Try again" only with the general failure; the other two
                // sentences ask the person to wait or fix the clock first.
                if (me.lakko.app.chat.joinFailureOffersRetry(s.message)) {
                    Button(
                        onClick = { viewModel.request() },
                        enabled = !requesting,
                    ) { Text(me.lakko.app.chat.JOIN_TRY_AGAIN_TEXT.text()) }
                } else {
                    Button(onClick = onDone) { Text(t("common.close")) }
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
        text = lines.headline.text(),
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
            text = members.text(),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/** The muted foot: `Invited by <name>`. No name, no line: digits mean nothing
 *  to a person. */
@Composable
private fun InviteFoot(card: GroupInviteCard?) {
    val line = invitedByLine(card?.preview) ?: return
    Spacer(Modifier.height(16.dp))
    Text(
        text = line.text(),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
        modifier = Modifier.fillMaxWidth(),
    )
}

/** Section 15.2 — `Accept` on a publisher link: joins here, the progress and
 *  any error under the button. */
@Composable
private fun PublisherAccept(
    onAccept: suspend (onStatus: (me.lakko.app.l10n.UiText?) -> Unit) -> String?,
    pendingSentAtMs: Long?,
    isPublisher: Boolean,
) {
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    var busy by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf(false) }
    var error by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf<me.lakko.app.l10n.UiText?>(null) }
    var linkDead by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf(false) }
    // The request this page just sent, until the stored record is read again.
    var sentHereAtMs by androidx.compose.runtime.remember { androidx.compose.runtime.mutableStateOf<Long?>(null) }
    val send: () -> Unit = {
        busy = true
        error = null
        scope.launch {
            try {
                val joined = onAccept {}
                // `null`: not approved yet. The request has no deadline and
                // lands by itself, so the page keeps saying it waits.
                if (joined == null) sentHereAtMs = System.currentTimeMillis()
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: me.lakko.app.channels.LegacyChannelException) {
                error = e.text
            } catch (e: Throwable) {
                android.util.Log.w("JoinGroupLanding", "publisher accept failed: ${e.javaClass.simpleName}")
                when (me.lakko.app.ui.channels.publisherAcceptFailure(e)) {
                    me.lakko.app.ui.channels.PublisherAcceptFailure.LINK_DEAD -> linkDead = true
                    me.lakko.app.ui.channels.PublisherAcceptFailure.OFFLINE ->
                        error = me.lakko.app.l10n.UiText("common.error.offline")
                    me.lakko.app.ui.channels.PublisherAcceptFailure.OTHER -> error = me.lakko.app.chat.joinFailureCopy(e)
                }
            } finally {
                busy = false
            }
        }
    }
    val sentAt = listOfNotNull(pendingSentAtMs, sentHereAtMs).maxOrNull()
    when (
        me.lakko.app.ui.channels.publisherInviteControl(
            isPublisher = isPublisher,
            pendingSentAtMs = sentAt,
            nowMs = System.currentTimeMillis(),
            sending = busy,
            linkDead = linkDead,
        )
    ) {
        // Approved meanwhile: the channel opens from Chats; nothing to press.
        me.lakko.app.ui.channels.PublisherInviteControl.OPEN -> Unit
        me.lakko.app.ui.channels.PublisherInviteControl.SENDING ->
            Button(enabled = false, onClick = {}) { Text(t("channels.join.waiting")) }
        me.lakko.app.ui.channels.PublisherInviteControl.WAITING ->
            Text(t("channels.join.waiting"), color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
        me.lakko.app.ui.channels.PublisherInviteControl.ASK_AGAIN -> {
            Text(t("requests_sent.no_answer"), color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
            Spacer(Modifier.height(8.dp))
            Button(onClick = send) { Text(t("requests_sent.ask_again")) }
        }
        me.lakko.app.ui.channels.PublisherInviteControl.LINK_INVALID ->
            Text(t("invite.link.invalid"), color = MaterialTheme.colorScheme.onSurfaceVariant, textAlign = TextAlign.Center)
        me.lakko.app.ui.channels.PublisherInviteControl.ACCEPT ->
            Button(onClick = send) { Text(t("channels.publishers.accept")) }
    }
    error?.let { line: me.lakko.app.l10n.UiText ->
        Spacer(Modifier.height(8.dp))
        Text(line.text(), color = MaterialTheme.colorScheme.error)
    }
}
