// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.webrtc.VideoTrack

/**
 * The light companion of a remote camera (ADR-0067 step 5,
 * `services/sfu/README.md`, "Two-track delivery").
 *
 * For a publisher that sends two simulcast layers the SFU gives each
 * subscriber two video tracks: the **main** one, whose MSID stream id is the
 * source peer id (as for every remote stream), and a **light companion** that
 * carries only the light layer, whose MSID stream id is the source peer id
 * followed by [LIGHT_STREAM_SUFFIX]. A publisher without simulcast has no
 * companion.
 *
 * The companion is a second picture of a person already in the call. It is
 * never a participant of its own: no tile, no camera flag. The tile of its
 * peer decodes it beside the main track and shows it while the main one
 * stands still (`LayerSwitch.kt`). The twin of the web `light-stream.ts`.
 */

/** What the SFU appends to a peer id to name that peer's light companion. */
internal const val LIGHT_STREAM_SUFFIX: String = "~light"

/** A remote MSID stream id, read. */
internal data class RemoteStreamId(
    /** The source peer. */
    val peerId: String,
    /** Whether the stream is that peer's light companion. */
    val light: Boolean,
)

/**
 * Split a remote stream id into the peer it belongs to and whether it is the
 * light companion. The bare suffix (no peer id before it) names no peer and
 * is read as an ordinary stream id.
 */
internal fun splitStreamId(streamId: String): RemoteStreamId =
    if (streamId.length > LIGHT_STREAM_SUFFIX.length && streamId.endsWith(LIGHT_STREAM_SUFFIX)) {
        RemoteStreamId(peerId = streamId.dropLast(LIGHT_STREAM_SUFFIX.length), light = true)
    } else {
        RemoteStreamId(peerId = streamId, light = false)
    }

/** The light stream id of a peer (the inverse of [splitStreamId]). */
internal fun lightStreamIdOf(peerId: String): String = "$peerId$LIGHT_STREAM_SUFFIX"

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
 * A light companion goes to the companion map only. It creates no participant
 * and no tile, never replaces the main track and never touches a camera flag.
 */
internal fun applyRemoteVideoTrack(
    ui: CallUiState,
    track: VideoTrack?,
    streamId: String?,
    lastKnownCameraOn: Map<String, Boolean>,
) {
    val id = splitStreamId(streamId ?: return)
    val participantId = remoteParticipantId(id.peerId)
    if (id.light) {
        ui.setRemoteLightVideoTrack(track, participantId)
        return
    }
    ui.setRemoteVideoTrack(track, participantId)
    val on = if (track != null) lastKnownCameraOn[participantId] ?: true else false
    ui.setParticipantCamera(participantId, on)
}
