// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import android.content.Context
import android.util.Log
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.TopAppBar
import androidx.compose.ui.Alignment
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.calls.CallVerb
import me.lakko.app.calls.LinkCallHost
import me.lakko.app.calls.LinkCallPhase
import me.lakko.app.calls.LinkCallRequest
import me.lakko.app.calls.LinkCallSlot
import me.lakko.app.calls.LinkCallStartOutcome
import me.lakko.app.calls.ParsedCallInvite
import me.lakko.app.calls.callIdHexOf
import me.lakko.app.calls.hexKey
import me.lakko.app.calls.linkCallOccupiesSlot
import me.lakko.app.calls.parseCallInviteSlug
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.auth.SaveFreshPhraseScreen
import uniffi.lakko_ffi_uniffi.CallInviteCapability
import uniffi.lakko_ffi_uniffi.Identity
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon

/**
 * `/c/<slug>` multi-joiner call-invite landing (ADR-0032, G-21/G-22) — the
 * Android twin of `apps/web/src/routes/JoinCall.tsx`.
 *
 * Whoever opens a `/c/<slug>` call link lands here. The screen decodes +
 * verifies the [CallInviteCapability] (owner's Ed25519 signature) and asks the
 * app-level [LinkCallHost] for the call; from then on it shows the host's state
 * for it. The call itself (the ADR-0032 owner-founds flow via
 * [me.lakko.app.calls.MultiJoinerCall]) lives in the host, so leaving this
 * screen does not end it, and a second link cannot start a second call:
 *
 *   * If the visitor IS the owner (its identity signature key matches the
 *     capability's owner key), it FOUNDS + hosts the call.
 *   * Otherwise it JOINS: mint a fresh KeyPackage, connect to the SFU room,
 *     relay the KeyPackage to the acting committer, and wait for the Welcome.
 *     Until the Welcome lands the screen shows a
 *     "waiting for the owner to admit you" state (invariant #4 — no media
 *     before the group is joined). If no committer responds within the
 *     admission window the call is dead (owner left / call ended).
 *
 * ## Guest join (no login required, ADR-0032, docs/ux/call-links.md section 9)
 *
 * A visitor with no session joins without an account of their own making: on
 * `Join` a guest account is created silently ([AuthRepository.signupGuest],
 * the signup machinery without a username, whose start pays the one proof of
 * work left, ADR-0054) and the call starts as soon as its session lands
 * ([JoinCallViewModel.onSessionLanded]). No recovery phrase is shown first: a
 * person who was sent a link wants the call. Whenever the guest is on this
 * screen and not in the call (it ended, it never started, the app was
 * restarted) the screen asks whether to keep the account
 * ([me.lakko.app.ui.auth.GuestAccountOffer]): `Create account` takes a
 * username and a first password (then the "Protect your account" step, no
 * phrase shown); `No, thanks` deletes the
 * account at once. An account left unanswered is deleted by the servers after
 * a day.
 */

/** The landing screen's own state machine. "Waiting" is the ADR-0032
 *  joiner-side pre-admission phase; "Dead" is the no-live-committer timeout. */
enum class JoinCallPhase { Idle, Starting, Waiting, Live, Ended, Dead, Error }

/** Decoded + verified capability fields the flow needs. */
data class DecodedCall(
    val callId: ByteArray,
    val ownerAccountId: ByteArray,
    val ownerSignatureKey: ByteArray,
    val ownerFingerprint: String,
    val mediaVideo: Boolean,
    val valid: Boolean,
    /** `true` when the local identity IS the capability owner. Always false
     *  for an anonymous visitor. */
    val isOwner: Boolean,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is DecodedCall) return false
        return callId.contentEquals(other.callId) &&
            ownerAccountId.contentEquals(other.ownerAccountId) &&
            ownerSignatureKey.contentEquals(other.ownerSignatureKey) &&
            ownerFingerprint == other.ownerFingerprint &&
            mediaVideo == other.mediaVideo &&
            valid == other.valid &&
            isOwner == other.isOwner
    }

    override fun hashCode(): Int {
        var result = callId.contentHashCode()
        result = 31 * result + ownerAccountId.contentHashCode()
        result = 31 * result + ownerSignatureKey.contentHashCode()
        result = 31 * result + ownerFingerprint.hashCode()
        result = 31 * result + mediaVideo.hashCode()
        result = 31 * result + valid.hashCode()
        result = 31 * result + isOwner.hashCode()
        return result
    }
}

/** Landing UI snapshot. */
data class JoinCallUiState(
    val loading: Boolean = true,
    /** `null` = invalid / expired / not-found link (or [rateLimited]). */
    val decoded: DecodedCall? = null,
    /** The discovery service answered `429` while resolving the link: not an
     *  invalid link, the screen says to try again later. */
    val rateLimited: Boolean = false,
    /** Sealed links (2026-09-28): the fragment is missing or malformed, or
     *  the link does not open under it. Refused with one sentence. */
    val damaged: Boolean = false,
    /** The ledger could not be asked for the link (the network, a server
     *  error): the screen offers it again. */
    val openFailed: Boolean = false,
    val phase: JoinCallPhase = JoinCallPhase.Idle,
    /** Why the last attempt failed (ADR-0061: a catalog message). */
    val error: UiText? = null,
    /** A guest's name in the call (`docs/ux/call-links.md` section 9.1),
     *  prefilled with a new random two-word name every time the page opens
     *  ([JoinCallViewModel.onLinkOpened]). */
    val guestName: String = GUEST_NAME_DEFAULT,
    /** The reason the typed guest name fails the rule, or `null`. */
    val guestNameError: UiText? = null,
    /** Why Join was refused while another call holds the one call slot. */
    val busyMessage: UiText? = null,
    /** ADR-0054 UX section 1 - the guest account is being created (the one
     *  heavy proof of work): `Getting you into the call...`. */
    val guestWorking: Boolean = false,
    /** The guest solve is still running, so the row offers `Cancel`. */
    val guestSolving: Boolean = false,
    /** The session is a guest account (`auth/GuestAccount.kt`). */
    val guest: Boolean = false,
    /** A guest account was deleted from this screen (`No, thanks`). */
    val guestDeleted: Boolean = false,
)

