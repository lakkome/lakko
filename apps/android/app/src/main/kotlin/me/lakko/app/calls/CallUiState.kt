// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
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
        public data object Prejoin : CallPhase()
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
    ) {
        public enum class Kind { Text, System }
    }

    // ----------------------------------------------------------
    // State flows
    // ----------------------------------------------------------

    private val _phase = MutableStateFlow<CallPhase>(CallPhase.Prejoin)
    public val phase: StateFlow<CallPhase> = _phase.asStateFlow()

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

    /** Active-speaker pick: pinned > speaking-remote > first
     *  remote > local. */
    public fun activeSpeaker(): Participant? {
        val list = _participants.value
        val pin = _pinnedId.value
        if (pin != null) {
            list.firstOrNull { it.id == pin }?.let { return it }
        }
        list.firstOrNull { it.speaking && !it.isLocal }?.let { return it }
        list.firstOrNull { !it.isLocal }?.let { return it }
        return list.firstOrNull()
    }

    /** Gallery = everyone except the active speaker. */
    public fun galleryParticipants(): List<Participant> {
        val active = activeSpeaker() ?: return emptyList()
        return _participants.value.filter { it.id != active.id }
    }

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
        _phase.value = phase
    }

    public fun join() {
        _phase.value = CallPhase.Connecting
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
        val senderName = local?.displayName ?: "You"
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
    public fun addSystemEvent(text: String) {
        val msg = ChatMessage(
            id = nextChatId(),
            kind = ChatMessage.Kind.System,
            senderId = null,
            senderName = "",
            text = text,
            tsMs = System.currentTimeMillis(),
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

    /**
     * Attach a live [CallSession] so the UI mirrors its
     * `localVideoTrack` StateFlow. Currently bridges only the
     * local video track; the audio / participant / e2ee /
     * connection-detail fields stay on the mock path until the
     * full-session-integration sub-sprint lands (the screen
     * today reads from [seedMockState]).
     *
     * Cancels any previous bind subscription so re-attaching
     * to a different session is safe.
     */
    public fun bindTo(session: CallSession) {
        bindJob?.cancel()
        bindJob = scope.launch {
            session.localVideoTrack.collect { track ->
                _localVideoTrack.value = track
            }
        }
    }

    public fun endCall() {
        _phase.value = CallPhase.Ended(endedAtMs = System.currentTimeMillis())
    }

    public fun reset() {
        _phase.value = CallPhase.Prejoin
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
