// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch
import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.TileVideoQuality

/**
 * In-call UI state (Phase 3c6-android).
 *
 * Mirror of `apps/web/src/state/call-state.ts` + `apps/ios/.../CallUiState.swift`.
 * Same Participant shape, same CallPhase variants, same actions.
 * Compose subscribes to the [StateFlow]s via
 * `collectAsStateWithLifecycle`; mutations go through the
 * `setX` methods.
 *
 * No WebRTC here: a host feeds it (the 1x1 [CallSession] through
 * [driveFrom], the SFU session through [SfuCallUi], the link call through
 * [MultiJoinerCall]) and the call screen draws what it holds.
 */
public class CallUiState(
    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate),
    /**
     * The call's device choices when something that outlives this state holds
     * them: the one-to-one screen builds its state anew every time the call
     * comes back from the bar, and a choice made in the Settings sheet has to
     * hold for the call. `null` makes this state the owner.
     */
    devices: CallDeviceSelection? = null,
) {
    /** What the person chose in the call's Settings sheet
     *  (`docs/ux/calls.md` section 4.1): microphone, camera, speaker. */
    public val devices: CallDeviceSelection = devices ?: CallDeviceSelection()

    /** Whether [reset] clears [devices]: only the ones this state made. */
    private val ownsDevices: Boolean = devices == null

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
        val quality: QualityTier,
        /** The speaking ring, from [setSpeech]: heard on this device, never
         *  told by a server (`docs/ux/group-call-grid.md` section 7.3). */
        val speaking: Boolean,
        /** A shared screen's tile (ADR-0083), beside its sharer's own tile.
         *  Never in [participants]: the screen adds it where tiles are laid
         *  out ([withShareTile]). */
        val isScreen: Boolean = false,
        /** ADR-0084 — the person behind a remote tile once their
         *  announcement verified (base64 of the account key), so a tile
         *  without video shows their profile photo. */
        val personKeyB64: String? = null,
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

    /** The tile the person pinned to the stage, or `null`. A pin is local:
     *  nobody else learns of it (`docs/ux/group-call-grid.md` section 6.2). */
    private val _pinnedId = MutableStateFlow<String?>(null)
    public val pinnedId: StateFlow<String?> = _pinnedId.asStateFlow()

    /**
     * The group layout the person chose by hand, or `null` for the default
     * (`docs/ux/group-call-grid.md` section 3). Remembered for this call only:
     * [reset] clears it, so the next call starts from the defaults. What is
     * actually drawn is [groupLayoutKind], which a pin overrides.
     */
    private val _layoutChoice = MutableStateFlow<GroupLayoutKind?>(null)
    public val layoutChoice: StateFlow<GroupLayoutKind?> = _layoutChoice.asStateFlow()

    public fun setLayoutChoice(choice: GroupLayoutKind?) {
        _layoutChoice.value = choice
    }

    /**
     * The call's current screen share (ADR-0083), or `null`. Written by the
     * call adapters from the shared fold; on a one-to-one by [driveFrom].
     */
    private val _screenShare = MutableStateFlow<ScreenShareView?>(null)
    public val screenShare: StateFlow<ScreenShareView?> = _screenShare.asStateFlow()

    /** The layout choice from before the first share began, held while a
     *  share is live ([shareLayoutTransition]). */
    private var choiceBeforeShare: SavedChoice? = null

    /** One announcement of a share starting or stopping, numbered so the
     *  screen reads each exactly once (section 5.3). */
    public data class ShareAnnouncementEvent(val serial: Long, val announcement: ScreenShareAnnouncement)

    private val _shareAnnouncement = MutableStateFlow<ShareAnnouncementEvent?>(null)
    public val shareAnnouncement: StateFlow<ShareAnnouncementEvent?> = _shareAnnouncement.asStateFlow()

    /**
     * The current share changed. A first share switches a group call to Focus
     * and the end of the last one puts the layout back as it was before
     * (`docs/ux/screen-share.md` section 5.2); a pin on a share tile that is
     * no longer the share is cleared.
     */
    public fun setScreenShare(share: ScreenShareView?) {
        val before = _screenShare.value
        if (before == share) return
        val layout = shareLayoutTransition(before, share, _layoutChoice.value, choiceBeforeShare)
        choiceBeforeShare = layout.saved
        _layoutChoice.value = layout.choice
        val pinned = _pinnedId.value
        if (pinned != null && isScreenShareTileId(pinned) && (share == null || pinned != screenShareTileId(share.peerId))) {
            _pinnedId.value = null
        }
        _screenShare.value = share
        screenShareAnnouncement(before, share)?.let { a ->
            val serial = (_shareAnnouncement.value?.serial ?: 0L) + 1L
            _shareAnnouncement.value = ShareAnnouncementEvent(serial, a)
        }
    }

    /**
     * `Your name in this call` (`docs/ux/calls.md` section 4.1, `CallName.kt`):
     * what the Settings sheet's name field saves through, or `null` on a call
     * that cannot carry a call name (a one-to-one call). Written by the SFU
     * call adapters.
     */
    private val _callNameControl = MutableStateFlow<CallNameControl?>(null)
    public val callNameControl: StateFlow<CallNameControl?> = _callNameControl.asStateFlow()

    /** The name this device typed for the call (`""` = none). */
    private val _ownCallName = MutableStateFlow("")
    public val ownCallName: StateFlow<String> = _ownCallName.asStateFlow()

    /** What others see when no call name is set (the profile name), the
     *  placeholder of the name field; `""` when none. */
    private val _callNameDefault = MutableStateFlow("")
    public val callNameDefault: StateFlow<String> = _callNameDefault.asStateFlow()

    /** The call can carry a call name: the sheet's field saves through
     *  [control]; [own] is the name already set, [default] the profile name. */
    public fun setCallNameControl(control: CallNameControl?, own: String, default: String) {
        _ownCallName.value = own
        _callNameDefault.value = default
        _callNameControl.value = control
    }

    /** This device's name for the call changed. */
    public fun setOwnCallName(name: String) {
        _ownCallName.value = name
    }

    /**
     * The call's chat (`docs/ux/calls.md` section 4.1.4, `CallChatModel.kt`),
     * or `null` on a call that has none (a one-to-one call). Written by the
     * SFU call adapters when the call starts; its messages go when the call
     * ends for this device.
     */
    private val _callChat = MutableStateFlow<CallChatHandle?>(null)
    public val callChat: StateFlow<CallChatHandle?> = _callChat.asStateFlow()

    /** The call can carry a chat ([handle]), or no longer can (`null`): the
     *  chat that was there is cleared. */
    public fun setCallChat(handle: CallChatHandle?) {
        val before = _callChat.value
        if (before != null && before !== handle) before.model.clear()
        _callChat.value = handle
    }

    /** System back on the call screen: closes the chat panel when it is
     *  open (`docs/ux/calls.md` section 4.1.4), and says whether it did. */
    public fun closeCallChatPanel(): Boolean {
        val model = _callChat.value?.model ?: return false
        if (!model.panelOpen.value) return false
        model.closePanel()
        return true
    }

    /**
     * Who holds the floor and who held it last, as the stage of the Focus
     * layout reads it. Written by [setSpeech]; [StageSpeech.None] until a level
     * has been heard, in which case the stage falls back to the roster.
     */
    private val _speech = MutableStateFlow(StageSpeech.None)
    public val speech: StateFlow<StageSpeech> = _speech.asStateFlow()

    /**
     * Whether a call screen is showing this call's tiles. [SpeakingMonitor]
     * reads sound levels only while one is: a ring nobody can see is not worth
     * polling the connection for.
     */
    private val _speakingWatched = MutableStateFlow(false)
    public val speakingWatched: StateFlow<Boolean> = _speakingWatched.asStateFlow()

    public fun setSpeakingWatched(watched: Boolean) {
        _speakingWatched.value = watched
    }

    /**
     * Which of the two short sounds this call has already played for
     * (`docs/ux/calls.md` section 3.4). Held here, beside the call, because the
     * call screen is destroyed and rebuilt whenever the call is put away and
     * brought back, and a screen that kept this itself would play the
     * connected sound again on every return.
     */
    public val cues: CallCueTracker = CallCueTracker()

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
     *  camera…" hint. */
    private val _videoPublishState = MutableStateFlow(VideoPublishState.Off)
    public val videoPublishState: StateFlow<VideoPublishState> =
        _videoPublishState.asStateFlow()

    /** G3-2-video-ui-screen — live local-camera track from the
     *  underlying [CallSession] (when one is attached via
     *  [bindTo]). `null` while audio-only, while the prejoin
     *  flow is still up, or while the camera-pipeline init is
     *  in-flight. Compose consumers (the self-tile in the
     *  participant grid, the prejoin preview) render
     *  `LocalVideoPreviewView(track:)` when this is non-null
     *  **and** the local participant's `camOn == true`. */
    private val _localVideoTrack = MutableStateFlow<VideoTrack?>(null)
    public val localVideoTrack: StateFlow<VideoTrack?> = _localVideoTrack.asStateFlow()

    /** G3-2-remote-video — the remote participant's live video
     *  track (when one is attached via [bindTo]). `null` for
     *  audio-only, before the remote adds a camera, or after
     *  teardown. The 1×1 direct-PeerConnection path has exactly
     *  one remote track; `CallScreen` renders it on the **sole
     *  remote participant's** tile (see [soleRemoteParticipantId]);
     *  a group call uses the per-participant map below. */
    private val _remoteVideoTrack = MutableStateFlow<VideoTrack?>(null)
    public val remoteVideoTrack: StateFlow<VideoTrack?> = _remoteVideoTrack.asStateFlow()

    /** N-way SFU remote video — the per-source-peer video track map,
     *  keyed on the roster participant id (`"peer-<streamId>"`, where the
     *  MSID stream id == the source peer id per the SFU contract). The
     *  group [SfuCallUi] path populates this so `CallScreen` renders each
     *  peer's own video on its own tile; a `null` track removes the entry
     *  (peer left / camera off). The single [remoteVideoTrack] above is
     *  retained for the 1×1 P2P [driveFrom] path (a direct PeerConnection
     *  has no MSID map). */
    private val _remoteVideoTracks = MutableStateFlow<Map<String, VideoTrack>>(emptyMap())
    public val remoteVideoTracks: StateFlow<Map<String, VideoTrack>> =
        _remoteVideoTracks.asStateFlow()

    /** The light companion track of each remote peer that sends one
     *  (`LightStream.kt`, ADR-0067 step 5), keyed on the same roster
     *  participant id as [remoteVideoTracks]. A second picture of a person
     *  already in the call: an entry here never creates a participant, a tile
     *  or a camera flag, and never replaces the main track. The peer's tile
     *  decodes it beside the main track and shows it while the main one
     *  stands still. Removed with its peer and when the call ends. */
    private val _remoteLightVideoTracks = MutableStateFlow<Map<String, VideoTrack>>(emptyMap())
    public val remoteLightVideoTracks: StateFlow<Map<String, VideoTrack>> =
        _remoteLightVideoTracks.asStateFlow()

    /** The middle track of each remote peer that sends one (`LightStream.kt`,
     *  ADR-0070 step 5), keyed like [remoteVideoTracks]. A second picture of a
     *  person already in the call, as the light companion is: a tile that
     *  asks the SFU for the middle picture puts its heavy sink on this track
     *  (`VideoWants.kt`). Removed with its peer and when the call ends. */
    private val _remoteMiddleVideoTracks = MutableStateFlow<Map<String, VideoTrack>>(emptyMap())
    public val remoteMiddleVideoTracks: StateFlow<Map<String, VideoTrack>> =
        _remoteMiddleVideoTracks.asStateFlow()

    /** What the SFU says it gives of each remote peer's video
     *  (`VideoGiven.kt`, ADR-0070 step 6), by roster participant id. A peer
     *  with no entry is given what is asked for. Empty with downlink control
     *  off, and when the call ends. */
    /** The picture of a remote shared screen (ADR-0083), keyed on its
     *  sharer's roster participant id. The share tile comes from
     *  [screenShare]; this is only its picture, which may arrive before or
     *  after the state that announces it. */
    private val _remoteScreenVideoTracks = MutableStateFlow<Map<String, VideoTrack>>(emptyMap())
    public val remoteScreenVideoTracks: StateFlow<Map<String, VideoTrack>> =
        _remoteScreenVideoTracks.asStateFlow()

    /** Set / remove the shared screen's picture of [participantId]. */
    public fun setRemoteScreenVideoTrack(track: VideoTrack?, participantId: String) {
        _remoteScreenVideoTracks.value =
            if (track == null) {
                _remoteScreenVideoTracks.value - participantId
            } else {
                _remoteScreenVideoTracks.value + (participantId to track)
            }
    }

    private val _remoteVideoGiven = MutableStateFlow<Map<String, TileVideoQuality>>(emptyMap())
    public val remoteVideoGiven: StateFlow<Map<String, TileVideoQuality>> =
        _remoteVideoGiven.asStateFlow()

    public fun setRemoteVideoGiven(given: Map<String, TileVideoQuality>) {
        if (_remoteVideoGiven.value != given) _remoteVideoGiven.value = given
    }

    /**
     * Where a tile says which picture of a person it shows (`VideoWants.kt`),
     * or `null` when the call cannot act on it (a one-to-one call, or no call).
     * Set by the SFU call's adapter and cleared when the call ends, so a tile
     * taken down after the call is over reports to nobody.
     */
    @Volatile
    private var tileVideoQualityReporter:
        ((tile: Any, participantId: String, quality: TileVideoQuality?) -> Unit)? = null

    /** What every drawn tile last reported, so a call bound after its tiles
     *  were drawn still hears of them. Main thread only. */
    private val tileVideoQualityReports = LinkedHashMap<Any, Pair<String, TileVideoQuality>>()

    /**
     * Bind (or, with `null`, unbind) the call that takes the tiles' reports.
     * A newly bound call is told at once what every drawn tile shows.
     */
    public fun setTileVideoQualityReporter(
        reporter: ((tile: Any, participantId: String, quality: TileVideoQuality?) -> Unit)?,
    ) {
        tileVideoQualityReporter = reporter
        if (reporter == null) return
        for ((tile, shown) in tileVideoQualityReports) reporter(tile, shown.first, shown.second)
    }

    /**
     * The tile [tile] (any object that is the same for the tile's lifetime)
     * shows [participantId] at [quality]; `null` when it stops showing a
     * picture of them. Kept for a call bound later; forwarded to the bound
     * one. Call on the main thread.
     */
    public fun reportTileVideoQuality(tile: Any, participantId: String, quality: TileVideoQuality?) {
        if (quality == null) {
            tileVideoQualityReports.remove(tile)
        } else {
            tileVideoQualityReports[tile] = participantId to quality
        }
        tileVideoQualityReporter?.invoke(tile, participantId, quality)
    }

    /** Job for an active [bindTo] subscription. Cancelled on
     *  [reset] / re-bind to a different session so the previous
     *  flow doesn't keep pushing onto the StateFlow. */
    private var bindJob: Job? = null

    // ----------------------------------------------------------
    // Derived helpers (UI calls these inline; Compose recomposes
    // on the StateFlow this peeks at).
    // ----------------------------------------------------------

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

    /** ADR-0084 — bind one tile to the person it is (their key, standard
     *  base64). No-op on an unknown id or an unchanged key. */
    public fun setParticipantPersonKey(id: String, personKeyB64: String?) {
        if (_participants.value.none { it.id == id && it.personKeyB64 != personKeyB64 }) return
        patch(id) { it.copy(personKeyB64 = personKeyB64) }
    }

    public fun participantLeft(id: String) {
        _participants.value = _participants.value.filterNot { it.id == id }
        // Section 6.2: if the pinned person leaves, the pin is cleared and the
        // stage goes back to the speaker; the layout stays as it is.
        if (_pinnedId.value == id) _pinnedId.value = null
        // A light companion and a middle track go with their peer, and so
        // does what the SFU said it gives of them.
        if (id in _remoteLightVideoTracks.value) {
            _remoteLightVideoTracks.value = _remoteLightVideoTracks.value - id
        }
        if (id in _remoteMiddleVideoTracks.value) {
            _remoteMiddleVideoTracks.value = _remoteMiddleVideoTracks.value - id
        }
        if (id in _remoteScreenVideoTracks.value) {
            _remoteScreenVideoTracks.value = _remoteScreenVideoTracks.value - id
        }
        if (id in _remoteVideoGiven.value) {
            _remoteVideoGiven.value = _remoteVideoGiven.value - id
        }
    }

    public fun setQuality(id: String, tier: QualityTier) {
        patch(id) { it.copy(quality = tier) }
    }

    /**
     * What the sound levels say ([SpeakingRule]): whose ring is on, who
     * holds the floor and who held it last. Several people can be speaking at
     * once; only one holds the floor.
     *
     * Writes only what changed, so a reading that says the same as the last one
     * does not re-emit the roster and recompose every tile.
     */
    public fun setSpeech(speakingIds: Set<String>, next: StageSpeech) {
        val current = _participants.value
        if (current.any { it.speaking != (it.id in speakingIds) }) {
            _participants.value = current.map { p ->
                val on = p.id in speakingIds
                if (p.speaking == on) p else p.copy(speaking = on)
            }
        }
        if (_speech.value != next) _speech.value = next
    }

    public fun setMic(on: Boolean) {
        val local = _participants.value.firstOrNull { it.isLocal } ?: return
        patch(local.id) { it.copy(micOn = on) }
    }

    public fun setCamera(on: Boolean) {
        val local = _participants.value.firstOrNull { it.isLocal } ?: return
        patch(local.id) { it.copy(camOn = on) }
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
     * Set / remove a remote peer's light companion track, keyed on the peer's
     * roster participant id. A `null` [track] removes the entry. Touches
     * nothing else: not the roster, not a camera flag, not the main track.
     */
    public fun setRemoteLightVideoTrack(track: VideoTrack?, participantId: String) {
        _remoteLightVideoTracks.value =
            if (track == null) {
                _remoteLightVideoTracks.value - participantId
            } else {
                _remoteLightVideoTracks.value + (participantId to track)
            }
    }

    /**
     * Set / remove a remote peer's middle track, keyed on the peer's roster
     * participant id. A `null` [track] removes the entry. Touches nothing
     * else: not the roster, not a camera flag, not the main track.
     */
    public fun setRemoteMiddleVideoTrack(track: VideoTrack?, participantId: String) {
        _remoteMiddleVideoTracks.value =
            if (track == null) {
                _remoteMiddleVideoTracks.value - participantId
            } else {
                _remoteMiddleVideoTracks.value + (participantId to track)
            }
    }

    /**
     * The call ended: drop every light companion and middle track and what
     * the SFU said it gives, and take no further tile reports.
     */
    public fun clearRemoteCopyVideoTracks() {
        _remoteLightVideoTracks.value = emptyMap()
        _remoteMiddleVideoTracks.value = emptyMap()
        _remoteScreenVideoTracks.value = emptyMap()
        _remoteVideoGiven.value = emptyMap()
        tileVideoQualityReporter = null
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
     * (G3-2-video-ui-screen + G3-2-remote-video) and nothing else;
     * [driveFrom] is the full binding.
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
     * connection-quality detail, and the mic control writing back
     * into the session. `CallScreen` calls this when a session is
     * injected.
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
            launch { session.screenShare.collect { rebuildFromSession(session) } }
            launch { session.remoteScreenTrack.collect { rebuildFromSession(session) } }
        }
    }

    /** ADR-0048 — the name [driveFrom] gave the 1x1 remote participant. */
    private var remoteName: String = ""

    /**
     * Take the session's present state once, without subscribing.
     *
     * For a state built for a screen that is coming BACK to a running call
     * (`docs/ux/calls.md` section 4.5): its first frame then shows the call as
     * it is, instead of one frame of `Calling...` before [driveFrom]'s
     * collectors catch up. That frame was not only a flicker: the screen read
     * it as the call changing phase and played the connected sound again on
     * every return.
     */
    public fun syncFrom(session: CallSession, remoteName: String = "") {
        this.remoteName = remoteName
        rebuildFromSession(session)
    }

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
                quality = tier,
                speaking = false,
            ),
        )
        if (connected || remoteTrack != null) {
            roster.add(
                Participant(
                    id = ONE_TO_ONE_REMOTE_ID,
                    displayName = remoteName,
                    isLocal = false,
                    micOn = true,
                    camOn = remoteTrack != null,
                    quality = QualityTier.Good,
                    speaking = false,
                ),
            )
        }
        _participants.value = roster
        _cameraFrontFacing.value = session.cameraIsFrontFacing.value
        _localVideoTrack.value = session.localVideoTrack.value
        _remoteVideoTrack.value = remoteTrack
        // ADR-0083 section 4 — the other person's share, its picture keyed on
        // their roster id.
        setRemoteScreenVideoTrack(session.remoteScreenTrack.value, ONE_TO_ONE_REMOTE_ID)
        setScreenShare(session.screenShare.value)
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
        // The layout choice and what was heard are per-call.
        _layoutChoice.value = null
        _speech.value = StageSpeech.None
        cues.reset()
        if (ownsDevices) devices.reset()
        _participantListOpen.value = false
        _e2ee.value = E2eeState.Inactive
        _connectionDetail.value = ConnectionDetail.Unknown
        _qualityPopoverOpen.value = false
        // Live media — drop the local track + cancel any
        // bound CallSession subscription so the next call
        // re-binds against a fresh session.
        bindJob?.cancel()
        bindJob = null
        _localVideoTrack.value = null
        _remoteVideoTrack.value = null
        // N-way SFU per-peer video map — wipe so the next call starts empty.
        _remoteVideoTracks.value = emptyMap()
        _remoteLightVideoTracks.value = emptyMap()
        _remoteMiddleVideoTracks.value = emptyMap()
        _remoteScreenVideoTracks.value = emptyMap()
        _remoteVideoGiven.value = emptyMap()
        tileVideoQualityReporter = null
        tileVideoQualityReports.clear()
        // A name typed for the call is per call (remembered by call id
        // elsewhere, `RememberedCallNames`).
        _callNameControl.value = null
        _ownCallName.value = ""
        _callNameDefault.value = ""
        // The call's chat is per call: nothing is kept after it.
        setCallChat(null)
        // A share is per call.
        _screenShare.value = null
        choiceBeforeShare = null
        _shareAnnouncement.value = null
        // ADR-0032 piece B2a — no camera-on in flight for a fresh call.
        _videoPublishState.value = VideoPublishState.Off
        // Every call starts on the front camera, so the self-view starts mirrored.
        _cameraFrontFacing.value = true
    }

    public companion object {
        /** The roster id of the other person on a one-to-one ([driveFrom]). */
        public const val ONE_TO_ONE_REMOTE_ID: String = "remote"
    }

    private fun patch(id: String, mutate: (Participant) -> Participant) {
        _participants.value = _participants.value.map { p ->
            if (p.id == id) mutate(p) else p
        }
    }
}