/** The guest name field's prefill: "<adjective> <animal>" from the shared rule
 *  (`lakko_ui_rules::guest_name`), drawn anew every time it is read, which is
 *  every time the page opens. Where the native library is not loaded (host
 *  tests) the catalog's plain word stands in. */
internal val GUEST_NAME_DEFAULT: String
    get() = try {
        uniffi.lakko_ffi_uniffi.guestName(kotlin.random.Random.nextLong().toULong())
    } catch (_: Throwable) {
        UiText("call.guest.default_name").render()
    }


/** What the landing needs from the auth layer: [AuthRepository] in
 *  production, a fake in a host test (a real guest signup pays a proof of
 *  work that needs the native library). */
internal interface JoinCallAuth {
    val state: StateFlow<AuthState>
    val guestAccount: StateFlow<me.lakko.app.auth.GuestSession?>
    val identitySecrets: StateFlow<IdentitySecrets?>

    suspend fun signupGuest(onProofSolved: () -> Unit)
}

/** What opening a `/c/` link came to. */
internal sealed interface CallLinkDecode {
    data class Opened(val call: DecodedCall) : CallLinkDecode

    /** Not found, expired, or it failed to verify. */
    data object Invalid : CallLinkDecode

    /** The ledger answered `429`: not an invalid link. */
    data object RateLimited : CallLinkDecode

    /** The ledger could not be asked (the network, a server error): the link
     *  may be fine, it is offered again. */
    data object OpenFailed : CallLinkDecode

    /** Sealed links: the fragment is missing or malformed, or the link does
     *  not open under it. */
    data object Damaged : CallLinkDecode
}

