// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import me.lakko.app.l10n.UiText
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch
import org.webrtc.VideoTrack

/**
 * In-call UI state (Phase 3c6-android).
 *
 * Mirror of `apps/web/src/state/call-state.ts` + `apps/ios/.../CallUiState.swift`.
 * Same Participant shape, same CallPhase variants, same actions.
 * Compose subscribes to the [StateFlow]s via
 * `collectAsStateWithLifecycle`; mutations go through the
 * `setX` methods.
 *
 * No WebRTC integration here — the live media path lands in
 * the platform's WebRTC pod (3a-pod-followup). Until then,
 * UI consumes mock data + the BandwidthController (#39) for
 * the connection-quality glyph.
 */
public class CallUiState(
    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate),
) {

    // ----------------------------------------------------------
    // Types
    // ----------------------------------------------------------

    public enum class QualityTier {
        Excellent, Good, Fair, Poor, Bad,
    }

    /**
     * ADR-0032 piece B2a — mid-call camera-on lifecycle, mirrored from
     * whichever session drives the screen (the 1×1 [CallSession] via
     * [driveFrom], or the SFU [SfuCallSession] via [SfuCallUi]). One surface
     * the `CallScreen` reads for the camera-button spinner + the self-tile
     * "Starting camera…" hint. [Starting] is the only value the UI reacts to;
     * the rest are terminal.
     */
    public enum class VideoPublishState { Off, Starting, Live, Failed }

    public data class Participant(
        val id: String,
        val displayName: String,
        val isLocal: Boolean,
        val micOn: Boolean,
        val camOn: Boolean,
        val handRaised: Boolean,
        val quality: QualityTier,
        val speaking: Boolean,
        val screenSharing: Boolean,
    )

    public sealed class CallPhase {
        /**
         * Dialled, nobody has picked up yet — `docs/ux/calls.md` sections 3.1
         * and 4.1. This used to be `Prejoin`, the state in which the screen
         * showed a settings card asking the user to configure the call before
         * it was placed. Section 3.1 removed that screen ("Tapping the phone
         * button places the call immediately"), so the first state of a call is
         * now the call itself, reading `Calling...`.
         */
        public data object Calling : CallPhase()
        public data object Connecting : CallPhase()
        public data object Connected : CallPhase()
        public data class Reconnecting(val reason: String) : CallPhase()
        public data class Ended(val endedAtMs: Long) : CallPhase()
    }

    public data class E2eeState(
        val active: Boolean,
        /** 12-hex-char short fingerprint (BLAKE3 over the
         *  CallKey seed). `null` while `active == false`. */
        val shortFingerprint: String?,
    ) {
        public companion object {
            public val Inactive: E2eeState = E2eeState(active = false, shortFingerprint = null)
        }
    }

    public data class ConnectionDetail(
        val rttMs: Int?,
        val downBps: Int?,
        val upBps: Int?,
        val connectionType: ConnectionType,
    ) {
        public enum class ConnectionType { Direct, Relay, Unknown }

        public companion object {
            public val Unknown: ConnectionDetail = ConnectionDetail(
                rttMs = null, downBps = null, upBps = null,
                connectionType = ConnectionType.Unknown,
            )
        }
    }

    /** A capture / playback device the OS exposes
     *  (S-3-A-settings). Same shape as the web `CallDevice`
     *  and iOS counterpart. */
    public data class CallDevice(
        val id: String,
        val label: String,
        val kind: Kind,
    ) {
        public enum class Kind { AudioInput, VideoInput, AudioOutput }
    }

    /** In-call audio / video settings (S-3-A-settings).
     *  Drives the gear-menu device pickers + the processing
     *  toggles applied to the local capture chain. */
    public data class CallSettings(
        val selectedAudioInput: String? = null,
        val selectedVideoInput: String? = null,
        val selectedAudioOutput: String? = null,
        val availableDevices: List<CallDevice> = emptyList(),
        val noiseSuppression: Boolean = true,
        val echoCancellation: Boolean = true,
        val autoGain: Boolean = true,
        val mirrorLocal: Boolean = true,
        val lowDataMode: Boolean = false,
    ) {
        public companion object {
            public val Default: CallSettings = CallSettings()
        }
    }

    /** A single chat message inside the call (S-3-A-chat).
     *  Same shape as the web `ChatMessage` and iOS counterpart. */
    public data class ChatMessage(
        val id: String,
        val kind: Kind,
        /** Author participant id; `null` for system events. */
        val senderId: String?,
        val senderName: String,
        val text: String,
        /** Unix milliseconds. */
        val tsMs: Long,
        /** A system event's words, rendered in the interface language
         *  (ADR-0061); [text] is empty for one. */
        val systemText: UiText? = null,
    ) {
        public enum class Kind { Text, System }
    }

    // ----------------------------------------------------------
    // State flows
    // ----------------------------------------------------------

    private val _phase = MutableStateFlow<CallPhase>(CallPhase.Calling)
    public val phase: StateFlow<CallPhase> = _phase.asStateFlow()

    private val _connectedSinceMs = MutableStateFlow<Long?>(null)

    /**
     * Wall clock at the moment this call's media first flowed, or `null` if it
     * has not. Latched once and kept for the call's lifetime; cleared only by
     * [reset], which is what a new call gets.
     *
     * It exists for the collapsed bar of `docs/ux/calls.md` section 4.5. The
     * call screen derives its running time from the moment IT observed the
     * connect, which is fine for a screen that is up for the whole call and
     * wrong for a bar: putting the call away and bringing it back destroys and
     * rebuilds the screen, and a timer restarted from zero at that point would
     * be a clock the person can watch being wrong. So the moment is held by the
     * call's state, which outlives every view of it.
     */
    public val connectedSinceMs: StateFlow<Long?> = _connectedSinceMs.asStateFlow()

    /**
     * Every write of [phase] goes through here, so the latch above cannot be
     * bypassed by a path that sets the phase directly.
     */
    private fun applyPhase(next: CallPhase) {
        if (next is CallPhase.Connected && _connectedSinceMs.value == null) {
            _connectedSinceMs.value = System.currentTimeMillis()
        }
        _phase.value = next
    }

    private val _participants = MutableStateFlow<List<Participant>>(emptyList())
    public val participants: StateFlow<List<Participant>> = _participants.asStateFlow()

    /** `null` = follow active speaker. */
    private val _pinnedId = MutableStateFlow<String?>(null)
    public val pinnedId: StateFlow<String?> = _pinnedId.asStateFlow()

    private val _participantListOpen = MutableStateFlow(false)
    public val participantListOpen: StateFlow<Boolean> = _participantListOpen.asStateFlow()

    private val _e2ee = MutableStateFlow(E2eeState.Inactive)
    public val e2ee: StateFlow<E2eeState> = _e2ee.asStateFlow()

    private val _connectionDetail = MutableStateFlow(ConnectionDetail.Unknown)
    public val connectionDetail: StateFlow<ConnectionDetail> = _connectionDetail.asStateFlow()

    private val _qualityPopoverOpen = MutableStateFlow(false)
    public val qualityPopoverOpen: StateFlow<Boolean> = _qualityPopoverOpen.asStateFlow()

    private val _hasMultipleCameras = MutableStateFlow(false)
    public val hasMultipleCameras: StateFlow<Boolean> = _hasMultipleCameras.asStateFlow()

    /**
     * `docs/ux/calls.md` section 4.1 — which camera is being captured, so the
     * self-view knows whether to mirror. The flip button lives in the
     * self-view's corner and this is what it moves; a back camera shown
     * mirrored would put the world the wrong way round.
     */
    private val _cameraFrontFacing = MutableStateFlow(true)
    public val cameraFrontFacing: StateFlow<Boolean> = _cameraFrontFacing.asStateFlow()

    /** See [cameraFrontFacing]. Set by whichever session owns the capture. */
    public fun setCameraFrontFacing(front: Boolean) {
        _cameraFrontFacing.value = front
    }

    /** ADR-0032 piece B2a — mid-call camera-on lifecycle (see
     *  [VideoPublishState]). Driven from the bound session; `CallScreen`
     *  reads it for the camera-button spinner + the self-tile "Starting
     *  camera…" hint. Untouched by [seedMockState] (the mock design path
     *  never spins). */
    private val _videoPublishState = MutableStateFlow(VideoPublishState.Off)
    public val videoPublishState: StateFlow<VideoPublishState> =
        _videoPublishState.asStateFlow()

    /** Gear-menu open / closed (S-3-A-settings). */
    private val _settingsOpen = MutableStateFlow(false)
    public val settingsOpen: StateFlow<Boolean> = _settingsOpen.asStateFlow()

    /** Settings + enumerated devices (S-3-A-settings). */
    private val _settings = MutableStateFlow(CallSettings.Default)
    public val settings: StateFlow<CallSettings> = _settings.asStateFlow()

    /** Chat panel open / closed (S-3-A-chat). */
    private val _chatOpen = MutableStateFlow(false)
    public val chatOpen: StateFlow<Boolean> = _chatOpen.asStateFlow()

    /** Chat-message log — oldest first. */
    private val _chatMessages = MutableStateFlow<List<ChatMessage>>(emptyList())
    public val chatMessages: StateFlow<List<ChatMessage>> = _chatMessages.asStateFlow()

    /** In-progress compose-box content. */
    private val _composeText = MutableStateFlow("")
    public val composeText: StateFlow<String> = _composeText.asStateFlow()

    /** Unread-message count since the user last opened the
     *  chat panel. System events do not count towards unread. */
    private val _chatUnreadCount = MutableStateFlow(0)
    public val chatUnreadCount: StateFlow<Int> = _chatUnreadCount.asStateFlow()

    /** G3-2-video-ui-screen — live local-camera track from the
     *  underlying [CallSession] (when one is attached via
     *  [bindTo]). `null` while audio-only, while the prejoin
     *  flow is still up, or while the camera-pipeline init is
     *  in-flight. Compose consumers (the self-tile in the
     *  participant grid, the prejoin preview) render
     *  `LocalVideoPreviewView(track:)` when this is non-null
     *  **and** the local participant's `camOn == true`.
     *  Untouched by [seedMockState] — the mock-driven design
     *  path keeps the legacy "(video)" placeholder. */
    private val _localVideoTrack = MutableStateFlow<VideoTrack?>(null)
    public val localVideoTrack: StateFlow<VideoTrack?> = _localVideoTrack.asStateFlow()

    /** G3-2-remote-video — the remote participant's live video
     *  track (when one is attached via [bindTo]). `null` for
     *  audio-only, before the remote adds a camera, or after
     *  teardown. The 1×1 direct-PeerConnection path has exactly
     *  one remote track; `CallScreen` renders it on the **sole
     *  remote participant's** tile (see [soleRemoteParticipantId]).
     *  Untouched by [seedMockState] so the mock design path keeps
     *  the legacy placeholder; the N-way SFU path will replace
     *  this with a per-participant track map. */
    private val _remoteVideoTrack = MutableStateFlow<VideoTrack?>(null)
    public val remoteVideoTrack: StateFlow<VideoTrack?> = _remoteVideoTrack.asStateFlow()

    /** N-way SFU remote video — the per-source-peer video track map,
     *  keyed on the roster participant id (`"peer-<streamId>"`, where the
     *  MSID stream id == the source peer id per the SFU contract). The
     *  group [SfuCallUi] path populates this so `CallScreen` renders each
     *  peer's own video on its own tile; a `null` track removes the entry
     *  (peer left / camera off). The single [remoteVideoTrack] above is
     *  retained for the 1×1 P2P [driveFrom] path (a direct PeerConnection
     *  has no MSID map). Untouched by [seedMockState]. */
    private val _remoteVideoTracks = MutableStateFlow<Map<String, VideoTrack>>(emptyMap())
    public val remoteVideoTracks: StateFlow<Map<String, VideoTrack>> =
        _remoteVideoTracks.asStateFlow()

    /** Job for an active [bindTo] subscription. Cancelled on
     *  [reset] / re-bind to a different session so the previous
     *  flow doesn't keep pushing onto the StateFlow. */
    private var bindJob: Job? = null

    /** Monotone counter for client-side chat-message ids. */
    private var chatIdCounter: Long = 0
    private fun nextChatId(): String {
        chatIdCounter += 1
        return "local-${System.currentTimeMillis() / 1000}-$chatIdCounter"
    }

    // ----------------------------------------------------------
    // Derived helpers (UI calls these inline; Compose recomposes
    // on the StateFlow this peeks at).
    // ----------------------------------------------------------

    /** The tile the group stage draws large, or `null` for an equal grid. The
     *  rule is [me.lakko.app.calls.featuredParticipant] (section 4.4), shared
     *  with the web and iOS stores. */
    public fun featuredParticipant(): Participant? =
        featuredParticipant(_participants.value, _pinnedId.value)

    /** The grid: everyone but the featured tile in roster order, or everyone
     *  when there is no featured tile. */
    public fun galleryParticipants(): List<Participant> =
        galleryParticipants(_participants.value, featuredParticipant())

    /** Local participant quality (drives the header dot). */
    public fun localQualityTier(): QualityTier =
        _participants.value.firstOrNull { it.isLocal }?.quality ?: QualityTier.Good

    /** `true` when no remote participants are present. */
    public fun isAlone(): Boolean =
        _participants.value.none { !it.isLocal }

    // ----------------------------------------------------------
    // Mutating actions
    // ----------------------------------------------------------

    public fun setParticipants(list: List<Participant>) {
        _participants.value = list
    }

    public fun participantJoined(p: Participant) {
        if (_participants.value.any { it.id == p.id }) return
        _participants.value = _participants.value + p
    }

    /** Rename one tile (ADR-0021 / ADR-0048 — a peer resolved to a name after
     *  it joined). No-op on an unknown id or an unchanged name. */
    public fun setParticipantName(id: String, name: String) {
        if (_participants.value.none { it.id == id && it.displayName != name }) return
        patch(id) { it.copy(displayName = name) }
    }

    public fun participantLeft(id: String) {
        _participants.value = _participants.value.filterNot { it.id == id }
        if (_pinnedId.value == id) _pinnedId.value = null
    }

    public fun setQuality(id: String, tier: QualityTier) {
        patch(id) { it.copy(quality = tier) }
    }

    public fun setSpeaking(id: String?) {
        _participants.value = _participants.value.map { it.copy(speaking = it.id == id) }
    }

    public fun setMic(on: Boolean) {
        val local = _participants.value.firstOrNull { it.isLocal } ?: return
        patch(local.id) { it.copy(micOn = on) }
    }

    public fun setCamera(on: Boolean) {
        val local = _participants.value.firstOrNull { it.isLocal } ?: return
        patch(local.id) { it.copy(camOn = on) }
    }

    public fun setHandRaised(raised: Boolean) {
        val local = _participants.value.firstOrNull { it.isLocal } ?: return
        patch(local.id) { it.copy(handRaised = raised) }
    }

    public fun setScreenSharing(on: Boolean) {
        val local = _participants.value.firstOrNull { it.isLocal } ?: return
        patch(local.id) { it.copy(screenSharing = on) }
    }

    public fun pin(id: String?) {
        _pinnedId.value = id
    }

    public fun setParticipantListOpen(open: Boolean) {
        _participantListOpen.value = open
    }

    public fun setPhase(phase: CallPhase) {
        applyPhase(phase)
    }

    public fun join() {
        applyPhase(CallPhase.Connecting)
    }

    public fun setE2ee(state: E2eeState) {
        _e2ee.value = state
    }

    public fun setConnectionDetail(detail: ConnectionDetail) {
        _connectionDetail.value = detail
    }

    public fun setQualityPopoverOpen(open: Boolean) {
        _qualityPopoverOpen.value = open
    }

    public fun setHasMultipleCameras(many: Boolean) {
        _hasMultipleCameras.value = many
    }

    /** ADR-0032 piece B2a — push the mid-call camera-on lifecycle. Driven by
     *  the bound session ([driveFrom] for the 1×1 path, [SfuCallUi] for the
     *  SFU path); exposed as a setter so both callers converge on the one UI
     *  surface `CallScreen` reads. */
    public fun setVideoPublishState(s: VideoPublishState) {
        _videoPublishState.value = s
    }

    // ----------------------------------------------------------
    // Settings actions (S-3-A-settings)
    // ----------------------------------------------------------

    /** Open / close the gear menu. */
    public fun setSettingsOpen(open: Boolean) {
        _settingsOpen.value = open
    }

    /** Replace the enumerated devices snapshot. Called by the
     *  WebRTC pod after `AudioManager.getDevices()` +
     *  `CameraManager.cameraIdList` have produced their
     *  results. */
    public fun setAvailableDevices(devices: List<CallDevice>) {
        _settings.value = _settings.value.copy(availableDevices = devices)
    }

    public fun setSelectedAudioInput(id: String?) {
        _settings.value = _settings.value.copy(selectedAudioInput = id)
    }

    public fun setSelectedVideoInput(id: String?) {
        _settings.value = _settings.value.copy(selectedVideoInput = id)
    }

    public fun setSelectedAudioOutput(id: String?) {
        _settings.value = _settings.value.copy(selectedAudioOutput = id)
    }

    public fun setNoiseSuppression(on: Boolean) {
        _settings.value = _settings.value.copy(noiseSuppression = on)
    }

    public fun setEchoCancellation(on: Boolean) {
        _settings.value = _settings.value.copy(echoCancellation = on)
    }

    public fun setAutoGain(on: Boolean) {
        _settings.value = _settings.value.copy(autoGain = on)
    }

    public fun setMirrorLocal(on: Boolean) {
        _settings.value = _settings.value.copy(mirrorLocal = on)
    }

    public fun setLowDataMode(on: Boolean) {
        _settings.value = _settings.value.copy(lowDataMode = on)
    }

    // ----------------------------------------------------------
    // Chat actions (S-3-A-chat)
    // ----------------------------------------------------------

    /** Open / close the chat panel. Opening clears the unread
     *  badge. */
    public fun setChatOpen(open: Boolean) {
        _chatOpen.value = open
        if (open) _chatUnreadCount.value = 0
    }

    /** Update the in-progress compose-box content. */
    public fun setComposeText(text: String) {
        _composeText.value = text
    }

    /** Send a chat message from the local participant. Returns
     *  the constructed message so the WebRTC pod can hand it
     *  to MLS application-message encryption. Returns `null`
     *  for whitespace-only input. */
    public fun sendChatMessage(text: String): ChatMessage? {
        val trimmed = text.trim()
        if (trimmed.isEmpty()) return null
        val local = _participants.value.firstOrNull { it.isLocal }
        val senderId = local?.id ?: "local"
        // Empty: this device's own name is drawn as `call.participant.you`.
        val senderName = local?.displayName.orEmpty()
        val msg = ChatMessage(
            id = nextChatId(),
            kind = ChatMessage.Kind.Text,
            senderId = senderId,
            senderName = senderName,
            text = trimmed,
            tsMs = System.currentTimeMillis(),
        )
        _chatMessages.value = _chatMessages.value + msg
        _composeText.value = ""
        return msg
    }

    /** Receive a chat message from a remote participant
     *  (decrypted MLS application message). If the chat panel
     *  is closed, the unread badge increments. */
    public fun receiveChatMessage(msg: ChatMessage) {
        _chatMessages.value = _chatMessages.value + msg
        if (!_chatOpen.value && msg.kind == ChatMessage.Kind.Text) {
            _chatUnreadCount.value = _chatUnreadCount.value + 1
        }
    }

    /** Append a system event ("Alice joined the call"). Does
     *  NOT bump the unread counter — system events are
     *  background context. */
    public fun addSystemEvent(text: UiText) {
        val msg = ChatMessage(
            id = nextChatId(),
            kind = ChatMessage.Kind.System,
            senderId = null,
            senderName = "",
            text = "",
            tsMs = System.currentTimeMillis(),
            systemText = text,
        )
        _chatMessages.value = _chatMessages.value + msg
    }

    public fun clearChatUnread() {
        _chatUnreadCount.value = 0
    }

    // ----------------------------------------------------------
    // Live video track (G3-2-video-ui-screen)
    // ----------------------------------------------------------

    /** Push a fresh local video track into the state. Callers
     *  are typically the [bindTo] coroutine collector; an
     *  explicit setter is exposed so testbeds + previews can
     *  drive the UI without standing up a real [CallSession]. */
    public fun setLocalVideoTrack(track: VideoTrack?) {
        _localVideoTrack.value = track
    }

    /** G3-2-remote-video — push the remote participant's video
     *  track. Same [bindTo]-driven / testbed-explicit dual use
     *  as [setLocalVideoTrack]. */
    public fun setRemoteVideoTrack(track: VideoTrack?) {
        _remoteVideoTrack.value = track
    }

    /**
     * N-way SFU remote video — set / remove a single source peer's video
     * track, keyed on its roster participant id (`"peer-<streamId>"`). A
     * `null` [track] removes that peer's entry (peer left / camera off).
     * Copy-on-write against the current map so Compose collectors observe
     * a fresh instance. Distinct from the single-track [setRemoteVideoTrack]
     * overload the 1×1 [driveFrom] path uses — the two do not interfere.
     */
    public fun setRemoteVideoTrack(track: VideoTrack?, participantId: String) {
        _remoteVideoTracks.value =
            if (track == null) {
                _remoteVideoTracks.value - participantId
            } else {
                _remoteVideoTracks.value + (participantId to track)
            }
    }

    /**
     * N-way SFU — flip a specific participant's camera flag. Used for a
     * REMOTE peer when its video track arrives / is dropped, so the tile's
     * video surface (gated on `camOn`) actually renders — the Kotlin twin
     * of the web adapter's `setParticipantCamera`. Distinct from [setCamera],
     * which only ever touches the local participant. No-op if [id] is unknown.
     */
    public fun setParticipantCamera(id: String, on: Boolean) {
        patch(id) { it.copy(camOn = on) }
    }

    /** The id of the only remote participant, when exactly one
     *  exists (the 1×1 case); `null` when alone or in an N-way
     *  call. `CallScreen` gates remote-video rendering on
     *  `participant.id == soleRemoteParticipantId` so the single
     *  [remoteVideoTrack] handle is never painted onto the wrong
     *  tile in a group call (per-participant track mapping is the
     *  deferred N-way follow-up). */
    public val soleRemoteParticipantId: String?
        get() {
            val remotes = _participants.value.filter { !it.isLocal }
            return if (remotes.size == 1) remotes.first().id else null
        }

    /**
     * Attach a live [CallSession] so the UI mirrors its video
     * StateFlows. Bridges the local **and** remote video tracks
     * (G3-2-video-ui-screen + G3-2-remote-video); the audio /
     * participant / e2ee / connection-detail fields stay on the
     * mock path until the full-session-integration sub-sprint
     * lands (the screen today reads from [seedMockState]).
     *
     * Cancels any previous bind subscription so re-attaching
     * to a different session is safe.
     */
    public fun bindTo(session: CallSession) {
        bindJob?.cancel()
        bindJob = scope.launch {
            launch {
                session.localVideoTrack.collect { track ->
                    _localVideoTrack.value = track
                }
            }
            launch {
                session.remoteVideoTrack.collect { track ->
                    _remoteVideoTrack.value = track
                }
            }
        }
    }

    /**
     * G3-2-session-bind — drive the **full** mock-free UI from a
     * live 1×1 [CallSession]. Maps `session.state` → [CallPhase],
     * synthesises the 2-person roster (local always; the single
     * remote appears once the call is `Connected` or a remote
     * track has arrived), and bridges both video tracks. Every
     * relevant flow emission triggers a full rebuild from the
     * session's current truth, so there are no partial-update
     * races. Supersedes [bindTo] (reuses the same [bindJob] so a
     * later [reset] cancels it).
     *
     * Scope (1×1): roster is exactly two; `camOn` is driven by
     * track presence. **Not** mapped here (each needs a dedicated
     * [CallSession] signal, deferred): the e2ee fingerprint pill,
     * connection-quality detail, and the mic / hand / share
     * controls writing back into the session. The prejoin Join →
     * `dial` / `acceptCall` action and the navigation that
     * presents this screen with a session are separate
     * follow-ups — `CallScreen` calls this when a session is
     * injected and falls back to [seedMockState] otherwise.
     */
    public fun driveFrom(
        session: CallSession,
        /** ADR-0048 — the remote person's resolved name ("" when unknown). */
        remoteName: String = "",
    ) {
        this.remoteName = remoteName
        bindJob?.cancel()
        bindJob = scope.launch {
            // Collect all three sources; recompute the whole UI
            // snapshot on every emission from any of them.
            launch { session.state.collect { rebuildFromSession(session) } }
            launch { session.localVideoTrack.collect { rebuildFromSession(session) } }
            launch { session.remoteVideoTrack.collect { rebuildFromSession(session) } }
            launch { session.micEnabled.collect { rebuildFromSession(session) } }
            launch { session.cameraEnabled.collect { rebuildFromSession(session) } }
            launch { session.e2eeActive.collect { rebuildFromSession(session) } }
            launch { session.rttMs.collect { rebuildFromSession(session) } }
            launch { session.connectionRelayed.collect { rebuildFromSession(session) } }
            launch { session.downBps.collect { rebuildFromSession(session) } }
            launch { session.upBps.collect { rebuildFromSession(session) } }
            launch { session.videoPublishState.collect { rebuildFromSession(session) } }
            launch { session.cameraIsFrontFacing.collect { rebuildFromSession(session) } }
        }
    }

    /** ADR-0048 — the name [driveFrom] gave the 1x1 remote participant. */
    private var remoteName: String = ""

    /** Recompute phase + roster + video from the session's
     *  current state. Called from each [driveFrom] collector. */
    private fun rebuildFromSession(session: CallSession) {
        applyPhase(mapSessionPhase(session.state.value))
        val connected = session.state.value == CallSession.WebCallState.Connected
        val remoteTrack = session.remoteVideoTrack.value
        // G3-2-conn-quality — local quality dot bucketed from RTT.
        val tier = qualityTierForRtt(session.rttMs.value)
        val roster = mutableListOf(
            Participant(
                id = "local",
                // Empty: the tile draws `call.participant.you` (ADR-0061).
                displayName = "",
                isLocal = true,
                micOn = session.micEnabled.value,
                camOn = session.cameraEnabled.value,
                handRaised = false,
                quality = tier,
                speaking = false,
                screenSharing = false,
            ),
        )
        if (connected || remoteTrack != null) {
            roster.add(
                Participant(
                    id = "remote",
                    displayName = remoteName,
                    isLocal = false,
                    micOn = true,
                    camOn = remoteTrack != null,
                    handRaised = false,
                    quality = QualityTier.Good,
                    speaking = false,
                    screenSharing = false,
                ),
            )
        }
        _participants.value = roster
        _cameraFrontFacing.value = session.cameraIsFrontFacing.value
        _localVideoTrack.value = session.localVideoTrack.value
        _remoteVideoTrack.value = remoteTrack
        // G3-2-e2ee-active — pill reflects real SFrame keying; the
        // safety number (canonical hash of the same seed, set alongside
        // e2eeActive in setCallKey) is the out-of-band MITM code.
        _e2ee.value =
            E2eeState(
                active = session.e2eeActive.value,
                shortFingerprint = session.safetyNumber.value,
            )
        // G3-2-conn-quality — header detail from getStats sampling.
        // iter2 adds the down/up bitrate from the candidate-pair byte
        // delta; RTT + path type from iter1.
        val rtt = session.rttMs.value
        _connectionDetail.value = ConnectionDetail(
            rttMs = rtt,
            downBps = session.downBps.value,
            upBps = session.upBps.value,
            connectionType = when {
                session.connectionRelayed.value -> ConnectionDetail.ConnectionType.Relay
                rtt != null -> ConnectionDetail.ConnectionType.Direct
                else -> ConnectionDetail.ConnectionType.Unknown
            },
        )
        // ADR-0032 piece B2a — mid-call camera-on lifecycle from the 1×1
        // session, mapped onto the shared UI surface.
        _videoPublishState.value = when (session.videoPublishState.value) {
            CallSession.VideoPublishState.Off -> VideoPublishState.Off
            CallSession.VideoPublishState.Starting -> VideoPublishState.Starting
            CallSession.VideoPublishState.Live -> VideoPublishState.Live
            CallSession.VideoPublishState.Failed -> VideoPublishState.Failed
        }
    }

    /**
     * RTT-to-[QualityTier] bucketing for the per-participant quality dot.
     *
     * These thresholds are UX display bands, NOT measured network SLOs:
     * a null sample (no nominated candidate-pair yet) reads as [QualityTier.Good]
     * so a freshly-connected call does not flash a red dot before the first
     * stats poll lands.
     */
    private fun qualityTierForRtt(rttMs: Int?): QualityTier =
        when {
            rttMs == null -> QualityTier.Good
            rttMs < 100 -> QualityTier.Excellent
            rttMs < 200 -> QualityTier.Good
            rttMs < 350 -> QualityTier.Fair
            rttMs < 600 -> QualityTier.Poor
            else -> QualityTier.Bad
        }

    private fun mapSessionPhase(s: CallSession.WebCallState): CallPhase =
        when (s) {
            // `docs/ux/calls.md` section 4.1's state table separates the two
            // waits. Idle is a call prepared and about to be dialled (section
            // 3.1 made that a single frame, not a screen) and Outgoing is a
            // dialled call nobody has answered: both are `Calling...`. Only
            // once the callee has picked up and the media is being negotiated
            // is it `Connecting...`. Before this every one of them read
            // `Connecting...`, so a ringing phone claimed to be connecting.
            CallSession.WebCallState.Idle,
            CallSession.WebCallState.Outgoing,
            -> CallPhase.Calling
            CallSession.WebCallState.Ringing,
            CallSession.WebCallState.Connecting,
            -> CallPhase.Connecting
            CallSession.WebCallState.Connected,
            CallSession.WebCallState.Ending,
            -> CallPhase.Connected
            CallSession.WebCallState.Done ->
                CallPhase.Ended(endedAtMs = System.currentTimeMillis())
        }

    public fun endCall() {
        applyPhase(CallPhase.Ended(endedAtMs = System.currentTimeMillis()))
    }

    public fun reset() {
        _phase.value = CallPhase.Calling
        _connectedSinceMs.value = null
        _participants.value = emptyList()
        _pinnedId.value = null
        _participantListOpen.value = false
        _e2ee.value = E2eeState.Inactive
        _connectionDetail.value = ConnectionDetail.Unknown
        _qualityPopoverOpen.value = false
        _settingsOpen.value = false
        // Preferences (toggles + selected ids) survive a reset
        // — only the enumerated-devices snapshot is cleared.
        _settings.value = _settings.value.copy(availableDevices = emptyList())
        // Chat is per-call — wipe.
        _chatOpen.value = false
        _chatMessages.value = emptyList()
        _composeText.value = ""
        _chatUnreadCount.value = 0
        // Live media — drop the local track + cancel any
        // bound CallSession subscription so the next call
        // re-binds against a fresh session.
        bindJob?.cancel()
        bindJob = null
        _localVideoTrack.value = null
        _remoteVideoTrack.value = null
        // N-way SFU per-peer video map — wipe so the next call starts empty.
        _remoteVideoTracks.value = emptyMap()
        // ADR-0032 piece B2a — no camera-on in flight for a fresh call.
        _videoPublishState.value = VideoPublishState.Off
        // Every call starts on the front camera, so the self-view starts mirrored.
        _cameraFrontFacing.value = true
    }

    /** Mock seed — same shape as the web `seedMockParticipants` /
     *  iOS `seedMockState`. Replaced by the WebRTC pod once
     *  3a-pod-followup lands. */
    public fun seedMockState() {
        setE2ee(E2eeState(active = true, shortFingerprint = "3F7A B2C1 9E0D"))
        setConnectionDetail(
            ConnectionDetail(
                rttMs = 45,
                downBps = 720_000,
                upBps = 380_000,
                connectionType = ConnectionDetail.ConnectionType.Direct,
            ),
        )
        setHasMultipleCameras(false)
        setParticipants(
            listOf(
                Participant(
                    id = "local", displayName = "You", isLocal = true,
                    micOn = true, camOn = true, handRaised = false,
                    quality = QualityTier.Excellent, speaking = false, screenSharing = false,
                ),
                Participant(
                    id = "leaf-1", displayName = "Alice", isLocal = false,
                    micOn = true, camOn = true, handRaised = false,
                    quality = QualityTier.Good, speaking = true, screenSharing = false,
                ),
                Participant(
                    id = "leaf-2", displayName = "Bob", isLocal = false,
                    micOn = false, camOn = true, handRaised = false,
                    quality = QualityTier.Fair, speaking = false, screenSharing = false,
                ),
                Participant(
                    id = "leaf-3", displayName = "Carl", isLocal = false,
                    micOn = true, camOn = false, handRaised = true,
                    quality = QualityTier.Poor, speaking = false, screenSharing = false,
                ),
            ),
        )
        scope.launch {
            delay(400)
            setPhase(CallPhase.Connected)
        }
    }

    private fun patch(id: String, mutate: (Participant) -> Participant) {
        _participants.value = _participants.value.map { p ->
            if (p.id == id) mutate(p) else p
        }
    }
}
