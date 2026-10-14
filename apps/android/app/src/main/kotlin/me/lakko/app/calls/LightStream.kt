// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.webrtc.VideoTrack
import uniffi.lakko_ffi_uniffi.TileVideoQuality

/**
 * The copies of a remote camera (ADR-0070 steps 1 and 5,
 * `services/sfu/README.md`): the twin of the web `light-stream.ts`.
 *
 * A person whose camera is on sends up to three ordinary video tracks, and
 * the SFU gives each subscriber every one of them under its own MSID stream
 * id: the **main** track under the source peer id (as for every remote
 * stream), the **middle** track under that id followed by
 * [MIDDLE_STREAM_SUFFIX], and the **light companion** under that id followed
 * by [LIGHT_STREAM_SUFFIX]. A publisher from before ADR-0070 has neither
 * copy.
 *
 * A copy is a second picture of a person already in the call. It is never a
 * participant of its own: no tile, no camera flag. The tile of its peer puts
 * its heavy sink on the main or the middle track as it asks the SFU for
 * (`VideoWants.kt`), and shows the light one while that picture stands still
 * (`LayerSwitch.kt`).
 */

/** What the SFU appends to a peer id to name that peer's light companion. */
internal const val LIGHT_STREAM_SUFFIX: String = "~light"

/** What the SFU appends to a peer id to name that peer's middle track. */
internal const val MIDDLE_STREAM_SUFFIX: String = "~mid"

/** Which of a peer's streams a remote stream id names. */
internal enum class RemoteStreamRole { MAIN, MIDDLE, LIGHT }

/** A remote MSID stream id, read. */
internal data class RemoteStreamId(
    /** The source peer. */
    val peerId: String,
    /** The peer's main stream (audio and the camera as it is sent), its
     *  middle track, or its light companion. */
    val role: RemoteStreamRole,
)

/**
 * Split a remote stream id into the peer it belongs to and which of that
 * peer's streams it is. A bare suffix (no peer id before it) names no peer
 * and is read as an ordinary stream id.
 */
internal fun splitStreamId(streamId: String): RemoteStreamId {
    for ((suffix, role) in STREAM_SUFFIXES) {
        if (streamId.length > suffix.length && streamId.endsWith(suffix)) {
            return RemoteStreamId(peerId = streamId.dropLast(suffix.length), role = role)
        }
    }
    return RemoteStreamId(peerId = streamId, role = RemoteStreamRole.MAIN)
}

private val STREAM_SUFFIXES: List<Pair<String, RemoteStreamRole>> = listOf(
    LIGHT_STREAM_SUFFIX to RemoteStreamRole.LIGHT,
    MIDDLE_STREAM_SUFFIX to RemoteStreamRole.MIDDLE,
)

/** The light stream id of a peer (the inverse of [splitStreamId]). */
internal fun lightStreamIdOf(peerId: String): String = "$peerId$LIGHT_STREAM_SUFFIX"

/** The middle stream id of a peer (the inverse of [splitStreamId]). */
internal fun middleStreamIdOf(peerId: String): String = "$peerId$MIDDLE_STREAM_SUFFIX"

/** The roster id of a remote peer's tile; the same key both SFU call adapters
 *  (`SfuCallUi`, `MultiJoinerCall`) use. */
internal fun remoteParticipantId(peerId: String): String = "peer-$peerId"

/** The SFU peer id behind a roster id made by [remoteParticipantId]. */
internal fun peerIdOfParticipant(participantId: String): String = participantId.removePrefix("peer-")

/**
 * A remote video track arrived (or went, for a `null` [track]) under
 * [streamId]: what both SFU call adapters do with it.
 *
 * The main track is keyed on the source peer's roster id and sets that
 * participant's camera flag, deferring to [lastKnownCameraOn] (the join-race
 * guard: a track arrival for a peer already known to be muted keeps it off, an
 * unknown peer defaults on, a `null` track is off).
 *
 * A light companion or a middle track goes to its own map only. It creates no
 * participant and no tile, never replaces the main track and never touches a
 * camera flag.
 */
internal fun applyRemoteVideoTrack(
    ui: CallUiState,
    track: VideoTrack?,
    streamId: String?,
    lastKnownCameraOn: Map<String, Boolean>,
) {
    val id = splitStreamId(streamId ?: return)
    val participantId = remoteParticipantId(id.peerId)
    when (id.role) {
        RemoteStreamRole.LIGHT -> ui.setRemoteLightVideoTrack(track, participantId)
        RemoteStreamRole.MIDDLE -> ui.setRemoteMiddleVideoTrack(track, participantId)
        RemoteStreamRole.MAIN -> {
            ui.setRemoteVideoTrack(track, participantId)
            val on = if (track != null) lastKnownCameraOn[participantId] ?: true else false
            ui.setParticipantCamera(participantId, on)
        }
    }
}

/**
 * What the SFU says it gives of each person (`VideoGiven.kt`), by SFU peer
 * id, put where the tiles read it: by roster id.
 */
internal fun applyVideoGiven(ui: CallUiState, given: Map<String, TileVideoQuality>) {
    ui.setRemoteVideoGiven(given.mapKeys { (peerId, _) -> remoteParticipantId(peerId) })
}

/**
 * Let the tiles of [ui] tell [report] which picture of a person they show
 * (`VideoWants.kt`): the roster id a tile knows is turned into the SFU peer
 * id the call asks under.
 */
internal fun bindTileVideoQuality(
    ui: CallUiState,
    report: (tile: Any, peerId: String, quality: TileVideoQuality?) -> Unit,
) {
    ui.setTileVideoQualityReporter { tile, participantId, quality ->
        report(tile, peerIdOfParticipant(participantId), quality)
    }
}