class JoinCallViewModel internal constructor(
    private val auth: JoinCallAuth,
    private val slug: String,
    private val host: LinkCallHost,
    private val endCurrentCallAction: () -> Unit,
    /** Open and verify the link ([decodeCallLink] in production). */
    private val decodeLink: suspend (slug: String, secrets: IdentitySecrets?) -> CallLinkDecode,
    /** Record the name a guest typed as the new account's own profile. */
    private val saveGuestName: (accountId: ByteArray, typedName: String) -> Unit,
    /** ADR-0074 Stage A - the seed of the account identity MLS groups know
     *  this account by (the predecessor after a key rotation). */
    private val conversationSeed: (IdentitySecrets) -> ByteArray = { it.accountIdentitySeed },
    /** The core's display-name rule (`normalizeDisplayName`); injected for
     *  host tests, where the native library is not loaded. */
    private val normalizeDisplayName: (String) -> String? = { uniffi.lakko_ffi_uniffi.normalizeDisplayName(it) },
    /** Draws the guest name prefill ([GUEST_NAME_DEFAULT]). */
    private val drawGuestName: () -> String = { GUEST_NAME_DEFAULT },
) : ViewModel() {

    constructor(
        authRepository: AuthRepository,
        applicationContext: Context,
        slug: String,
        host: LinkCallHost,
        endCurrentCallAction: () -> Unit,
    ) : this(
        auth = object : JoinCallAuth {
            override val state: StateFlow<AuthState> = authRepository.state
            override val guestAccount = authRepository.guestAccount
            override val identitySecrets: StateFlow<IdentitySecrets?> = authRepository.identitySecrets

            override suspend fun signupGuest(onProofSolved: () -> Unit) =
                authRepository.signupGuest(onProofSolved)
        },
        slug = slug,
        host = host,
        endCurrentCallAction = endCurrentCallAction,
        decodeLink = { link, secrets -> decodeCallLink(applicationContext, link, secrets) },
        saveGuestName = { accountId, typedName -> saveGuestProfileName(applicationContext, accountId, typedName) },
        conversationSeed = { secrets ->
            me.lakko.app.auth.SessionSecretWipe.use(secrets.accountIdentitySeed) { seed ->
                me.lakko.app.auth.conversationAccountSeedOf(
                    applicationContext,
                    me.lakko.app.auth.base64Encode(secrets.accountId),
                    seed,
                )
            }
        },
    )

    /** This screen's own state before (or instead of) the host's call. */
    private val local = MutableStateFlow(JoinCallUiState(guestName = drawGuestName()))

    /** The link open this model last drew a guest name for
     *  ([onLinkOpened]); `null` before the screen reported one. */
    private var linkOpen: Long? = null

    /** Landing UI snapshot: the local state, with the phase of the host's call
     *  for this link when it holds one. */
    val uiState: StateFlow<JoinCallUiState> =
        combine(local, host.slot) { l, slot -> l.withHostSlot(slot) }
            .stateIn(viewModelScope, SharingStarted.Eagerly, JoinCallUiState())

    /** The join to resume once a guest account's session has landed. */
    private var pendingJoin: DecodedCall? = null

    // FINDING 5: a synchronous double-tap latch (set BEFORE any suspend), so a
    // rapid second tap can't mint a SECOND guest account. A second call is
    // prevented by the host, which refuses a call while one holds the slot.
    private var mintingGuest = false

    /** The guest sign-up in flight, so its solve can be cancelled. */
    private var guestJob: kotlinx.coroutines.Job? = null

    /** Numbers the guest sign-ups, so a cancelled one that finishes late does
     *  not release the latch of the one started after it. */
    private var guestAttempt = 0

    /** A guest session was seen here and its end not yet reported
     *  ([me.lakko.app.auth.observeGuestOutcome]). */
    private var wasGuestHere = false

    init {
        runDecode()
        // What became of a guest account on this screen: deleted, the screen
        // says so and offers Join again; kept, the app root leaves the
        // landing once the account has its name and password.
        viewModelScope.launch {
            combine(auth.state, auth.guestAccount) { authState, guest ->
                authState to (me.lakko.app.auth.guestSessionOf(authState, guest) != null)
            }.collect { (authState, guest) -> onGuestObserved(authState, guest) }
        }
    }

    private fun onGuestObserved(authState: AuthState, guest: Boolean) {
        val watch = me.lakko.app.auth.observeGuestOutcome(
            wasGuest = wasGuestHere,
            guest = guest,
            state = authState,
            // Neither delays what this screen reports (the deletion); the app
            // root, which acts on a kept account, passes the real values.
            signupPhrasePending = false,
            callInProgress = false,
        )
        wasGuestHere = watch.wasGuest
        if (watch.outcome == me.lakko.app.auth.GuestOutcome.DELETED) {
            // Join is offered again, as to any visitor without a session.
            mintingGuest = false
            pendingJoin = null
        }
        local.update {
            it.copy(
                guest = guest,
                guestDeleted = when {
                    guest -> false
                    watch.outcome == me.lakko.app.auth.GuestOutcome.DELETED -> true
                    else -> it.guestDeleted
                },
            )
        }
    }

    private fun runDecode() {
        viewModelScope.launch {
            val outcome = decodeLink(slug, auth.identitySecrets.value)
            local.update {
                it.copy(
                    loading = false,
                    decoded = (outcome as? CallLinkDecode.Opened)?.call,
                    rateLimited = outcome == CallLinkDecode.RateLimited,
                    damaged = outcome == CallLinkDecode.Damaged,
                    openFailed = outcome == CallLinkDecode.OpenFailed,
                )
            }
        }
    }

    /** `Try again` after a rate-limited or failed resolve. */
    fun retryDecode() {
        if (!local.value.rateLimited && !local.value.openFailed) return
        local.update { it.copy(loading = true, rateLimited = false, openFailed = false) }
        runDecode()
    }

    private fun JoinCallUiState.withHostSlot(slot: LinkCallSlot?): JoinCallUiState {
        val d = decoded ?: return this
        if (slot == null || slot.callIdHex != callIdHexOf(d.callId)) return this
        val phase = when (slot.phase) {
            LinkCallPhase.STARTING -> JoinCallPhase.Starting
            LinkCallPhase.WAITING -> JoinCallPhase.Waiting
            LinkCallPhase.LIVE -> JoinCallPhase.Live
            LinkCallPhase.ENDED -> JoinCallPhase.Ended
            LinkCallPhase.DEAD -> JoinCallPhase.Dead
            LinkCallPhase.ERROR -> JoinCallPhase.Error
        }
        // The slot's error is the kind of failure, already a catalog line
        // (ADR-0061); an error with no kind reads the generic line.
        val shownError = if (phase == JoinCallPhase.Error) {
            slot.error?.let {
                UiText("common.sentence_pair", parts = mapOf("first" to UiText("call.link.error.generic"), "second" to it))
            } ?: error ?: UiText("call.link.error.generic")
        } else {
            error
        }
        return copy(phase = phase, error = shownError)
    }

    /**
     * Whether the page asks for a name (`docs/ux/call-links.md` section 9.1):
     * a visitor who will join as a new guest, and a guest whose guest account
     * from an earlier link is still on this device (amended 2026-11-07:
     * before, that guest saw no field and kept the first random name for
     * every later call). Never the link's owner, never a real account.
     */
    fun needsGuestName(): Boolean {
        if (uiState.value.decoded?.isOwner == true) return false
        return auth.identitySecrets.value == null || local.value.guest
    }

    /**
     * The page was opened for this link, [open] numbering the opens
     * ([me.lakko.app.contacts.InviteIntake.callJoinOpens]). Every open after
     * the first this model saw draws a new guest name: the model outlives the
     * page (it is kept per link), and the name of an earlier visit is never
     * offered again. The same open seen twice (the page moving between the
     * signed-out and signed-in roots when a guest account lands, a rotation)
     * draws nothing, so a name being used for a join is never replaced.
     */
    fun onLinkOpened(open: Long) {
        val before = linkOpen
        if (before == open) return
        linkOpen = open
        if (before == null) return
        if (mintingGuest || pendingJoin != null) return
        local.update { it.copy(guestName = drawGuestName(), guestNameError = null) }
    }

    /** The name typed on the page as the guest's name for the call: the
     *  trimmed name, or `null` when the field is empty or fails the rule. */
    private fun typedCallName(): String? =
        (checkGuestName(local.value.guestName) as? me.lakko.app.profile.DisplayNameCheck.Valid)?.normalized

    private fun checkGuestName(value: String): me.lakko.app.profile.DisplayNameCheck =
        me.lakko.app.profile.checkDisplayName(value, normalizeDisplayName)

    /** The guest name field, checked against the core's rule. */
    fun onGuestNameChange(value: String) {
        val check = checkGuestName(value)
        local.update {
            it.copy(
                guestName = value,
                guestNameError = (check as? me.lakko.app.profile.DisplayNameCheck.Invalid)?.problem?.text,
            )
        }
    }

    /** This screen is showing the call; the host does not repeat its status
     *  elsewhere meanwhile. Returns the release. */
    fun claimLanding(): () -> Unit {
        val release = host.claimLanding()
        return {
            release()
            // An outcome seen here is not repeated on the next screen either.
            val slot = host.slot.value
            val d = local.value.decoded
            if (slot != null && d != null && slot.callIdHex == callIdHexOf(d.callId) &&
                !linkCallOccupiesSlot(slot.phase)
            ) {
                host.dismissOutcome()
            }
        }
    }

    /** Tap Join: use the live session if there is one, else create a guest
     *  account first (username-less full account) and go on into the call as
     *  soon as its session lands. The screen has already obtained the
     *  microphone (and, for a video link, camera) permission; [videoAllowed]
     *  is `false` when the camera was refused and the user chose audio only. */
    fun onStart(videoAllowed: Boolean = true) {
        val decoded = uiState.value.decoded ?: return
        // A video link joined after the camera was refused joins audio only
        // (the user chose that in the permission dialog).
        val d = if (videoAllowed) decoded else decoded.copy(mediaVideo = false)
        local.update { it.copy(busyMessage = null) }
        // Refuse before minting a guest account for a call that could not start.
        val busy = host.busyMessage(if (d.isOwner) CallVerb.START else CallVerb.JOIN, callIdHexOf(d.callId))
        if (busy != null) {
            local.update { it.copy(busyMessage = busy) }
            return
        }
        val secrets = auth.identitySecrets.value
        if (secrets != null) {
            // A returning guest's typed name is their name for this call; it
            // must pass the rule like a new guest's. The profile is not
            // written. A real account starts under its profile name.
            val callName = if (local.value.guest && !d.isOwner) {
                if (checkGuestName(local.value.guestName) is me.lakko.app.profile.DisplayNameCheck.Invalid) return
                typedCallName()
            } else {
                null
            }
            viewModelScope.launch { beginJoin(d, secrets, callName) }
            return
        }
        // No session — mint a guest account. Its session lands at once (no
        // recovery-phrase screen first) and onSessionLanded continues the
        // join. The synchronous latch makes a rapid double-tap idempotent —
        // never two guest accounts.
        if (mintingGuest) return
        mintingGuest = true
        local.update {
            it.copy(phase = JoinCallPhase.Starting, error = null, guestWorking = true, guestSolving = true)
        }
        pendingJoin = d
        val attempt = ++guestAttempt
        guestJob = viewModelScope.launch {
            try {
                auth.signupGuest(onProofSolved = {
                    local.update { it.copy(guestSolving = false) }
                })
                local.update { it.copy(guestWorking = false, guestSolving = false) }
            } catch (err: kotlin.coroutines.cancellation.CancellationException) {
                throw err
            } catch (err: Throwable) {
                pendingJoin = null
                Log.w("JoinCall", "guest account failed: ${err.javaClass.simpleName}")
                val copy = when (err) {
                    is me.lakko.app.auth.AuthHttpError ->
                        me.lakko.app.auth.authLimitText(err) ?: UiText("auth.signup.error.generic")
                    is me.lakko.app.auth.AuthNetworkError -> UiText("common.error.offline")
                    else -> UiText("auth.signup.error.generic")
                }
                local.update {
                    it.copy(phase = JoinCallPhase.Error, error = copy, guestWorking = false, guestSolving = false)
                }
            } finally {
                // The latch covers the signup itself (a second tap while it
                // runs). Once it is over a tap takes the signed-in branch
                // above; and if that guest account is later deleted on this
                // screen (`No, thanks`), Join has to work again. Left set
                // after a successful signup, Join did nothing after the
                // delete (found live on web, 2026-10-02).
                if (guestAttempt == attempt) mintingGuest = false
            }
        }
    }

    /** `Cancel` while the guest account's proof of work runs: back to Join,
     *  nothing was sent. */
    fun onCancelGuest() {
        if (!local.value.guestSolving) return
        guestJob?.cancel()
        guestJob = null
        mintingGuest = false
        pendingJoin = null
        local.update {
            it.copy(phase = JoinCallPhase.Idle, error = null, guestWorking = false, guestSolving = false)
        }
    }

    /** Called by [JoinCallRoot] once auth flips to Session — resume a parked
     *  guest join. */
    fun onSessionLanded() {
        val d = pendingJoin ?: return
        val secrets = auth.identitySecrets.value ?: return
        pendingJoin = null
        // ADR-0048 UX §4 — the guest account now exists: record the name it
        // asked for as its own profile (no username). Never blocks the join.
        try {
            saveGuestName(secrets.accountId, local.value.guestName)
        } catch (err: Throwable) {
            Log.w("JoinCall", "guest profile not saved", err)
        }
        // The same name is the guest's name for this call.
        val callName = typedCallName()
        viewModelScope.launch { beginJoin(d, secrets, callName) }
    }

    /** Hand the ADR-0032 host / join to the app-level call host. */
    private suspend fun beginJoin(d: DecodedCall, secrets: IdentitySecrets, callName: String? = null) {
        local.update { it.copy(phase = JoinCallPhase.Starting, error = null, busyMessage = null) }
        val outcome = host.start(
            LinkCallRequest(
                callId = d.callId,
                ownerAccountId = d.ownerAccountId,
                ownerSignatureKey = d.ownerSignatureKey,
                mediaVideo = d.mediaVideo,
                isOwner = d.isOwner,
                slug = slug,
                accountIdentitySeed = conversationSeed(secrets),
                sessionToken = secrets.sessionToken,
                accountId = secrets.accountId,
                initialCallName = callName,
            ),
        )
        when (outcome) {
            // The host's slot is what this screen shows from here on.
            LinkCallStartOutcome.Started,
            LinkCallStartOutcome.AlreadyInThisCall,
            -> local.update { it.copy(phase = JoinCallPhase.Idle) }
            is LinkCallStartOutcome.Busy ->
                local.update { it.copy(phase = JoinCallPhase.Idle, busyMessage = outcome.message) }
            is LinkCallStartOutcome.Failed -> {
                // The host's sentence is diagnostic English; the log keeps it and
                // the screen shows the catalog line (ADR-0061).
                Log.w("JoinCall", "link call failed: ${outcome.message}")
                local.update { it.copy(phase = JoinCallPhase.Error, error = UiText("call.link.error.generic")) }
            }
        }
    }

    /** The host's slot holds this link's call (so there is a call to cancel). */
    fun hostHoldsThisCall(state: JoinCallUiState): Boolean {
        val d = state.decoded ?: return false
        val slot = host.slot.value ?: return false
        return slot.callIdHex == callIdHexOf(d.callId) && linkCallOccupiesSlot(slot.phase)
    }

    /** Stop waiting to be admitted (or leave the call). */
    fun onCancel() {
        host.leave()
    }

    /** The busy notice's "End current call". */
    fun onEndCurrentCall() {
        endCurrentCallAction()
        local.update { it.copy(busyMessage = null) }
    }

    /** Forget a failed attempt and offer Join again. */
    fun onTryAgain() {
        val slot = host.slot.value
        val d = local.value.decoded
        if (slot != null && d != null && slot.callIdHex == callIdHexOf(d.callId)) host.dismissOutcome()
        mintingGuest = false
        local.update { it.copy(phase = JoinCallPhase.Idle, error = null) }
    }
}

