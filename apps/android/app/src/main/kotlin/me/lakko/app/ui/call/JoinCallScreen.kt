// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import android.content.Context
import android.util.Log
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
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
 * ## Guest join (no login required, ADR-0032 UX)
 *
 * An anonymous visitor (no session) can join WITHOUT logging in: on tapping
 * Join we auto-create a FULL, username-less account via
 * [AuthRepository.signupGuest] (reusing the audited signup machinery, whose
 * start pays the one proof of work left, ADR-0054). That flips auth to [AuthState.SaveFreshPhrase], so [JoinCallRoot]
 * renders the existing [SaveFreshPhraseScreen] for the guest to save their
 * recovery phrase; on acknowledgement [AuthRepository.confirmFreshPhraseSaved]
 * lands the session and the join resumes automatically ([JoinCallViewModel.onSessionLanded]).
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
    val phase: JoinCallPhase = JoinCallPhase.Idle,
    /** Why the last attempt failed (ADR-0061: a catalog message). */
    val error: UiText? = null,
    /** ADR-0048 UX §4 — a guest's "Your name", prefilled `Guest`. */
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
)

/** ADR-0048 UX §4 — the guest name field's prefill, in the interface
 *  language at the moment the screen opens. */
internal val GUEST_NAME_DEFAULT: String
    get() = UiText("call.guest.default_name").render()


