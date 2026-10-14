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
 * It is also sent as up to three ordinary senders (ADR-0070 steps 1 and 5):
 * the main one; a light one that carries the same camera scaled to a quarter
 * on a transceiver of its own, which every viewer receives and shows while
 * the picture it watches stands still; and a middle one at half size, sent
 * only from three people in the call on, which a viewer's grid tile watches
 * in place of the main one. No simulcast: which of them is sent is this
 * client's and the SFU's decision, never a browser's layer allocator's.
 *
 * Microphone tracks are added as before.
 *
 * Every write to a sender's parameters made here goes through the sender's
 * one queue (`sender-parameters.ts`), so it never overlaps the writes of
 * `sender-activity.ts` (the encoding's `active` flag) and `publish-budget.ts`
 * (its bitrate ceiling) on the same sender.
 */

import { queueParameterWrite } from "./sender-parameters";

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
export function preferCameraFramerate(sender: RTCRtpSender): Promise<void> {
  return queueParameterWrite(sender, async () => {
    try {
      const params = sender.getParameters();
      if (params.degradationPreference === CAMERA_DEGRADATION_PREFERENCE) return;
      params.degradationPreference = CAMERA_DEGRADATION_PREFERENCE;
      await sender.setParameters(params);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("camera sender: frame-rate preference refused", err);
    }
  });
}

/** The one encoding of the main camera sender. */
export function cameraSingleEncoding(): RTCRtpEncodingParameters[] {
  return [{ scalabilityMode: CAMERA_SCALABILITY_MODE } as LayeredEncoding];
}

/**
 * Add the camera to `pc` as the main sender of the first publish: one
 * encoding in temporal layers on a transceiver of its own. A browser that
 * refuses the mode gets the track added plainly, with the layers and the
 * frame-rate preference asked for afterwards: the call works either way, only
 * less well under loss.
 */
export function addCameraSender(
  pc: RTCPeerConnection,
  track: MediaStreamTrack,
  stream: MediaStream,
): RTCRtpSender {
  try {
    const sender = pc.addTransceiver(track, {
      direction: "sendrecv",
      streams: [stream],
      sendEncodings: cameraSingleEncoding(),
    }).sender;
    void preferCameraFramerate(sender);
    return sender;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("camera sender: temporal layers refused, trying less", err);
  }
  const sender = pc.addTrack(track, stream);
  void requestCameraLayers(sender);
  return sender;
}

/**
 * Add the camera to `pc` as the main sender when it is turned on mid-call.
 * `addTrack`, not a new transceiver: mid-call it may reuse a video
 * transceiver the SFU already negotiated. `addTrack` takes no encodings, so
 * the layers and the frame-rate preference are asked for afterwards.
 */
export function addCameraSenderMidCall(
  pc: RTCPeerConnection,
  track: MediaStreamTrack,
  stream: MediaStream,
): RTCRtpSender {
  const sender = pc.addTrack(track, stream);
  void requestCameraLayers(sender);
  return sender;
}

/** The light sender's picture is the camera's scaled down by this, each side. */
export const CAMERA_LIGHT_SCALE_DOWN = 4;

/** The light sender's bitrate ceiling, bits a second. */
export const CAMERA_LIGHT_MAX_BITRATE = 150_000;

/** The middle sender's picture is the camera's scaled down by this, each side. */
export const CAMERA_MIDDLE_SCALE_DOWN = 2;

/** The middle sender's bitrate ceiling, bits a second. */
export const CAMERA_MIDDLE_MAX_BITRATE = 500_000;

/**
 * The copies of the camera that are sent beside the main sender, each an
 * ordinary sender on a transceiver of its own (ADR-0070 steps 1 and 5): the
 * `light` one (a quarter of the size, at most 150 kbit/s) and the `middle`
 * one (half the size, at most 500 kbit/s).
 */
export type CameraCopyKind = "light" | "middle";

/** Every copy, in the order they are added (so the light one keeps the
 *  media line it had before there was a middle one). */
export const CAMERA_COPY_KINDS: readonly CameraCopyKind[] = ["light", "middle"];

const CAMERA_COPIES: Record<CameraCopyKind, { scaleDown: number; maxBitrate: number; ordinal: string }> = {
  light: { scaleDown: CAMERA_LIGHT_SCALE_DOWN, maxBitrate: CAMERA_LIGHT_MAX_BITRATE, ordinal: "second" },
  middle: { scaleDown: CAMERA_MIDDLE_SCALE_DOWN, maxBitrate: CAMERA_MIDDLE_MAX_BITRATE, ordinal: "third" },
};

/**
 * The one encoding of a copy's sender. No `rid`: this is an ordinary sender,
 * not a simulcast layer. `active` is stated only where the caller decides it
 * (the middle sender, which sends only from three people on); left out, the
 * encoding is active, as every encoding is by default.
 */
export function cameraCopyEncoding(kind: CameraCopyKind, active?: boolean): RTCRtpEncodingParameters {
  const spec = CAMERA_COPIES[kind];
  const encoding: RTCRtpEncodingParameters = {
    scaleResolutionDownBy: spec.scaleDown,
    maxBitrate: spec.maxBitrate,
  };
  if (active !== undefined) encoding.active = active;
  return encoding;
}

/**
 * The light sender's one encoding. A second of heavy uplink loss cannot be
 * repaired inside a 720p stream; it can inside one a sixteenth of the size,
 * so the light sender is what a viewer's tile shows until the main one plays
 * steadily again.
 */
export function cameraLightEncoding(): RTCRtpEncodingParameters {
  return cameraCopyEncoding("light");
}

/**
 * The middle sender's one encoding (ADR-0070 step 5): what a grid tile of a
 * call of three or more shows. `active` says whether it is sent now.
 */
export function cameraMiddleEncoding(active: boolean): RTCRtpEncodingParameters {
  return cameraCopyEncoding("middle", active);
}

/** A copy of the camera: a clone of the camera track on a transceiver of its
 *  own. */
export interface CopySender {
  readonly transceiver: RTCRtpTransceiver;
  readonly sender: RTCRtpSender;
  /** The clone the sender carries now. Replaced when the camera is. */
  track: MediaStreamTrack;
}

/** The camera's light sender. */
export type LightSender = CopySender;

/** What {@link addCopySender} did. */
export interface CopySenderAdded {
  readonly copy: CopySender;
  /**
   * `true` when the browser took the encoding with the transceiver. `false`
   * when it refused and the transceiver was added plainly: the caller must
   * then {@link configureCopySender} and remove the sender if that fails,
   * or the camera would be sent another time at full size.
   */
  readonly configured: boolean;
}

/** What {@link addLightSender} did. */
export interface LightSenderAdded {
  readonly light: LightSender;
  /** As {@link CopySenderAdded.configured}. */
  readonly configured: boolean;
}

/**
 * Add the `kind` copy of `cameraTrack` to `pc`. Synchronous, so the caller
 * can attach the SFrame transform in the same turn, before any offer is
 * built. Returns `null` when the browser will not add another sender at all;
 * `refused` is then told why. `active` is as in {@link cameraCopyEncoding}.
 */
export function addCopySender(
  pc: RTCPeerConnection,
  cameraTrack: MediaStreamTrack,
  kind: CameraCopyKind,
  refused: (what: string, err: unknown) => void = () => {},
  active?: boolean,
): CopySenderAdded | null {
  let track: MediaStreamTrack;
  try {
    track = cameraTrack.clone();
  } catch (err) {
    refused("the camera track cannot be cloned", err);
    return null;
  }
  try {
    const transceiver = pc.addTransceiver(track, {
      direction: "sendonly",
      sendEncodings: [cameraCopyEncoding(kind, active)],
    });
    void preferCameraFramerate(transceiver.sender);
    return { copy: { transceiver, sender: transceiver.sender, track }, configured: true };
  } catch {
    // Tried again below without the encoding.
  }
  try {
    const transceiver = pc.addTransceiver(track, { direction: "sendonly" });
    return { copy: { transceiver, sender: transceiver.sender, track }, configured: false };
  } catch (err) {
    track.stop();
    refused(`a ${CAMERA_COPIES[kind].ordinal} camera transceiver was refused`, err);
    return null;
  }
}

/** {@link addCopySender} for the light sender. */
export function addLightSender(
  pc: RTCPeerConnection,
  cameraTrack: MediaStreamTrack,
  refused: (what: string, err: unknown) => void = () => {},
): LightSenderAdded | null {
  const added = addCopySender(pc, cameraTrack, "light", refused);
  return added === null ? null : { light: added.copy, configured: added.configured };
}

/**
 * Give an already-added copy's sender its size, its bitrate ceiling and the
 * frame-rate preference with `setParameters` (the second way, for a browser
 * that refused them with the transceiver). Resolves `false` when the browser
 * refuses this too, or when the sender has no encoding to set them on.
 */
export function configureCopySender(
  sender: RTCRtpSender,
  kind: CameraCopyKind,
  active?: boolean,
): Promise<boolean> {
  return queueParameterWrite(sender, async () => {
    try {
      const params = sender.getParameters();
      const first = params.encodings[0];
      if (first === undefined) return false;
      params.encodings[0] = { ...first, ...cameraCopyEncoding(kind, active) };
      params.degradationPreference = CAMERA_DEGRADATION_PREFERENCE;
      await sender.setParameters(params);
      return true;
    } catch {
      return false;
    }
  });
}

/** {@link configureCopySender} for the light sender. */
export function configureLightSender(sender: RTCRtpSender): Promise<boolean> {
  return configureCopySender(sender, "light");
}

/**
 * Start or stop what a copy's sender sends, with `setParameters` on its one
 * encoding: no renegotiation, the media line stays. Resolves `false` when the
 * browser refuses or the sender has no encoding; `true` also when the
 * encoding already says so.
 */
export function setCopySenderActive(sender: RTCRtpSender, active: boolean): Promise<boolean> {
  return queueParameterWrite(sender, async () => {
    try {
      const params = sender.getParameters();
      const first = params.encodings[0];
      if (first === undefined) return false;
      // An encoding that does not say is active.
      if ((first.active ?? true) === active) return true;
      params.encodings[0] = { ...first, active };
      await sender.setParameters(params);
      return true;
    } catch {
      return false;
    }
  });
}

/**
 * Take a copy's sender off `pc` and stop its clone. Every step is best effort
 * and the clone is stopped whatever the browser does with the rest, so a
 * sender that could not be removed sends nothing.
 */
export function removeCopySender(pc: RTCPeerConnection, copy: CopySender): void {
  try {
    copy.track.stop();
  } catch {
    // best-effort
  }
  try {
    pc.removeTrack(copy.sender);
  } catch {
    // best-effort
  }
  try {
    copy.transceiver.stop();
  } catch {
    // best-effort
  }
}

/** {@link removeCopySender} for the light sender. */
export function removeLightSender(pc: RTCPeerConnection, light: LightSender): void {
  removeCopySender(pc, light);
}

/**
 * Add `track` to `pc` the way `addTrack(track, stream)` would, with the camera
 * as the main sender {@link addCameraSender} describes. Microphone tracks are
 * added as before. The camera's copies are added separately
 * ({@link addCopySender}).
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
 * sender came from `addTrack`, which takes no encodings (the fallback of
 * {@link addCameraSender}, and {@link addCameraSenderMidCall}). Best effort: a
 * browser that refuses keeps one layer, and the media counters log which
 * mode the sender actually uses.
 */
export function requestCameraLayers(sender: RTCRtpSender): Promise<void> {
  return queueParameterWrite(sender, async () => {
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
  });
}