/**
 * Open and verify a `/c/` link: the production [JoinCallViewModel] decoder.
 * [secrets] is the live session, when there is one, to tell whether this
 * device's account is the link's owner.
 */
internal suspend fun decodeCallLink(
    applicationContext: Context,
    slug: String,
    secrets: IdentitySecrets?,
): CallLinkDecode {
    // Sealed links: `<slug>#<secret>`; a missing or malformed secret is
    // refused before any request.
    val link = me.lakko.app.discovery.InviteLinkArgument.decode(slug)
    val secret = me.lakko.app.discovery.inviteLinkSecretOf(link.fragment) ?: return CallLinkDecode.Damaged
    return try {
        val parsed = parseCallInviteSlug(link.slug)
        val sealedBytes: ByteArray = when (parsed) {
            is ParsedCallInvite.Pointer -> {
                val discovery = DiscoveryClient()
                try {
                    discovery.fetchCallInvite(parsed.blobId) ?: return CallLinkDecode.Invalid
                } catch (e: Throwable) {
                    // A failure to ask the ledger says nothing about the link.
                    me.lakko.app.rethrowIfCancelled(e)
                    return when {
                        me.lakko.app.discovery.isDiscoveryRateLimited(e) -> CallLinkDecode.RateLimited
                        e is DiscoveryHttpError && e.status == 404 -> CallLinkDecode.Invalid
                        else -> {
                            Log.w("JoinCall", "the call link could not be fetched", e)
                            CallLinkDecode.OpenFailed
                        }
                    }
                } finally {
                    discovery.close()
                }
            }
        }
        me.lakko.app.discovery.openSealedInviteLink(sealedBytes, secret) { bytes, key ->
            CallInviteCapability.openLink(bytes, key)
        }.use { cap ->
            // ADR-0074 Stage A - the account identity MLS groups know this
            // account by (the predecessor after a key rotation). Read through
            // [SessionSecretWipe]: whole, or the session has ended.
            val isOwner = secrets != null &&
                me.lakko.app.auth.SessionSecretWipe.use(secrets.accountIdentitySeed) { seed ->
                    me.lakko.app.auth.conversationAccountSeedOf(
                        applicationContext,
                        me.lakko.app.auth.base64Encode(secrets.accountId),
                        seed,
                    )
                }.let { seed ->
                    try {
                        Identity.fromSeed(seed).use {
                            hexKey(it.signaturePublicKey()) == hexKey(cap.ownerSignaturePubkey())
                        }
                    } finally {
                        seed.fill(0)
                    }
                }
            CallLinkDecode.Opened(
                DecodedCall(
                    callId = cap.callId(),
                    ownerAccountId = cap.ownerAccountId(),
                    ownerSignatureKey = cap.ownerSignaturePubkey(),
                    ownerFingerprint = cap.ownerFingerprint(),
                    mediaVideo = cap.media().video,
                    valid = cap.isValidNow(System.currentTimeMillis().toULong()),
                    isOwner = isOwner,
                ),
            )
        }
    } catch (e: me.lakko.app.discovery.InviteLinkDamaged) {
        CallLinkDecode.Damaged
    } catch (e: DiscoveryHttpError) {
        when {
            me.lakko.app.discovery.isDiscoveryRateLimited(e) -> CallLinkDecode.RateLimited
            e.status == 404 -> CallLinkDecode.Invalid
            else -> {
                Log.w("JoinCall", "decode/verify failed", e)
                CallLinkDecode.Invalid
            }
        }
    } catch (e: Throwable) {
        me.lakko.app.rethrowIfCancelled(e)
        Log.w("JoinCall", "decode/verify failed", e)
        CallLinkDecode.Invalid
    }
}

