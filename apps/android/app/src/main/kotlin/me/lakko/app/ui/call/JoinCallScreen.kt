// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.call

import android.content.Context
import android.util.Log
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.IdentitySecrets
import me.lakko.app.calls.CallUiState
import me.lakko.app.calls.MultiJoinerCall
import me.lakko.app.calls.ParsedCallInvite
import me.lakko.app.calls.SfuConfig
import me.lakko.app.calls.hexKey
import me.lakko.app.calls.parseCallInviteSlug
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.ui.auth.SaveFreshPhraseScreen
import me.lakko.sframe.SFrameAvailability
import uniffi.lakko_ffi_uniffi.CallInviteCapability
import uniffi.lakko_ffi_uniffi.Identity

/**
 * `/c/<slug>` multi-joiner call-invite landing (ADR-0032, G-21/G-22) — the
 * Android twin of `apps/web/src/routes/JoinCall.tsx`.
 *
 * Whoever opens a `/c/<slug>` call link lands here. The screen decodes +
 * verifies the [CallInviteCapability] (owner's Ed25519 signature), then drives
 * the ADR-0032 owner-founds flow via [MultiJoinerCall]:
 *
 *   * If the visitor IS the owner (its identity signature key matches the
 *     capability's owner key), it FOUNDS + hosts the call
 *     ([MultiJoinerCall.host]).
 *   * Otherwise it JOINS: mint a fresh KeyPackage, connect to the SFU room,
 *     relay the KeyPackage to the acting committer, and wait for the Welcome
 *     ([MultiJoinerCall.join]). Until the Welcome lands the screen shows a
 *     "waiting for the owner to admit you" state (invariant #4 — no media
 *     before the group is joined). If no committer responds within the
 *     admission window the call is dead (owner left / call ended).
 *
 * ## Guest join (no login required, ADR-0032 UX)
 *
 * An anonymous visitor (no session) can join WITHOUT logging in: on tapping
 * Join we auto-create a FULL, username-less account via
 * [AuthRepository.signupGuest] (reusing the audited PoW-gated signup
 * machinery). That flips auth to [AuthState.SaveFreshPhrase], so [JoinCallRoot]
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
    /** `null` = invalid / expired / not-found link. */
    val decoded: DecodedCall? = null,
    val phase: JoinCallPhase = JoinCallPhase.Idle,
    val error: String? = null,
    /** ADR-0048 UX §4 — a guest's "Your name", prefilled `Guest`. */
    val guestName: String = GUEST_NAME_DEFAULT,
    /** The reason the typed guest name fails the rule, or `null`. */
    val guestNameError: String? = null,
)

/** ADR-0048 UX §4 — the guest name field's prefill. */
internal const val GUEST_NAME_DEFAULT: String = "Guest"


