// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The group call's senders (ADR-0067 step 2).
 *
 * A camera is sent with three temporal layers: two frames in three are
 * referenced by no later base-layer frame, so one lost in a burst of packet
 * loss costs that frame rather than the picture until a keyframe. The SFU
 * forwards packets unchanged and relays the frame descriptor that tells the
 * receiver which frames depend on which.
 *
 * It is also sent twice, as two simulcast layers: the full picture and a
 * light one. The SFU forwards both to every viewer, and the viewer shows the
 * light one while the full one stands still.
 *
 * Microphone tracks are added as before.
 */

/** The camera's encoding: one spatial layer, three temporal layers. */
export const CAMERA_SCALABILITY_MODE = "L1T3";

/** `RTCRtpEncodingParameters` with the WebRTC-SVC field, which the DOM types
 *  this project builds against do not declare yet. */
type LayeredEncoding = RTCRtpEncodingParameters & { scalabilityMode?: string };

/**
 * What the camera gives up when the bitrate falls short: sharpness, not
 * motion. A browser's default for a camera trades frame rate as well, and on
 * live calls a sender whose bitrate estimate had collapsed after a second of
 * packet loss sent 1 to 10 frames a second at 480p-540p for several seconds,
 * which a viewer sees as a frozen picture. With this the picture turns soft
 * and keeps moving.
 */
export const CAMERA_DEGRADATION_PREFERENCE: RTCDegradationPreference = "maintain-framerate";

/**
 * Tell a camera sender to keep its frame rate under a bitrate shortfall.
 * Best effort: a browser that refuses keeps its default.
 */
export async function preferCameraFramerate(sender: RTCRtpSender): Promise<void> {
  try {
    const params = sender.getParameters();
    if (params.degradationPreference === CAMERA_DEGRADATION_PREFERENCE) return;
    params.degradationPreference = CAMERA_DEGRADATION_PREFERENCE;
    await sender.setParameters(params);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("camera sender: frame-rate preference refused", err);
  }
}

/**
 * The light simulcast layer's RTP stream id. The SFU forwards this layer to
 * every viewer as the publisher's light companion track, and matches it by
 * this exact name.
 */
export const CAMERA_RID_LOW = "q";

/** The full-resolution simulcast layer's RTP stream id. */
export const CAMERA_RID_FULL = "f";

/** The light layer is the camera picture scaled down by this, each side. */
export const CAMERA_LOW_SCALE_DOWN = 4;

/** The light layer's bitrate ceiling, bits a second. */
export const CAMERA_LOW_MAX_BITRATE = 150_000;

/**
 * The camera's two simulcast encodings, light layer first. A second of heavy
 * uplink loss cannot be repaired inside a 720p stream; it can inside one a
 * sixteenth of the size, so the light layer is what a viewer's tile shows
 * until the full one plays steadily again. `layered` asks for temporal layers on both.
 */
export function cameraSimulcastEncodings(layered = true): RTCRtpEncodingParameters[] {
  const mode = layered ? { scalabilityMode: CAMERA_SCALABILITY_MODE } : {};
  const low: LayeredEncoding = {
    rid: CAMERA_RID_LOW,
    scaleResolutionDownBy: CAMERA_LOW_SCALE_DOWN,
    maxBitrate: CAMERA_LOW_MAX_BITRATE,
    ...mode,
  };
  const full: LayeredEncoding = { rid: CAMERA_RID_FULL, ...mode };
  return [low, full];
}

/** The one encoding sent when a browser refuses simulcast. */
export function cameraSingleEncoding(): RTCRtpEncodingParameters[] {
  return [{ scalabilityMode: CAMERA_SCALABILITY_MODE } as LayeredEncoding];
}

/**
 * Whether the camera is sent as two simulcast layers. On: the SFU sends each
 * viewer both layers as two tracks, and the viewer's tile shows the light one
 * the moment the full one stands still (`light-stream.ts`,
 * `layer-display.ts`). Switching one track to the light layer on the SFU was
 * measured (2026-10-04, `services/sfu/README.md`, "Uplink fallback") and did
 * not shorten a viewer's wait through a loss burst, because that switch needs
 * the light layer's key frame and that crosses the same lossy uplink; a
 * companion track the viewer already decodes needs none. The light layer
 * costs a publisher about 150 kbit/s.
 */
export const CAMERA_SIMULCAST_ENABLED = true;

/**
 * Add the camera to `pc` as a new sender. Encodings can only be given when
 * the transceiver is created, so both the first publish and the mid-call
 * camera-on come through here. Tried in order, each step logged when the
 * browser refuses it:
 *
 *   1. two simulcast layers, each in temporal layers;
 *   2. two simulcast layers, the browser's default temporal structure;
 *   3. one encoding in temporal layers;
 *   4. the track added plainly, the layers asked for afterwards.
 *
 * The call works at every step, only less well under loss.
 */
export function addCameraSender(
  pc: RTCPeerConnection,
  track: MediaStreamTrack,
  stream: MediaStream,
  simulcast: boolean = CAMERA_SIMULCAST_ENABLED,
): RTCRtpSender {
  const attempts: readonly (readonly [string, RTCRtpEncodingParameters[]])[] = [
    ...(simulcast
      ? ([
          ["simulcast with temporal layers", cameraSimulcastEncodings(true)],
          ["simulcast", cameraSimulcastEncodings(false)],
        ] as const)
      : []),
    ["temporal layers", cameraSingleEncoding()],
  ];
  for (const [what, sendEncodings] of attempts) {
    try {
      const sender = pc.addTransceiver(track, {
        direction: "sendrecv",
        streams: [stream],
        sendEncodings,
      }).sender;
      void preferCameraFramerate(sender);
      return sender;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`camera sender: ${what} refused, trying less`, err);
    }
  }
  const sender = pc.addTrack(track, stream);
  void requestCameraLayers(sender);
  return sender;
}

/**
 * Add `track` to `pc` the way `addTrack(track, stream)` would, with the camera
 * sent as {@link addCameraSender} describes. Microphone tracks are added as
 * before.
 */
export function addGroupCallSender(
  pc: RTCPeerConnection,
  track: MediaStreamTrack,
  stream: MediaStream,
): RTCRtpSender {
  if (track.kind !== "video") return pc.addTrack(track, stream);
  return addCameraSender(pc, track, stream);
}

/**
 * Ask an already-added camera sender for temporal layers and for the
 * frame-rate preference. Used where the
 * sender came from `addTrack`, which takes no encodings (the last fallback
 * of {@link addCameraSender}). Best effort: a
 * browser that refuses keeps one layer, and the media counters log which
 * mode the sender actually uses.
 */
export async function requestCameraLayers(sender: RTCRtpSender): Promise<void> {
  try {
    const params = sender.getParameters();
    const first = params.encodings[0] as LayeredEncoding | undefined;
    const layersSet = first === undefined || first.scalabilityMode === CAMERA_SCALABILITY_MODE;
    const framerateSet = params.degradationPreference === CAMERA_DEGRADATION_PREFERENCE;
    if (layersSet && framerateSet) return;
    if (!layersSet && first !== undefined) {
      const layered: LayeredEncoding = { ...first, scalabilityMode: CAMERA_SCALABILITY_MODE };
      params.encodings[0] = layered;
    }
    params.degradationPreference = CAMERA_DEGRADATION_PREFERENCE;
    await sender.setParameters(params);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("camera sender: temporal layers refused, sending one layer", err);
  }
}