/**
 * ADR-0048 UX §4 — record the name a guest typed on the landing as the new
 * account's own profile (no username). A name the rule refuses is not stored.
 */
internal fun saveGuestProfileName(applicationContext: Context, accountId: ByteArray, typedName: String) {
    val nameCheck = me.lakko.app.profile.checkDisplayName(typedName)
    if (nameCheck !is me.lakko.app.profile.DisplayNameCheck.Valid) return
    val updated = me.lakko.app.profile.applyOwnNameChange(
        current = me.lakko.app.profile.OwnProfileStore.load(applicationContext, accountId),
        newName = nameCheck.normalized,
        username = null,
        nowMs = System.currentTimeMillis(),
    ) ?: return
    me.lakko.app.profile.OwnProfileStore.save(applicationContext, accountId, updated)
    // ADR-0048 section 4a — to the account's other devices too (nothing is
    // sent before a session binds).
    me.lakko.app.reach.ReachManager.get(applicationContext).publishOwnProfileToDevices()
}

/**
 * Public-route root for a pending `/c/<slug>` deep link. Works in ANY auth
 * state (an anonymous visitor gets the guest path). A LaunchedEffect resumes
 * the parked join the moment auth becomes a Session, which for a guest is the
 * moment its account exists: no recovery phrase is shown before the call.
 *
 * [guestOffer] is the question a guest is asked when not in the call
 * (`null` when the session is not a guest's), with [guestClaimedUsername] the
 * name an unfinished `Create account` already took.
 */