class JoinCallViewModel(
    private val authRepository: AuthRepository,
    private val applicationContext: Context,
    private val slug: String,
) : ViewModel() {

    private val _uiState = MutableStateFlow(JoinCallUiState())
    val uiState: StateFlow<JoinCallUiState> = _uiState.asStateFlow()

    /** The shared call-UI state the in-call [me.lakko.app.ui.call.CallScreen]
     *  renders once the call is live. Owned here for the screen's lifetime. */
    val callUiState: CallUiState = CallUiState()

    private var call: MultiJoinerCall? = null
    /** The canonical Identity built for this call's lifetime; freed on exit. */
    private var callIdentity: Identity? = null
    /** The join to resume once a guest account's session has landed. */
    private var pendingJoin: DecodedCall? = null

    // FINDING 5: synchronous double-tap latches (set BEFORE any suspend), so a
    // rapid second tap can't mint a SECOND guest account or start a second
    // call.
    private var mintingGuest = false
    private var beginning = false

    init {
        viewModelScope.launch {
            val d = decode(slug)
            _uiState.value = _uiState.value.copy(loading = false, decoded = d)
        }
        // ADR-0032 piece B — mid-call camera-on bridge. The in-call
        // [CallScreen] is presented WITHOUT a session (only [callUiState]), so
        // the camera button only flips the store's local `camOn`; mirror it
        // onto the live [MultiJoinerCall] here (the analog of the web camera
        // effect). camOn TRUE -> `publishVideo` (acquire + SFrame-seal +
        // renegotiate); FALSE -> disable the track. On a failure revert the
        // toggle — fail-safe, the call stays up audio-only (invariant #4).
        viewModelScope.launch {
            callUiState.participants
                .map { list -> list.firstOrNull { it.isLocal }?.camOn ?: false }
                .distinctUntilChanged()
                .collect { camOn ->
                    val c = call ?: return@collect
                    if (camOn) {
                        try {
                            c.publishVideo()
                        } catch (err: Throwable) {
                            callUiState.setCamera(false)
                            Log.w("JoinCall", "mid-call publishVideo failed — reverting camera", err)
                        }
                    } else {
                        c.setCameraEnabled(false)
                    }
                }
        }
    }

    private suspend fun decode(slug: String): DecodedCall? {
        return try {
            val parsed = parseCallInviteSlug(slug)
            val capabilityBytes: ByteArray = when (parsed) {
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
            CallInviteCapability.decodeAndVerify(capabilityBytes).use { cap ->
                val secrets = authRepository.identitySecrets.value
                val isOwner = secrets != null &&
                    Identity.fromSeed(secrets.accountIdentitySeed).use {
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
        } catch (e: DiscoveryHttpError) {
            if (e.status == 404) null else {
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
        _uiState.value = _uiState.value.copy(
            guestName = value,
            guestNameError = (check as? me.lakko.app.profile.DisplayNameCheck.Invalid)?.problem?.copy,
        )
    }

    /** Tap Join: use the live session if there is one, else create a guest
     *  account first (username-less full account) and resume after the phrase
     *  is saved. The screen has already obtained the microphone (and, for a
     *  video link, camera) permission; [videoAllowed] is `false` when the
     *  camera was refused and the user chose audio only. */
    fun onStart(videoAllowed: Boolean = true) {
        val decoded = _uiState.value.decoded ?: return
        // A video link joined after the camera was refused joins audio only
        // (the user chose that in the permission dialog).
        val d = if (videoAllowed) decoded else decoded.copy(mediaVideo = false)
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
        _uiState.value = _uiState.value.copy(phase = JoinCallPhase.Starting, error = null)
        pendingJoin = d
        viewModelScope.launch {
            try {
                authRepository.signupGuest()
            } catch (err: Throwable) {
                mintingGuest = false // allow a retry after a failed mint
                pendingJoin = null
                _uiState.value = _uiState.value.copy(
                    phase = JoinCallPhase.Error,
                    error = err.message ?: err.toString(),
                )
            }
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
        val nameCheck = me.lakko.app.profile.checkDisplayName(_uiState.value.guestName)
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
                }
            } catch (err: Throwable) {
                Log.w("JoinCall", "guest profile not saved", err)
            }
        }
        viewModelScope.launch { beginJoin(d, secrets) }
    }

    /** Run the actual ADR-0032 host/join now that a live session exists. */
    private suspend fun beginJoin(d: DecodedCall, secrets: IdentitySecrets) {
        // Synchronous latch (FINDING 5): guard against a rapid double-tap
        // starting two SFU sessions before `call` is assigned.
        if (call != null || beginning) return
        beginning = true

        val base = SfuConfig.baseUrl
        if (base == null) {
            beginning = false
            _uiState.value = _uiState.value.copy(
                phase = JoinCallPhase.Error,
                error = "This client has no SFU endpoint — set lakko.sfu.url and rebuild.",
            )
            return
        }
        // Fail-closed: no SFrame native shim => no call (invariant #4).
        if (!SFrameAvailability.isCallingAvailable) {
            beginning = false
            _uiState.value = _uiState.value.copy(
                phase = JoinCallPhase.Error,
                error = "Secure media encryption is unavailable on this device.",
            )
            return
        }
        _uiState.value = _uiState.value.copy(phase = JoinCallPhase.Starting, error = null)

        // Build the canonical identity for the call's lifetime.
        val identity = try {
            Identity.fromSeed(secrets.accountIdentitySeed)
        } catch (e: Throwable) {
            beginning = false
            _uiState.value = _uiState.value.copy(
                phase = JoinCallPhase.Error,
                error = "Re-authenticate to join this call (no live identity).",
            )
            return
        }
        callIdentity = identity

        val opts = MultiJoinerCall.Options(
            applicationContext = applicationContext,
            identity = identity,
            callId = d.callId,
            ownerAccountId = d.ownerAccountId,
            ownerSignatureKey = d.ownerSignatureKey,
            sfuBaseUrl = base,
            sessionToken = secrets.sessionToken,
            audio = true,
            video = d.mediaVideo,
            ui = callUiState,
            scope = viewModelScope,
            onJoined = {
                _uiState.value = _uiState.value.copy(phase = JoinCallPhase.Live)
            },
            onEnded = {
                _uiState.value = _uiState.value.copy(phase = JoinCallPhase.Ended)
            },
            onNoCommitter = {
                call = null
                _uiState.value = _uiState.value.copy(phase = JoinCallPhase.Dead)
            },
            onError = { err ->
                _uiState.value = _uiState.value.copy(
                    phase = JoinCallPhase.Error,
                    error = err.message ?: err.toString(),
                )
            },
        )

        try {
            call = if (d.isOwner) {
                // Owner founds + hosts; onJoined fires synchronously in host().
                MultiJoinerCall.host(opts)
            } else {
                // Joiner connects signaling-only + relays its KeyPackage; sit
                // in "waiting" until the Welcome arrives (onJoined -> Live).
                val c = MultiJoinerCall.join(opts)
                if (_uiState.value.phase == JoinCallPhase.Starting) {
                    _uiState.value = _uiState.value.copy(phase = JoinCallPhase.Waiting)
                }
                c
            }
        } catch (err: Throwable) {
            beginning = false // start failed (call unset) — allow a retry
            identity.close()
            callIdentity = null
            _uiState.value = _uiState.value.copy(
                phase = JoinCallPhase.Error,
                error = err.message ?: err.toString(),
            )
        }
    }

    /** Tear down the call + free the call identity. Idempotent. */
    fun onExit() {
        call?.hangUp()
        call = null
        callIdentity?.close()
        callIdentity = null
        callUiState.reset()
    }

    override fun onCleared() {
        call?.hangUp()
        call = null
        callIdentity?.close()
        callIdentity = null
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
) {
    val authState by authRepository.state.collectAsStateWithLifecycle()
    val vm: JoinCallViewModel = viewModel(
        key = "joincall-$slug",
        factory = object : ViewModelProvider.Factory {
            @Suppress("UNCHECKED_CAST")
            override fun <T : ViewModel> create(modelClass: Class<T>): T =
                JoinCallViewModel(authRepository, applicationContext, slug) as T
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
    val ui by viewModel.uiState.collectAsState()
    // Hosting or joining a call link needs the microphone, and the camera for
    // a video link — asked here, before any account is minted or any SFU
    // connection opened.
    val callPermissions = rememberCallPermissionGate()

    // Once live, the shared in-call UI takes the whole screen.
    if (ui.phase == JoinCallPhase.Live) {
        Box(modifier = Modifier.fillMaxSize()) {
            CallScreen(
                onExit = {
                    viewModel.onExit()
                    onExit()
                },
                state = viewModel.callUiState,
            )
        }
        return
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(text = "Join call", style = MaterialTheme.typography.headlineMedium)

        when {
            ui.loading -> Text("Loading call link…")
            ui.decoded == null || ui.decoded?.valid == false -> {
                Text("This call link is invalid or has expired.")
                TextButton(onClick = onExit) { Text("Go back") }
            }
            else -> {
                val decoded = ui.decoded!!
                // ADR-0048 — the host through the one resolver: a name when this
                // device knows one, else the fingerprint without `@`.
                val appContext = androidx.compose.ui.platform.LocalContext.current.applicationContext
                val host = androidx.compose.runtime.remember(decoded.ownerSignatureKey) {
                    me.lakko.app.profile.ProfileDirectory.get(appContext).resolve(decoded.ownerSignatureKey)
                }
                Text(
                    text = "Host: ${host.listLabel}",
                    style = MaterialTheme.typography.bodyMedium,
                    fontFamily = if (host.isFingerprint) FontFamily.Monospace else FontFamily.Default,
                )
                when (ui.phase) {
                    JoinCallPhase.Idle -> {
                        // ADR-0048 UX §4 — a guest (no session) is asked for a
                        // name before Join mints the guest account.
                        val asksName = viewModel.needsGuestName()
                        if (asksName) {
                            androidx.compose.material3.OutlinedTextField(
                                value = ui.guestName,
                                onValueChange = viewModel::onGuestNameChange,
                                label = { Text("Your name") },
                                singleLine = true,
                                isError = ui.guestNameError != null,
                                supportingText = ui.guestNameError?.let { reason -> { Text(reason) } },
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
                            Text(if (decoded.isOwner) "Start call" else "Join call")
                        }
                    }
                    JoinCallPhase.Starting -> Text("Connecting…")
                    JoinCallPhase.Waiting -> Text("Waiting for the owner to admit you…")
                    JoinCallPhase.Dead -> {
                        Text("This call has ended or isn't active.")
                        TextButton(onClick = onExit) { Text("Go back") }
                    }
                    JoinCallPhase.Ended -> {
                        Text("The call has ended.")
                        TextButton(onClick = onExit) { Text("Go back") }
                    }
                    JoinCallPhase.Error -> {
                        Text(
                            text = "Failed: ${ui.error ?: "unknown error"}",
                            color = MaterialTheme.colorScheme.error,
                        )
                        TextButton(onClick = onExit) { Text("Go back") }
                    }
                    // Live is handled above.
                    JoinCallPhase.Live -> Unit
                }
            }
        }
    }
}
