// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

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