@Composable
fun JoinCallRoot(
    authRepository: AuthRepository,
    slug: String,
    applicationContext: Context,
    onExit: () -> Unit,
    host: LinkCallHost,
    endCurrentCall: () -> Unit,
    guestOffer: me.lakko.app.ui.auth.GuestAccountOfferViewModel? = null,
    guestClaimedUsername: String? = null,
    /** Numbers the opens of a call link (`InviteIntake.callJoinOpens`): a
     *  new one draws a new guest name ([JoinCallViewModel.onLinkOpened]). */
    linkOpen: Long = 0L,
) {
    val authState by authRepository.state.collectAsStateWithLifecycle()
    val vm: JoinCallViewModel = viewModel(
        key = "joincall-$slug",
        factory = object : ViewModelProvider.Factory {
            @Suppress("UNCHECKED_CAST")
            override fun <T : ViewModel> create(modelClass: Class<T>): T =
                JoinCallViewModel(authRepository, applicationContext, slug, host, endCurrentCall) as T
        },
    )

    // A new open of the link draws a new guest name (section 9.1).
    androidx.compose.runtime.LaunchedEffect(vm, linkOpen) { vm.onLinkOpened(linkOpen) }

    // Guest-join resume: once signupGuest has landed a full session, continue
    // the parked join automatically.
    LaunchedEffectOnSession(authState) { vm.onSessionLanded() }

    when (val current = authState) {
        // ADR-0017 lost-everything fallback with a call link pending: the
        // fresh identity's phrase is saved before anything else. (A guest
        // signup no longer comes through this state.)
        is AuthState.SaveFreshPhrase -> SaveFreshPhraseScreen(
            phrase = authRepository.freshPhrase() ?: "",
            onConfirm = { authRepository.confirmFreshPhraseSaved() },
            username = current.account.username,
        )
        else -> JoinCallScreen(
            viewModel = vm,
            onExit = onExit,
            guestOffer = guestOffer,
            guestClaimedUsername = guestClaimedUsername,
        )
    }
}

