// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The group call's senders (ADR-0067 step 2).
 *
 * A camera is sent with three temporal layers: two frames in three are
 * referenced by no later base-layer frame, so one lost in a burst of packet
 * loss costs that frame rather than the picture until a keyframe. The SFU
 * forwards packets unchanged and relays the frame descriptor that tells the
 * receiver which frames depend on which, so nothing else changes.
 *
 * Microphone tracks are added as before.
 */

/** The camera's encoding: one spatial layer, three temporal layers. */
export const CAMERA_SCALABILITY_MODE = "L1T3";

/** `RTCRtpEncodingParameters` with the WebRTC-SVC field, which the DOM types
 *  this project builds against do not declare yet. */
type LayeredEncoding = RTCRtpEncodingParameters & { scalabilityMode?: string };

/**
 * Add `track` to `pc` the way `addTrack(track, stream)` would, with the camera
 * encoded in temporal layers. A browser that refuses the mode gets the track
 * added plainly, as before this change: the call works either way, only less
 * well under loss.
 */
export function addGroupCallSender(
  pc: RTCPeerConnection,
  track: MediaStreamTrack,
  stream: MediaStream,
): RTCRtpSender {
  if (track.kind !== "video") return pc.addTrack(track, stream);
  try {
    return pc.addTransceiver(track, {
      direction: "sendrecv",
      streams: [stream],
      sendEncodings: [{ scalabilityMode: CAMERA_SCALABILITY_MODE } as LayeredEncoding],
    }).sender;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("camera sender: temporal layers refused, sending one layer", err);
    return pc.addTrack(track, stream);
  }
}

/**
 * Ask an already-added camera sender for temporal layers. Used where the
 * sender came from `addTrack`, which takes no encodings. Best effort: a
 * browser that refuses keeps one layer, and the media counters log which
 * mode the sender actually uses.
 */
export async function requestCameraLayers(sender: RTCRtpSender): Promise<void> {
  try {
    const params = sender.getParameters();
    const first = params.encodings[0] as LayeredEncoding | undefined;
    if (first === undefined || first.scalabilityMode === CAMERA_SCALABILITY_MODE) return;
    const layered: LayeredEncoding = { ...first, scalabilityMode: CAMERA_SCALABILITY_MODE };
    params.encodings[0] = layered;
    await sender.setParameters(params);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("camera sender: temporal layers refused, sending one layer", err);
  }
}