class JoinCallViewModel(
    private val authRepository: AuthRepository,
    private val applicationContext: Context,
    private val slug: String,
    private val host: LinkCallHost,
    private val endCurrentCallAction: () -> Unit,
) : ViewModel() {

    /** ADR-0066 Stage A - the seed of the account identity MLS groups know
     *  this account by (the predecessor after a key rotation). */
    private fun conversationSeed(secrets: me.lakko.app.auth.IdentitySecrets): ByteArray =
        me.lakko.app.auth.conversationAccountSeedOf(
            applicationContext,
            me.lakko.app.auth.base64Encode(secrets.accountId),
            secrets.accountIdentitySeed,
        )

    /** This screen's own state before (or instead of) the host's call. */
    private val local = MutableStateFlow(JoinCallUiState())

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

    /** Set by [decode] when the ledger answered `429`. */
    private var lastDecodeRateLimited = false

    /** Set by [decode] when the link is damaged (sealed links). */
    private var lastDecodeDamaged = false

    init {
        runDecode()
    }

    private fun runDecode() {
        viewModelScope.launch {
            lastDecodeRateLimited = false
            lastDecodeDamaged = false
            val d = decode(slug)
            local.update {
                it.copy(
                    loading = false,
                    decoded = d,
                    rateLimited = d == null && lastDecodeRateLimited,
                    damaged = d == null && lastDecodeDamaged,
                )
            }
        }
    }

    /** `Try again` after a rate-limited resolve. */
    fun retryDecode() {
        if (!local.value.rateLimited) return
        local.update { it.copy(loading = true, rateLimited = false) }
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

    private suspend fun decode(slug: String): DecodedCall? {
        // Sealed links: `<slug>#<secret>`; a missing or malformed secret is
        // refused before any request.
        val link = me.lakko.app.discovery.InviteLinkArgument.decode(slug)
        val secret = me.lakko.app.discovery.inviteLinkSecretOf(link.fragment) ?: run {
            lastDecodeDamaged = true
            return null
        }
        return try {
            val parsed = parseCallInviteSlug(link.slug)
            val sealedBytes: ByteArray = when (parsed) {
                is ParsedCallInvite.Pointer -> {
                    val discovery = DiscoveryClient()
                    try {
                        discovery.fetchCallInvite(parsed.blobId) ?: return null
                    } finally {
                        discovery.close()
                    }
                }
                is ParsedCallInvite.Inline -> parsed.capabilityBytes
            }
            me.lakko.app.discovery.openSealedInviteLink(sealedBytes, secret) { bytes, key ->
                CallInviteCapability.openLink(bytes, key)
            }.use { cap ->
                val secrets = authRepository.identitySecrets.value
                val isOwner = secrets != null &&
                    Identity.fromSeed(conversationSeed(secrets)).use {
                        hexKey(it.signaturePublicKey()) == hexKey(cap.ownerSignaturePubkey())
                    }
                DecodedCall(
                    callId = cap.callId(),
                    ownerAccountId = cap.ownerAccountId(),
                    ownerSignatureKey = cap.ownerSignaturePubkey(),
                    ownerFingerprint = cap.ownerFingerprint(),
                    mediaVideo = cap.media().video,
                    valid = cap.isValidNow(System.currentTimeMillis().toULong()),
                    isOwner = isOwner,
                )
            }
        } catch (e: me.lakko.app.discovery.InviteLinkDamaged) {
            lastDecodeDamaged = true
            null
        } catch (e: DiscoveryHttpError) {
            if (me.lakko.app.discovery.isDiscoveryRateLimited(e)) {
                lastDecodeRateLimited = true
                null
            } else if (e.status == 404) null else {
                Log.w("JoinCall", "decode/verify failed", e)
                null
            }
        } catch (e: Throwable) {
            Log.w("JoinCall", "decode/verify failed", e)
            null
        }
    }

    /** `true` while there is no signed-in session, i.e. Join would mint a guest
     *  account — the screen then asks for the guest's name. */
    fun needsGuestName(): Boolean = authRepository.identitySecrets.value == null

    /** ADR-0048 UX §4 — the guest name field, checked against the core's rule. */
    fun onGuestNameChange(value: String) {
        val check = me.lakko.app.profile.checkDisplayName(value)
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
     *  account first (username-less full account) and resume after the phrase
     *  is saved. The screen has already obtained the microphone (and, for a
     *  video link, camera) permission; [videoAllowed] is `false` when the
     *  camera was refused and the user chose audio only. */
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
        val secrets = authRepository.identitySecrets.value
        if (secrets != null) {
            viewModelScope.launch { beginJoin(d, secrets) }
            return
        }
        // No session — mint a guest account. This flips auth to
        // SaveFreshPhrase (the SaveFreshPhraseScreen renders); onSessionLanded
        // continues the join once the session lands. The synchronous latch
        // makes a rapid double-tap idempotent — never two guest accounts.
        if (mintingGuest) return
        mintingGuest = true
        local.update {
            it.copy(phase = JoinCallPhase.Starting, error = null, guestWorking = true, guestSolving = true)
        }
        pendingJoin = d
        guestJob = viewModelScope.launch {
            try {
                authRepository.signupGuest(onProofSolved = {
                    local.update { it.copy(guestSolving = false) }
                })
                local.update { it.copy(guestWorking = false, guestSolving = false) }
            } catch (err: kotlin.coroutines.cancellation.CancellationException) {
                throw err
            } catch (err: Throwable) {
                mintingGuest = false // allow a retry after a failed mint
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
        val secrets = authRepository.identitySecrets.value ?: return
        pendingJoin = null
        // ADR-0048 UX §4 — the guest account now exists: record the name it
        // asked for as its own profile (no username). A name the rule refuses
        // is not stored; the guest can set one later in Settings -> Profile.
        val nameCheck = me.lakko.app.profile.checkDisplayName(local.value.guestName)
        if (nameCheck is me.lakko.app.profile.DisplayNameCheck.Valid) {
            try {
                val updated = me.lakko.app.profile.applyOwnNameChange(
                    current = me.lakko.app.profile.OwnProfileStore.load(applicationContext, secrets.accountId),
                    newName = nameCheck.normalized,
                    username = null,
                    nowMs = System.currentTimeMillis(),
                )
                if (updated != null) {
                    me.lakko.app.profile.OwnProfileStore.save(applicationContext, secrets.accountId, updated)
                    // ADR-0048 section 4a — to the account's other devices
                    // too (nothing is sent before a session binds).
                    me.lakko.app.reach.ReachManager.get(applicationContext).publishOwnProfileToDevices()
                }
            } catch (err: Throwable) {
                Log.w("JoinCall", "guest profile not saved", err)
            }
        }
        viewModelScope.launch { beginJoin(d, secrets) }
    }

    /** Hand the ADR-0032 host / join to the app-level call host. */
    private suspend fun beginJoin(d: DecodedCall, secrets: IdentitySecrets) {
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
 * Public-route root for a pending `/c/<slug>` deep link. Works in ANY auth
 * state (an anonymous visitor gets the guest path). Renders the guest
 * recovery-phrase prompt while auth is [AuthState.SaveFreshPhrase] (mid-guest
 * onboarding), else the [JoinCallScreen]. A LaunchedEffect resumes the parked
 * join the moment auth becomes a Session.
 */
@Composable
fun JoinCallRoot(
    authRepository: AuthRepository,
    slug: String,
    applicationContext: Context,
    onExit: () -> Unit,
    host: LinkCallHost,
    endCurrentCall: () -> Unit,
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

    // Guest-join resume: once signupGuest + the SaveFreshPhraseScreen land a
    // full session, continue the parked join automatically.
    LaunchedEffectOnSession(authState) { vm.onSessionLanded() }

    when (authState) {
        // Guest onboarding: after signupGuest the auth state is
        // SaveFreshPhrase — surface the recovery-phrase prompt. On
        // acknowledgement the session lands and the resume effect continues.
        is AuthState.SaveFreshPhrase -> SaveFreshPhraseScreen(
            phrase = authRepository.freshPhrase() ?: "",
            onConfirm = { authRepository.confirmFreshPhraseSaved() },
        )
        else -> JoinCallScreen(viewModel = vm, onExit = onExit)
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

@Composable
fun JoinCallScreen(
    viewModel: JoinCallViewModel,
    onExit: () -> Unit,
) {
    val ui by viewModel.uiState.collectAsStateWithLifecycle()
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

    Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .windowInsetsPadding(WindowInsets.safeDrawing)
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            text = t(if (ui.decoded?.isOwner == true) "call.link.title_start" else "call.link.title_join"),
            style = MaterialTheme.typography.headlineMedium,
        )

        when {
            ui.loading -> Text(t("call.link.loading"))
            ui.rateLimited -> {
                Text(me.lakko.app.discovery.INVITE_RATE_LIMITED_TEXT.text())
                TextButton(onClick = { viewModel.retryDecode() }) { Text(t("common.try_again")) }
                TextButton(onClick = onExit) { Text(t("common.back")) }
            }
            ui.damaged -> {
                Text(me.lakko.app.discovery.INVITE_LINK_DAMAGED_COPY.text())
                TextButton(onClick = onExit) { Text(t("common.back")) }
            }
            ui.decoded == null || ui.decoded?.valid == false -> {
                Text(t("call.link.invalid"))
                TextButton(onClick = onExit) { Text(t("common.back")) }
            }
            else -> {
                val decoded = ui.decoded!!
                // ADR-0048 — the host through the one resolver, shown only
                // when this device knows a name (a contact or a received
                // profile). Never a fingerprint or a placeholder: a string of
                // digits names nobody, and the owner's own page has nothing to
                // say about its host.
                val appContext = androidx.compose.ui.platform.LocalContext.current.applicationContext
                val host = androidx.compose.runtime.remember(decoded.ownerSignatureKey) {
                    me.lakko.app.profile.ProfileDirectory.get(appContext).resolve(decoded.ownerSignatureKey)
                }
                if (!decoded.isOwner && !host.isPlaceholder) {
                    Text(
                        text = t("call.link.host", mapOf("name" to host.primary)),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
                ui.busyMessage?.let { message ->
                    Text(text = message.text(), color = MaterialTheme.colorScheme.error)
                    androidx.compose.foundation.layout.Row(
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Button(onClick = viewModel::onEndCurrentCall) { Text(t("call.link.end_current")) }
                        TextButton(onClick = onExit) { Text(t("common.back")) }
                    }
                }
                when (ui.phase) {
                    JoinCallPhase.Idle -> {
                        // ADR-0048 UX §4 — a guest (no session) is asked for a
                        // name before Join mints the guest account.
                        val asksName = viewModel.needsGuestName()
                        if (asksName) {
                            androidx.compose.material3.OutlinedTextField(
                                value = ui.guestName,
                                onValueChange = viewModel::onGuestNameChange,
                                label = { Text(t("settings.profile.name.label")) },
                                singleLine = true,
                                isError = ui.guestNameError != null,
                                supportingText = ui.guestNameError?.let { reason -> { Text(reason.text()) } },
                            )
                        }
                        Button(
                            enabled = !asksName || ui.guestNameError == null,
                            onClick = {
                                callPermissions.request(video = decoded.mediaVideo) { v ->
                                    viewModel.onStart(videoAllowed = v)
                                }
                            },
                        ) {
                            Text(if (decoded.isOwner) t("call.link.start") else t("call.link.join"))
                        }
                        if (ui.busyMessage == null) {
                            TextButton(onClick = onExit) { Text(t("common.back")) }
                        }
                    }
                    JoinCallPhase.Starting -> {
                        if (ui.guestWorking) {
                            // ADR-0054 UX section 1 - the guest account's one
                            // heavy piece of work.
                            androidx.compose.material3.CircularProgressIndicator()
                            Text(t("call.guest.working.title"))
                            Text(
                                text = t("auth.signup.working.line"),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            if (ui.guestSolving) {
                                TextButton(onClick = viewModel::onCancelGuest) { Text(t("common.cancel")) }
                            }
                        } else {
                            Text(t("call.link.connecting"))
                            // Only once the host holds the call.
                            if (viewModel.hostHoldsThisCall(ui)) {
                                TextButton(onClick = viewModel::onCancel) { Text(t("common.cancel")) }
                            }
                        }
                    }
                    JoinCallPhase.Waiting -> {
                        Text(t("call.link.waiting"))
                        Text(
                            text = t("call.link.waiting_hint"),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        androidx.compose.foundation.layout.Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            TextButton(onClick = viewModel::onCancel) { Text(t("common.cancel")) }
                            TextButton(onClick = onExit) { Text(t("common.back")) }
                        }
                    }
                    JoinCallPhase.Live -> {
                        Text(t("call.link.live"))
                        TextButton(onClick = onExit) { Text(t("common.back")) }
                    }
                    JoinCallPhase.Dead -> {
                        Text(t("call.link.dead"))
                        TextButton(onClick = onExit) { Text(t("common.back")) }
                    }
                    JoinCallPhase.Ended -> {
                        Text(t("call.link.ended"))
                        TextButton(onClick = onExit) { Text(t("common.back")) }
                    }
                    JoinCallPhase.Error -> {
                        Text(
                            text = (ui.error ?: UiText("call.link.error.generic")).text(),
                            color = MaterialTheme.colorScheme.error,
                        )
                        androidx.compose.foundation.layout.Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            TextButton(onClick = viewModel::onTryAgain) { Text(t("common.try_again")) }
                            TextButton(onClick = onExit) { Text(t("common.back")) }
                        }
                    }
                }
            }
        }
    }
    }
}