@Composable
private fun LaunchedEffectOnSession(
    authState: AuthState,
    block: () -> Unit,
) {
    androidx.compose.runtime.LaunchedEffect(authState) {
        if (authState is AuthState.Session) block()
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun JoinCallScreen(
    viewModel: JoinCallViewModel,
    onExit: () -> Unit,
    guestOffer: me.lakko.app.ui.auth.GuestAccountOfferViewModel? = null,
    guestClaimedUsername: String? = null,
) {
    val ui by viewModel.uiState.collectAsStateWithLifecycle()
    // The question a guest is asked whenever they are on this screen and not
    // in the call. Drawn only for a guest session: for anyone else the
    // screen's own `Back` is the way on.
    val offer: (@Composable () -> Unit)? = guestOffer?.takeIf { ui.guest }?.let { offerModel ->
        {
            me.lakko.app.ui.auth.GuestAccountOffer(
                viewModel = offerModel,
                claimedUsername = guestClaimedUsername,
            )
        }
    }
    // Hosting or joining a call link needs the microphone, and the camera for
    // a video link — asked here, before any account is minted or any SFU
    // connection opened.
    val callPermissions = rememberCallPermissionGate()

    // While this screen is up it shows the host's state for its call itself, so
    // the host does not repeat it as a status line. A live call is shown by the
    // app-level in-call screen, above this one.
    DisposableEffect(viewModel) {
        val release = viewModel.claimLanding()
        onDispose { release() }
    }

    // A call that ends while this screen is up sends a signed-in person back
    // into the app (`docs/ux/call-links.md` section 5). A guest stays: this
    // screen is where they are asked about an account.
    androidx.compose.runtime.LaunchedEffect(ui.phase, ui.guest) {
        if (ui.phase == JoinCallPhase.Ended && !ui.guest) onExit()
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(t(if (ui.decoded?.isOwner == true) "call.link.title_start" else "call.link.title_join"))
                },
                navigationIcon = {
                    // The system back of every other link page (the invite and
                    // group pages): leaves this screen, a running call goes on.
                    IconButton(onClick = onExit) {
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
                .verticalScroll(rememberScrollState())
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            Column(
                modifier = Modifier.widthIn(max = PANEL_MAX_WIDTH).fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                JoinCallPanel(
                    ui = ui,
                    viewModel = viewModel,
                    onExit = onExit,
                    offer = offer,
                    onJoin = { video, start ->
                        callPermissions.request(video = video) { v -> start(v) }
                    },
                )
            }
        }
    }
}

/** The panel's width cap: the auth card's (design system section 2), so the
 *  page reads as one card on a tablet too. */
private val PANEL_MAX_WIDTH = 400.dp

/**
 * What the link page shows, state by state: the same states and controls as
 * the web page (`routes/JoinCall.tsx`) and the iOS one
 * (`JoinCallLandingScreen.swift`), in the shape of this client's other link
 * pages (the contact and group invite pages): centred, the host's picture and
 * name, one full-width primary action and the secondary ones under it.
 */
@Composable
private fun JoinCallPanel(
    ui: JoinCallUiState,
    viewModel: JoinCallViewModel,
    onExit: () -> Unit,
    offer: (@Composable () -> Unit)?,
    onJoin: (video: Boolean, start: (videoAllowed: Boolean) -> Unit) -> Unit,
) {
    val decodedOrNull = ui.decoded
    when {
        ui.loading -> {
            CircularProgressIndicator()
            CenteredText(t("call.link.loading"))
        }
        ui.rateLimited -> {
            CenteredText(me.lakko.app.discovery.INVITE_RATE_LIMITED_TEXT.text())
            Button(onClick = { viewModel.retryDecode() }, modifier = Modifier.fillMaxWidth()) {
                Text(t("common.try_again"))
            }
            CloseButton(onExit)
        }
        ui.damaged -> {
            CenteredText(me.lakko.app.discovery.INVITE_LINK_DAMAGED_COPY.text())
            CloseButton(onExit)
        }
        ui.openFailed -> {
            CenteredText(me.lakko.app.chat.INVITE_LINK_OPEN_FAILED_TEXT.text())
            Button(onClick = { viewModel.retryDecode() }, modifier = Modifier.fillMaxWidth()) {
                Text(t("common.try_again"))
            }
            CloseButton(onExit)
        }
        decodedOrNull == null || !decodedOrNull.valid -> {
            CenteredText(t("call.link.invalid"))
            CloseButton(onExit)
        }
        else -> {
            val decoded: DecodedCall = decodedOrNull
            if (ui.guestDeleted) {
                CenteredText(t("guest.offer.deleted"), muted = true)
            }
            HostHeader(decoded)
            ui.busyMessage?.let { message ->
                CenteredText(message.text(), color = MaterialTheme.colorScheme.error)
                Button(
                    onClick = viewModel::onEndCurrentCall,
                    modifier = Modifier.fillMaxWidth(),
                    colors = ButtonDefaults.buttonColors(
                        containerColor = MaterialTheme.colorScheme.error,
                        contentColor = MaterialTheme.colorScheme.onError,
                    ),
                ) { Text(t("call.link.end_current")) }
                CloseButton(onExit)
            }
            when (ui.phase) {
                JoinCallPhase.Idle -> {
                    // ADR-0098 section 4 — calls are not offered to a
                    // suspended account: no Join, only the way out.
                    val callsSuspended by me.lakko.app.auth.SuspensionGate.readOnly.collectAsStateWithLifecycle()
                    if (callsSuspended) {
                        CenteredText(t("suspended.self.body"), muted = true)
                        CloseButton(onExit)
                    } else {
                    // Section 9.1 — a guest (no session yet, or a guest
                    // account from an earlier link) is asked for their name
                    // in the call before Join.
                    val asksName = viewModel.needsGuestName()
                    if (asksName) {
                        OutlinedTextField(
                            value = ui.guestName,
                            onValueChange = viewModel::onGuestNameChange,
                            label = { Text(t("call.link.guest_name.label")) },
                            singleLine = true,
                            isError = ui.guestNameError != null,
                            supportingText = ui.guestNameError?.let { reason -> { Text(reason.text()) } },
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                    Button(
                        enabled = !asksName || ui.guestNameError == null,
                        onClick = {
                            onJoin(decoded.mediaVideo) { v -> viewModel.onStart(videoAllowed = v) }
                        },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(if (decoded.isOwner) t("call.link.start") else t("call.link.join"))
                    }
                    // No login required: a visitor with no session joins as
                    // a guest, and is told so before pressing.
                    if (asksName && !ui.guest) {
                        CenteredText(t("call.guest.note"), muted = true, small = true)
                    }
                    // A guest who is here and not in the call (the app was
                    // restarted, or they stopped waiting) is asked too;
                    // anyone else gets the way out.
                    if (offer != null) {
                        offer()
                    } else if (ui.busyMessage == null) {
                        CloseButton(onExit)
                    }
                    }
                }
                JoinCallPhase.Starting -> {
                    CircularProgressIndicator()
                    if (ui.guestWorking) {
                        // ADR-0054 UX section 1 - the guest account's one
                        // heavy piece of work.
                        CenteredText(t("call.guest.working.title"))
                        CenteredText(t("auth.signup.working.line"), muted = true, small = true)
                        if (ui.guestSolving) {
                            OutlinedButton(onClick = viewModel::onCancelGuest, modifier = Modifier.fillMaxWidth()) {
                                Text(t("common.cancel"))
                            }
                        }
                    } else {
                        CenteredText(t("call.link.connecting"))
                        // Cancel only once the host holds the call.
                        RunningButtons(
                            onBack = onExit,
                            onCancel = viewModel::onCancel.takeIf { viewModel.hostHoldsThisCall(ui) },
                        )
                    }
                }
                JoinCallPhase.Waiting -> {
                    CircularProgressIndicator()
                    CenteredText(t("call.link.waiting"))
                    CenteredText(t("call.link.waiting_hint"), muted = true, small = true)
                    RunningButtons(onBack = onExit, onCancel = viewModel::onCancel)
                }
                JoinCallPhase.Live -> {
                    CenteredText(t("call.link.live"))
                    OutlinedButton(onClick = onExit, modifier = Modifier.fillMaxWidth()) { Text(t("common.back")) }
                }
                // What follows a call that is over: a guest is asked whether
                // to keep the account; anyone else gets the way on.
                JoinCallPhase.Dead -> {
                    CenteredText(t("call.link.dead"))
                    if (offer != null) offer() else CloseButton(onExit)
                }
                JoinCallPhase.Ended -> {
                    CenteredText(t("call.link.ended"))
                    if (offer != null) offer() else CloseButton(onExit)
                }
                JoinCallPhase.Error -> {
                    CenteredText(
                        (ui.error ?: UiText("call.link.error.generic")).text(),
                        color = MaterialTheme.colorScheme.error,
                    )
                    Button(onClick = viewModel::onTryAgain, modifier = Modifier.fillMaxWidth()) {
                        Text(t("common.try_again"))
                    }
                    // The join failed: the guest can still be asked.
                    if (offer != null) offer() else CloseButton(onExit)
                }
            }
        }
    }
}

/**
 * The host's picture and the line naming them, shown only when this device
 * knows a name (a contact or a received profile; ADR-0048, ADR-0084). Never
 * a fingerprint or a placeholder: a string of digits names nobody, and the
 * owner's own page has nothing to say about its host.
 */
@Composable
private fun HostHeader(decoded: DecodedCall) {
    if (decoded.isOwner) return
    val appContext = androidx.compose.ui.platform.LocalContext.current.applicationContext
    val host = androidx.compose.runtime.remember(decoded.ownerSignatureKey) {
        me.lakko.app.profile.ProfileDirectory.get(appContext).resolve(decoded.ownerSignatureKey)
    }
    if (host.isPlaceholder) return
    me.lakko.app.ui.components.LakkoAvatar(
        name = host.primary,
        seed = host.primary,
        size = 72.dp,
        personKey = decoded.ownerSignatureKey,
    )
    Text(
        text = t("call.link.host", mapOf("name" to host.primary)),
        style = MaterialTheme.typography.titleMedium,
        textAlign = TextAlign.Center,
    )
}

/** A starting or waiting call: leave the screen with the call still going,
 *  or cancel it ([onCancel] `null` when there is nothing to cancel yet). */
@Composable
private fun RunningButtons(onBack: () -> Unit, onCancel: (() -> Unit)?) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        OutlinedButton(onClick = onBack, modifier = Modifier.weight(1f)) { Text(t("common.back")) }
        if (onCancel != null) {
            OutlinedButton(
                onClick = onCancel,
                modifier = Modifier.weight(1f),
                colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
            ) { Text(t("common.cancel")) }
        }
    }
}

/** The way out of the page when there is nothing else to do. */
@Composable
private fun CloseButton(onExit: () -> Unit) {
    TextButton(onClick = onExit, modifier = Modifier.fillMaxWidth()) { Text(t("common.close")) }
}

/** One centred line of the page. */
@Composable
private fun CenteredText(
    text: String,
    muted: Boolean = false,
    small: Boolean = false,
    color: androidx.compose.ui.graphics.Color? = null,
) {
    Text(
        text = text,
        style = if (small) MaterialTheme.typography.bodySmall else MaterialTheme.typography.bodyMedium,
        color = color ?: if (muted) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
        textAlign = TextAlign.Center,
        modifier = Modifier.fillMaxWidth(),
    )
}
