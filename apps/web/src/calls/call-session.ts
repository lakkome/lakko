// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * 1×1 P2P call session on the web client (Phase 3a8).
 *
 * Bridges the shared Rust state machine (`session.rs`, wired
 * through the wasm FFI as the typed signaling composer /
 * parser) and the browser-native `RTCPeerConnection`. The
 * web client OWNS the live `RTCPeerConnection` — the Rust
 * core only sees opaque SDP + ICE strings flow past, plus
 * `Event::*` raises from `oniceconnectionstatechange` etc.
 *
 * Scope:
 *
 *   * Audio + video. Both the outbound caller (`dial` with a
 *     camera) and the incoming answerer ("Accept with video")
 *     can send video; either side can also turn the camera on
 *     mid-call (`addLocalVideo`, perfect-negotiation add-track).
 *   * 1×1 only — no SFU, no SFrame insertable streams. The P2P
 *     path uses native DTLS-SRTP; SFrame layers in only when a
 *     SFU sits between the peers (the group-call path).
 *   * No screen share, no echo-cancellation tuning beyond
 *     `getUserMedia` defaults.
 *
 * Architecture:
 *
 * ```text
 *   user UI ──tap───────────────────────────────────────┐
 *                                                       ▼
 *      ┌──────────────────────────────────────────────────────┐
 *      │              CallSession (this file)                 │
 *      │                                                      │
 *      │  state machine (mirror of Rust CallSession state)    │
 *      │  RTCPeerConnection (owned + lifecycle-managed)       │
 *      │  MediaStream (mic capture)                           │
 *      │  remote MediaStream (audio playback)                 │
 *      │                                                      │
 *      └────────────┬───────────────────────────┬─────────────┘
 *                   │                           │
 *                   │ outbound bytes            │ inbound bytes
 *                   ▼                           ▲
 *      ┌─────────────────────┐  ┌─────────────────────────────┐
 *      │ signaling-publisher │  │ signaling-receiver          │
 *      │ (compose + send via │  │ (parse + driveStateMachine) │
 *      │  ChatClient.sendApp)│  │                             │
 *      └─────────────────────┘  └─────────────────────────────┘
 * ```
 *
 * Until Phase 3a2 (STUN bootstrap / TURN deployment), we
 * configure the public Google STUN servers as a fallback —
 * good enough for two NAT'd peers to find each other in
 * the canonical home-broadband case. TURN relay arrives in
 * 3a2.
 */
import { InboundCounterLog } from "./media-counters";
import {
  inboundHasGoneSilent,
  noteInboundMedia,
  type InboundActivity,
} from "./media-watchdog";
import {
  armGenericFrameDescriptor,
  type HeaderExtensionTransceiver,
} from "./frame-descriptor";
import {
  attachReceiverSFrame,
  attachSenderSFrame,
  coverEncodedStreams,
  isSFrameTransformSupported,
  SFRAME_STREAM_SCREEN_AUDIO,
  SFRAME_STREAM_SCREEN_SHARE,
} from "./sframe-transform";
import {
  ScreenShareNotSealedError,
  screenStreamPeerOf,
  withScreenStreamId,
} from "./screen-share-tracks";
import {
  sframePeerConnectionConfig,
  sframeTransport,
  sframeTransportLine,
} from "./sframe-transport";

/**
 * The three SFrame calls this session makes, injectable so a test can prove
 * that every sender and receiver is sealed without a browser.
 *
 * It is a seam and not a switch: the default is the real transform and there
 * is no "off". A caller that supplies its own is a test.
 */
export interface SFrameAttachers {
  readonly supported: () => boolean;
  readonly attachSender: (
    sender: RTCRtpSender,
    opts: {
      readonly seed: Uint8Array;
      readonly localLeaf: number;
      readonly kind?: "audio" | "video";
      /** Stated for a sender that is not the microphone or the camera (a
       *  screen share, ADR-0083); otherwise the kind decides. */
      readonly streamIndex?: number;
    },
  ) => void;
  readonly attachReceiver: (
    receiver: RTCRtpReceiver,
    opts: { readonly seed: Uint8Array },
  ) => void;
}

/** The real transforms (`sframe-transform.ts`). */
export const DEFAULT_SFRAME_ATTACHERS: SFrameAttachers = {
  supported: isSFrameTransformSupported,
  attachSender: (sender, opts) => {
    attachSenderSFrame(sender, opts);
  },
  attachReceiver: (receiver, opts) => {
    attachReceiverSFrame(receiver, opts);
  },
};
import {
  preferVp8,
  videoReceiveCapabilities,
  type CodecPreferenceTransceiver,
} from "./video-codec-preference";
import { CallPathLog } from "./call-path";
import { iceServersFor, iceServersLine } from "./ice-servers";
import { loadWasm } from "../wasm";
import type { CallSignalingWake } from "./call-signaling-wake";
import type { ParsedCallSignaling } from "../../wasm/lakko_ffi_wasm";

/** Lifecycle phases this session can be in. Mirror of
 *  `lakko_calls::CallState` minus the `Ended` terminal,
 *  which we squash into `done`. */
export type WebCallState =
  | "idle"
  | "outgoing"
  | "ringing"
  | "connecting"
  | "connected"
  | "ending"
  | "done";

/** Reason the call ended. Mirror of `lakko_calls::EndReason`. */
export type WebCallEndReason =
  | "hungUp"
  | "busy"
  | "declined"
  | "timedOut"
  | "error";

/**
 * The reason a one-to-one call ended, from the reason on the wire. A
 * one-to-one call has five; anything else (`endedForAll` belongs to group and
 * link calls, `docs/ux/calls.md` section 4.1.2) reads as a plain hang-up, the
 * way the core folds a reason it does not know.
 */
export function oneToOneEndReason(wire: string): WebCallEndReason {
  switch (wire) {
    case "busy":
    case "declined":
    case "timedOut":
    case "error":
      return wire;
    default:
      return "hungUp";
  }
}

/**
 * Lifecycle of a mid-call camera-on (piece B). A call started audio-only has
 * no video sender; turning the camera on ACQUIRES the camera and renegotiates
 * (adds a track after the initial offer):
 *   * `off`      — no video published (the default; also after a failure).
 *   * `starting` — `getUserMedia` + the addTrack/renegotiation are in flight.
 *   * `live`     — the local video track is captured + attached (self-preview
 *                  renders; the SDP renegotiation continues in the background).
 *   * `failed`   — permission denied / no camera / capture error; the caller
 *                  reverts the toggle + surfaces a message. The call stays up
 *                  audio-only.
 *
 * Defined here as the canonical home so the 1×1 (`CallSession`) and SFU
 * (`SfuCallSession`, which re-exports this) paths share ONE type + one
 * videoPublishState surface driving one spinner. */
export type VideoPublishState = "off" | "starting" | "live" | "failed";

/**
 * G3-3-conn-quality — live connection-quality sample derived
 * from the active ICE candidate-pair (the web mirror of the
 * iOS/Android `ConnectionStatsSnapshot`).
 *
 * `rttMs` is `null` until WebRTC has a usable round-trip
 * estimate; `downBps` / `upBps` are `null` until a second poll
 * sample lands (a single sample carries no rate) and are in
 * **bits per second**. `relayed` is `true` when either end of
 * the selected path is a TURN relay candidate.
 */
export interface WebConnectionStats {
  readonly rttMs: number | null;
  readonly relayed: boolean;
  readonly downBps: number | null;
  readonly upBps: number | null;
}

/** Read a numeric field off an opaque `RTCStats` entry. The
 *  DOM types these as `any`; we narrow from `unknown` so the
 *  parse stays type-safe. */
function statNumber(stat: unknown, key: string): number | null {
  if (typeof stat !== "object" || stat === null) return null;
  const v = (stat as Record<string, unknown>)[key];
  return typeof v === "number" ? v : null;
}

/** Read a string field off an opaque `RTCStats` entry. */
function statString(stat: unknown, key: string): string | null {
  if (typeof stat !== "object" || stat === null) return null;
  const v = (stat as Record<string, unknown>)[key];
  return typeof v === "string" ? v : null;
}

/** Callback set the caller wires in. Each fires on a
 *  state-machine transition. */
export interface CallSessionCallbacks {
  /** Audio + video tracks the platform-native PeerConnection
   *  produces for the remote side. UI binds them to
   *  `<audio>` / `<video>` elements. */
  onRemoteStream(stream: MediaStream): void;
  /** The local capture stream (mic + camera). Fired once after
   *  `getUserMedia` succeeds, so the UI can bind the local video
   *  track to a self-preview `<video>`. Optional — an audio-only
   *  caller can omit it. */
  onLocalStream?(stream: MediaStream): void;
  /** Outbound signaling bytes to PUT to the chat group's DS
   *  journal. The composer wraps in MLS application
   *  message; transport stays on the existing Phase 1
   *  pipeline.
   *
   *  `wake` is whether this frame rings someone or stops a ring: `"call"`
   *  only for the dial offer and for the End of a call that never connected
   *  on this device. A renegotiation offer and the End of a connected call
   *  are `"none"`: their tags match the dial and the cancel, and a call wake
   *  on them rang the peer's iPhone (a VoIP push is always reported to
   *  CallKit as an incoming call). See `call-signaling-wake.ts`. */
  onOutboundSignaling(plaintextBytes: Uint8Array, wake: CallSignalingWake): void;
  /** State transition tick. UI re-renders. */
  onStateChange(state: WebCallState, reason?: WebCallEndReason): void;
  /** G3-3-conn-quality — periodic connection-quality tick while
   *  the call is connected (every 2 s). Optional: callers that
   *  render no quality indicator can omit it and skip the poll. */
  onConnectionStats?(stats: WebConnectionStats): void;
  /** Mid-call camera-on lifecycle (piece B). Drives the camera
   *  button's spinner + the fail-safe revert in the UI. Optional —
   *  an audio-only caller that renders no camera feedback can omit
   *  it. Fires on every transition (`off` → `starting` → `live` /
   *  `failed`, and back to `off` at teardown). */
  onVideoPublishState?(state: VideoPublishState): void;
  /** The other person's screen share arrived (ADR-0083): a stream whose id
   *  ends in `~screen`, never the camera stream `onRemoteStream` names.
   *  Optional: a caller that shows no share omits it. */
  onRemoteScreenStream?(stream: MediaStream, track: MediaStreamTrack): void;
}

/** Audio / video request flags. Phase 3a8 MVP is audio-only
 *  on both sides; the field is here for forward-compat. */
export interface CallMedia {
  readonly audio: boolean;
  readonly video: boolean;
  /** Preferred microphone `deviceId` (from the in-call device picker).
   *  `null`/absent → the OS default. Honoured at initial capture; a mid-call
   *  change goes through {@link CallSession.switchAudioInput}. */
  readonly audioDeviceId?: string | null;
  /** Preferred camera `deviceId`. `null`/absent → the OS default. */
  readonly videoDeviceId?: string | null;
}

/** Build a `getUserMedia` per-kind constraint: `false` when the kind is
 *  disabled, an `{ deviceId: { exact } }` object when a specific device is
 *  picked, else `true` (OS default). Shared with the SFU session. */
export function trackConstraint(
  enabled: boolean,
  deviceId: string | null | undefined,
): boolean | MediaTrackConstraints {
  if (!enabled) return false;
  if (deviceId != null && deviceId.length > 0) {
    return { ...CAMERA_CAPTURE_SIZE, deviceId: { exact: deviceId } };
  }
  return { ...CAMERA_CAPTURE_SIZE };
}

/**
 * The picture the camera is asked for: 1280x720 at 30 frames a second, the
 * size iOS and Android already capture at. Without it Chrome opens the camera
 * at 640x480. `ideal`, never `exact`: a camera that cannot do 720p still
 * opens, at the closest size it has.
 */
export const CAMERA_CAPTURE_SIZE: MediaTrackConstraints = {
  width: { ideal: 1280 },
  height: { ideal: 720 },
  frameRate: { ideal: 30 },
};

/** The microphone constraint: the picked device (else the OS default) with
 *  echo cancellation, noise suppression and gain control asked for by name.
 *  Browsers default all three on, and asking explicitly keeps that so on one
 *  that does not; each device cancels only what it played itself, so two
 *  devices in one room still hear each other through the air. */
export function audioTrackConstraint(
  enabled: boolean,
  deviceId: string | null | undefined,
): boolean | MediaTrackConstraints {
  if (!enabled) return false;
  const processing: MediaTrackConstraints = {
    echoCancellation: true,
    noiseSuppression: true,
    autoGainControl: true,
  };
  if (deviceId != null && deviceId.length > 0) {
    return { ...processing, deviceId: { exact: deviceId } };
  }
  return processing;
}

/**
 * Timers that bound how long a 1×1 call may sit in a non-terminal state.
 *
 * Without them a call that nobody answers never ends: the caller shows
 * "Calling…" forever and the callee's banner rings until the tab closes. Each
 * expiry publishes `End{timedOut}` so the other side stops too.
 */
export interface CallSessionTiming {
  /** How long a call waits for the ANSWER: the callee rings for this long,
   *  and the caller waits for this long from `dial()`, before either ends the
   *  call as `timedOut` (`No answer`).
   *
   *  It has no default and must be supplied, because it is the one number
   *  here that two DIFFERENT devices have to agree on: if the two sides
   *  disagree, one files a missed call while the other is still ringing.
   *  Its one home is `lakko_ui_rules::ring::CALL_RING_WINDOW_MS`, reached
   *  over wasm as `callRingWindowMs()`; see `calls/ring-rule.ts`. The other
   *  three below are this device's own business and do default. */
  readonly ringTimeoutMs: number;
  /** From the answer (caller: the answer arriving; callee: the accept) until
   *  ICE connects. A separate window from the ring, counted from the answer:
   *  a call answered late in the ring must not be cut off by a clock that
   *  started at the dial (`docs/ux/calls.md` section 3.4). Ends as `error`
   *  (`Call failed`), never `timedOut`, because someone did answer. */
  readonly connectTimeoutMs: number;
  /** ICE `disconnected` is often transient (a Wi-Fi roam, a brief loss); the
   *  call ends only if it has not recovered after this grace. `failed` still
   *  ends the call at once. */
  readonly iceDisconnectedGraceMs: number;
  /** How long a CONNECTED call may receive nothing at all before it is over
   *  (`media-watchdog.ts`). The ICE state answers whether two addresses can
   *  still reach each other, which is not whether there is still a call; this
   *  is the window for the case the ICE state cannot see. `0` disables it. */
  readonly mediaSilenceTimeoutMs: number;
}

/** What this session decides for itself, and what it must be told.
 *
 *  The three defaults are chosen, not measured: 30 s is several times what a
 *  direct connection needs, so a call that has not connected by then is not
 *  going to; 10 s is long enough to ride out a roam and short enough that a
 *  dead call does not look alive; 30 s of NOTHING arriving is several times
 *  any renegotiation gap and is how a call that outlived its peer by 45
 *  minutes would have been caught.
 *
 *  `ringTimeoutMs` is deliberately NOT among them — see its doc on
 *  {@link CallSessionTiming}. It used to be a `45_000` literal here, one of
 *  several across the three clients with nothing holding them equal. */
export const DEFAULT_CALL_SESSION_TIMING: Omit<CallSessionTiming, "ringTimeoutMs"> = {
  connectTimeoutMs: 30_000,
  iceDisconnectedGraceMs: 10_000,
  mediaSilenceTimeoutMs: 30_000,
};

/** The timing a caller supplies: the shared ring window, and any override of
 *  the three this session defaults. */
export type CallSessionTimingInput = Partial<Omit<CallSessionTiming, "ringTimeoutMs">> &
  Pick<CallSessionTiming, "ringTimeoutMs">;


/** Generate a fresh 16-byte call_id. */
export function mintCallId(): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(16));
}

/**
 * 1×1 call session. One instance per active call.
 *
 * The outbound path: caller drives `dial(media)`; the
 * session creates the PeerConnection, gets the SDP offer,
 * emits the Offer signaling event, and waits for an
 * Answer.
 *
 * The inbound path: caller invokes `applyIncomingSignaling`
 * for every parsed `ParsedCallSignaling` payload that
 * arrived for the chat group; the session dispatches on
 * `kind` and either accepts the offer (ringer surface) or
 * applies the answer / candidate to its PeerConnection.
 *
 * Lifecycle: caller `hangUp(reason)` to tear down; `dispose()`
 * is automatic on `done`.
 */
export class CallSession {
  private placedNowhere = false;
  /** 16-byte call id minted by the offerer. Stable across
   *  the call lifetime. */
  readonly callId: Uint8Array;

  private state: WebCallState = "idle";
  private isOfferer = false;
  private localStream: MediaStream | null = null;
  private pc: RTCPeerConnection | null = null;
  private pendingCandidates: RTCIceCandidateInit[] = [];

  // Perfect-negotiation state (piece B2b: 1×1 mid-call add-track). The OFFERER
  // (caller) is the impolite peer, the ANSWERER (callee) is polite
  // (`polite = !isOfferer`). `negotiationReady` gates `onnegotiationneeded` off
  // during the manual initial offer/answer so it is not duplicated; it flips
  // true once that initial exchange is applied.
  private makingOffer = false;
  private isSettingRemoteAnswerPending = false;
  private negotiationReady = false;
  /** Whether this call ever reached `connected` on this device. Decides the
   *  wake of its End: an End for a call that connected reaches a peer that is
   *  in the call and awake, so it is published silently. */
  private everConnected = false;
  /** The per-call SFrame seed and this client's MLS leaf, armed by
   *  {@link setCallKey} before the peer connection opens. `null` means the
   *  call refuses rather than sending anything unsealed (invariant #4). */
  private callKey: { readonly seed: Uint8Array; readonly localLeaf: number } | null = null;

  /** Said once per call when a sender is found with no SFrame transform. */
  private unsealedSenderWarned = false;

  /**
   * Insertable-streams browsers only (`./sframe-transport.ts`); nothing
   * elsewhere. There a sender or receiver whose encoded streams were never
   * taken passes no media, so after every step that can create a transceiver
   * every receiver without a transform is given one, and a sender left
   * without one is said out loud. Nothing here can let a frame through
   * unsealed: an uncovered endpoint is silent, not plain.
   */
  private coverSFrame(): void {
    if (sframeTransport() !== "encoded-streams") return;
    const pc = this.pc;
    const key = this.callKey;
    if (pc === null || key === null) return;
    const cover = coverEncodedStreams(pc, key.seed);
    if (cover.unsealedSenders.length > 0 && !this.unsealedSenderWarned) {
      this.unsealedSenderWarned = true;
      // eslint-disable-next-line no-console
      console.warn(
        `CallSession: ${cover.unsealedSenders.length} sender(s) have no SFrame transform; ` +
          "their media does not flow",
      );
    }
  }

  /** Logged once per session; see {@link armFrameDescriptor}. */
  private frameDescriptorWarned = false;
  /** Logged once per session; see {@link armFrameDescriptor}. */
  private vp8Warned = false;

  /** Receivers that already carry this session's decrypt transform. */
  private readonly sealedReceivers = new WeakSet<RTCRtpReceiver>();

  /** Senders that already carry this session's encrypt transform. */
  private readonly sealedSenders = new WeakSet<RTCRtpSender>();

  /** Attach the encrypt transform to `sender` unless it has one. A second
   *  attach would build a new transform on the script-transform path, which
   *  Chromium may not run (see {@link sealSendersOfTransceivers}). */
  private sealSender(
    sender: RTCRtpSender,
    key: { readonly seed: Uint8Array; readonly localLeaf: number },
    kind?: "audio" | "video",
    streamIndex?: number,
  ): void {
    if (this.sealedSenders.has(sender)) return;
    this.sframe.attachSender(sender, {
      seed: key.seed,
      localLeaf: key.localLeaf,
      ...(kind !== undefined ? { kind } : {}),
      ...(streamIndex !== undefined ? { streamIndex } : {}),
    });
    this.sealedSenders.add(sender);
  }

  /** This device's screen share, while it sends one (ADR-0083). */
  private screen: { readonly streamId: string; readonly transceivers: RTCRtpTransceiver[] } | null =
    null;

  /**
   * Add a screen capture to the call (ADR-0083 section 4): its picture and,
   * when there is any, its sound, each on a transceiver of its own, sealed
   * as the camera is on this path but under stream index 2 (picture) and 5
   * (sound), so it never shares the camera's key. The renegotiation the
   * camera uses describes them; the SDP sent names their stream with the
   * `~screen` suffix, which is how the other device knows it is a share.
   *
   * Rejects with {@link ScreenShareNotSealedError} when a sender could not be
   * sealed (nothing of the share is left in the call), with a plain error
   * when there is no connection yet or a share is already there.
   */
  async attachScreenShare(capture: MediaStream): Promise<void> {
    const pc = this.pc;
    const key = this.callKey;
    if (pc === null || key === null || this.callIsOver()) {
      throw new Error("no media connection to share the screen on");
    }
    if (this.screen !== null) throw new Error("a screen share is already in the call");
    const tracks = [...capture.getVideoTracks().slice(0, 1), ...capture.getAudioTracks().slice(0, 1)];
    const transceivers: RTCRtpTransceiver[] = [];
    try {
      for (const track of tracks) {
        // A new transceiver, never one the other side's offer created (whose
        // sender may already carry the camera's seal).
        transceivers.push(pc.addTransceiver(track, { direction: "sendonly", streams: [capture] }));
      }
    } catch (err) {
      this.removeTransceivers(pc, transceivers);
      throw err instanceof Error ? err : new Error(String(err));
    }
    try {
      for (const transceiver of transceivers) {
        const video = transceiver.sender.track?.kind === "video";
        this.sealSender(
          transceiver.sender,
          key,
          video ? "video" : "audio",
          video ? SFRAME_STREAM_SCREEN_SHARE : SFRAME_STREAM_SCREEN_AUDIO,
        );
      }
    } catch (err) {
      this.removeTransceivers(pc, transceivers);
      throw new ScreenShareNotSealedError(err);
    }
    this.screen = { streamId: capture.id, transceivers };
  }

  /** Take this device's screen share out of the call. Idempotent. */
  detachScreenShare(): void {
    const slot = this.screen;
    if (slot === null) return;
    this.screen = null;
    const pc = this.pc;
    if (pc !== null) this.removeTransceivers(pc, slot.transceivers);
  }

  private removeTransceivers(pc: RTCPeerConnection, transceivers: readonly RTCRtpTransceiver[]): void {
    for (const transceiver of transceivers) {
      try {
        if (typeof transceiver.stop === "function") transceiver.stop();
        else pc.removeTrack(transceiver.sender);
      } catch {
        try {
          pc.removeTrack(transceiver.sender);
        } catch {
          // A closed connection has nothing left to remove.
        }
      }
    }
  }

  /** An SDP this device sends, with its share's stream named as a share. */
  private outgoingSdp(sdp: string): string {
    return withScreenStreamId(sdp, this.screen?.streamId ?? null);
  }

  /**
   * Attach the encrypt transform to the sender of every transceiver that has
   * none yet, right after a remote offer was applied and before the answer.
   *
   * Chromium does not run a send transform set on the sender that `addTrack`
   * hands back when it REUSES a transceiver a remote offer created: the
   * answerer of a video call turning its camera on (it captures audio only and
   * adds the camera to the video transceiver the caller's offer made) sent its
   * video unsealed, and the caller dropped every frame of it ("media frame
   * version ... is not supported"; seen on test.lakko.me 2026-10-24, both
   * peers Chrome). Measured in Chromium 1243 (Playwright 1.63) with a marking
   * worker: that transform received 0 of 58 frames, with the transform set
   * after `addTrack`, after the renegotiation, or one task later; set on the
   * transceiver's sender as soon as the transceiver existed, before any track,
   * it received all 58, and it kept them when `addTrack` later reused the
   * sender. A transceiver `addTrack` creates is not affected. Nothing leaves
   * unsealed when this is skipped on another browser: the peer drops it.
   */
  private sealSendersOfTransceivers(pc: RTCPeerConnection): void {
    const key = this.callKey;
    if (key === null) return;
    for (const t of pc.getTransceivers()) {
      const kind = t.receiver.track.kind === "video" ? "video" : "audio";
      this.sealSender(t.sender, key, kind);
    }
  }

  /** Attach the decrypt transform to `receiver` unless it has one. Returns
   *  whether it attached. */
  private sealReceiver(receiver: RTCRtpReceiver, seed: Uint8Array): boolean {
    if (this.sealedReceivers.has(receiver)) return false;
    this.sframe.attachReceiver(receiver, { seed });
    this.sealedReceivers.add(receiver);
    return true;
  }

  /**
   * Attach the decrypt transform to the receiver of every transceiver that
   * has none yet, and let one task pass before the caller applies a
   * description.
   *
   * Chromium bypasses a receive transform set from `ontrack` on a
   * transceiver this side created with `addTrack` (a two-way audio
   * transceiver, which is every 1x1 call): the frames go to the decoder
   * still sealed and the transform never sees one. Measured 2026-10-22 in
   * Chromium 1243 (Playwright 1.63) with a counting worker: two-way audio
   * with the receive transforms set in `ontrack` delivered 0 frames to them
   * while 300 were sent each way and the peer played the sealed bytes as
   * noise (about 60% of samples concealed); the same transforms set before
   * the description, with one task in between, delivered every frame. WebKit
   * delivered every frame either way. A transceiver a remote offer creates
   * is not affected, which is why the group call (receive-only transceivers
   * from the SFU's offers) never showed it; `ontrack` still covers those.
   */
  private async sealReceiversBeforeDescription(pc: RTCPeerConnection): Promise<void> {
    const key = this.callKey;
    if (key === null) return;
    let attached = false;
    for (const t of pc.getTransceivers()) {
      if (this.sealReceiver(t.receiver, key.seed)) attached = true;
    }
    if (attached) await new Promise<void>((resolve) => setTimeout(resolve, 0));
  }

  /** In-flight guard for {@link addLocalVideo} (no double-acquire). */
  private addingVideo = false;
  /** Mid-call camera-on lifecycle (piece B). */
  private videoPublishState: VideoPublishState = "off";

  /** G3-3-conn-quality — `getStats()` poll loop handle, armed on
   *  the first `connected` transition, cleared in `dispose()`. */
  private statsTimer: ReturnType<typeof setInterval> | null = null;
  /** What the receivers report, logged with every stats sample and as a
   *  total in `dispose()` (ADR-0067 step 1, as the group call does). */
  private readonly inboundCounters = new InboundCounterLog();
  /** Previous candidate-pair byte counters + monotonic timestamp
   *  (ms) used to derive the down/up bitrate. */
  private prevStatsBytes: {
    received: number;
    sent: number;
    atMs: number;
  } | null = null;
  /** How long inbound media has been silent (`media-watchdog.ts`). */
  private inboundActivity: InboundActivity | null = null;
  /** Which network path the media takes, logged when it is first known and
   *  when it changes (`call-path.ts`). */
  private callPathLog = new CallPathLog();

  /** The inbound offer this callee is ringing for. Held, not applied: the
   *  answerer captures media and builds its `RTCPeerConnection` only when the
   *  user accepts (see {@link accept}). `null` for the caller and once
   *  accepted. */
  private pendingOffer: { sdp: string; mediaAudio: boolean } | null = null;
  /** In-flight guard for {@link accept} (a double tap must not answer twice). */
  private accepting = false;
  /** Ring timeout (callee) or establish timeout (both sides). One slot: a
   *  call is only ever in one of those phases. */
  private phaseTimer: ReturnType<typeof setTimeout> | null = null;
  /** Grace timer armed on ICE `disconnected`. */
  private iceGraceTimer: ReturnType<typeof setTimeout> | null = null;
  private readonly timing: CallSessionTiming;

  /** Set by the call-host layer; `null` means STUN-only ICE. */
  private turnProvider: (() => Promise<RTCIceServer | null>) | null = null;

  constructor(
    callId: Uint8Array,
    private readonly callbacks: CallSessionCallbacks,
    timing: CallSessionTimingInput,
    private readonly sframe: SFrameAttachers = DEFAULT_SFRAME_ATTACHERS,
  ) {
    this.callId = callId;
    this.timing = { ...DEFAULT_CALL_SESSION_TIMING, ...timing };
  }

  /**
   * Bind the TURN-credential source used when the peer connection is built.
   *
   * A provider rather than a list, because the credential is short-lived and
   * minted per call. Fail-soft: a provider that resolves `null`, or throws,
   * leaves the call on STUN.
   */
  setTurnProvider(provider: (() => Promise<RTCIceServer | null>) | null): void {
    this.turnProvider = provider;
  }

  /** Resolve the ICE servers for this call: STUN always, TURN when minted. */
  /** Lakko's own servers only (`./ice-servers.ts`): the minted TURN entry
   *  and STUN on its host, or none. */
  private async resolveIceServers(): Promise<RTCIceServer[]> {
    const provider = this.turnProvider;
    let turn: RTCIceServer | null = null;
    if (provider !== null) {
      try {
        turn = await provider();
      } catch {
        turn = null;
      }
    }
    const servers = iceServersFor(turn);
    // eslint-disable-next-line no-console
    console.info(`call ice servers: ${iceServersLine(servers)}`);
    return servers;
  }

  /**
   * Arm the per-call SFrame key before the peer connection opens.
   *
   * `seed` is the MLS exporter output
   * `group.exportSecret(identity, "lakko/call/v1", callId, 32)` and
   * `localLeaf` this client's leaf index — the same two values iOS and Android
   * derive for the same call, which is what makes the keys agree across
   * stacks (audit-pack C-1). The seed becomes this session's frame key and the
   * leaf is stamped into every sealed frame's KID (ADR-0035).
   *
   * Call it before {@link dial} or {@link accept}. Without it the call
   * refuses: until 2026-09-21 this path attached no SFrame at all while both
   * natives sealed every 1x1 frame and required sealing on receipt, so a call
   * between a browser and a phone exchanged media neither end could use -
   * the phone rendered no frame of the browser's video and the browser
   * assembled none of the phone's.
   */
  setCallKey(seed: Uint8Array, localLeaf: number): void {
    this.callKey = { seed, localLeaf };
  }

  /** Whether media on this call is sealed — what the encryption chip reads. */
  hasCallKey(): boolean {
    return this.callKey !== null;
  }

  /** A call this device was placing ended before anything of it was sent:
   *  it is no call at all, so nothing records it. */
  neverPlaced(): boolean {
    return this.placedNowhere;
  }

  /** Current state — UI re-reads after every callback. */
  getState(): WebCallState {
    return this.state;
  }

  /** Current mid-call camera-on lifecycle state (piece B). */
  getVideoPublishState(): VideoPublishState {
    return this.videoPublishState;
  }

  private setVideoPublishState(state: VideoPublishState): void {
    if (this.videoPublishState === state) return;
    this.videoPublishState = state;
    this.callbacks.onVideoPublishState?.(state);
  }

  /**
   * Outbound side: start dialing. Acquires the mic via
   * `getUserMedia`, builds the PeerConnection, generates
   * the SDP offer, emits the `Offer` signaling event.
   *
   * Throws on mic-permission denial or PeerConnection
   * failure — the caller surfaces the error in the UI.
   */
  async dial(media: CallMedia): Promise<void> {
    if (this.state !== "idle") return;
    this.isOfferer = true;
    this.setState("outgoing");
    // Nobody answering is a normal outcome, not a hang: end as `timedOut`
    // (published, so the callee's banner stops too) unless the answer comes
    // first, which starts the connect window in its place.
    this.armPhaseTimer(this.timing.ringTimeoutMs, () => {
      if (this.state === "outgoing") {
        void this.endLocal("timedOut");
      }
    });
    await this.openPeerConnection(media);
    // The call may have ended while the capture permission prompt was open
    // (read through the getter: the narrowing above predates the await).
    if (this.getState() !== "outgoing") return;
    const pc = this.pc;
    if (pc === null) throw new Error("peer connection not initialised");
    this.armFrameDescriptor(pc);
    await this.sealReceiversBeforeDescription(pc);
    if (this.offerIsStale(pc)) return;
    let offer: RTCSessionDescriptionInit;
    try {
      offer = await pc.createOffer();
      // The call ended while the offer was being made: no Offer is published
      // for a call whose End is already out.
      if (this.offerIsStale(pc)) return;
      await pc.setLocalDescription(offer);
    } catch (err) {
      // A peer connection closed by an ended call may reject what was pending
      // on it; that is the call having ended, not a failure to start it.
      if (this.offerIsStale(pc)) return;
      throw err;
    }
    if (this.offerIsStale(pc)) return;
    if (offer.sdp === undefined) {
      throw new Error("createOffer produced no SDP body");
    }
    const wasm = await loadWasm();
    if (this.offerIsStale(pc)) return;
    const bytes = wasm.composeCallOfferSignaling(
      this.callId,
      offer.sdp,
      BigInt(Date.now()),
      media.audio,
      media.video,
    );
    // The dial: the one offer that rings the callee.
    this.callbacks.onOutboundSignaling(bytes, "call");
  }

  /** Whether the dial that built `pc` is over: the call left `outgoing`, or
   *  the peer connection is no longer this session's. Read through the
   *  getter, since the callers' narrowing predates their awaits. */
  private offerIsStale(pc: RTCPeerConnection): boolean {
    return this.getState() !== "outgoing" || this.pc !== pc;
  }

  /**
   * Inbound side: route a parsed signaling payload into
   * the state machine. Caller's chat-message dispatcher
   * filters by call_id (only payloads with this session's
   * call_id reach this method), delivers each journal row
   * exactly once, and feeds them in order — one at a time.
   *
   * Group-control bodies (`groupStart` / `groupJoin` /
   * `groupLeave`) are dropped here as well as at the
   * dispatcher (C-11): they never advance a 1×1 state machine.
   */
  async applyIncomingSignaling(parsed: ParsedCallSignaling): Promise<void> {
    switch (parsed.kind) {
      case "offer":
        if (this.state === "connecting" || this.state === "connected") {
          // Mid-call renegotiation offer (piece B2b: the peer added a video
          // track). NOT a fresh call — apply it with the perfect-negotiation
          // glare guard and answer, keeping the call up.
          await this.applyRenegotiationOffer(parsed.sdp);
          return;
        }
        // Anything but `idle` here is a repeat, never a renegotiation: a
        // second copy of the offer we are already ringing for, an offer that
        // arrives while we are dialing, or one that lands while the call is
        // ending. Treating a ringing callee's repeat as a renegotiation is
        // what used to publish an Answer the user never gave and then leave
        // `accept()` running `createAnswer` in `stable`.
        if (this.state !== "idle") return;
        // Fresh inbound call — we're the answerer. Nothing is captured and no
        // peer connection exists until the user accepts: ringing must not
        // open the microphone or raise a permission prompt with no banner to
        // explain it, and an Accept tap must not race a capture still in
        // flight. The offer is held and applied in `accept()`.
        this.isOfferer = false;
        this.pendingOffer = { sdp: parsed.sdp, mediaAudio: parsed.mediaAudio };
        this.setState("ringing");
        this.armPhaseTimer(this.timing.ringTimeoutMs, () => {
          if (this.state === "ringing" && !this.accepting) {
            void this.endLocal("timedOut");
          }
        });
        break;

      case "answer":
        if (this.pc === null) return;
        if (this.state === "outgoing") {
          // Initial answer to our dial.
          const pc = this.pc;
          try {
            await pc.setRemoteDescription({
              type: "answer",
              sdp: parsed.sdp,
            });
            // The call ended (hung up, timed out, ended by the peer) while
            // the answer was being applied: it does not come back as
            // `connecting`, and no connect timer is armed for it.
            if (this.offerIsStale(pc)) return;
            this.coverSFrame();
            await this.applyQueuedCandidates();
            if (this.offerIsStale(pc)) return;
            this.setState("connecting");
            // Answered: the ring clock is over, the connect clock starts now.
            this.armConnectTimer();
            // ICE may already have connected during the two awaits above.
            this.promoteIfTransportUp();
            // Arm onnegotiationneeded for future local track changes (B2b).
            this.negotiationReady = true;
          } catch (err) {
            // A peer connection closed by an ended call may reject the
            // description; the call is over already and nothing failed.
            if (this.offerIsStale(pc)) return;
            // eslint-disable-next-line no-console
            console.warn("setRemoteDescription(answer) failed", err);
            this.endLocal("error");
          }
        } else if (this.state === "connecting" || this.state === "connected") {
          // Mid-call renegotiation answer (to a reneg offer we sent).
          const pc = this.pc;
          try {
            this.isSettingRemoteAnswerPending = true;
            await pc.setRemoteDescription({
              type: "answer",
              sdp: parsed.sdp,
            });
            // Ended meanwhile: `dispose()` reset the flag, leave it so.
            if (this.pc !== pc) return;
            this.isSettingRemoteAnswerPending = false;
            this.coverSFrame();
          } catch (err) {
            if (this.pc !== pc) return;
            this.isSettingRemoteAnswerPending = false;
            // eslint-disable-next-line no-console
            console.warn("setRemoteDescription(reneg answer) failed", err);
          }
        }
        break;

      case "iceCandidate": {
        const init: RTCIceCandidateInit = {
          candidate: parsed.candidate,
          sdpMid: parsed.sdpMid,
          sdpMLineIndex: parsed.sdpMLineIndex,
        };
        if (this.pc === null || this.pc.remoteDescription === null) {
          // Queue until we apply the remote description —
          // canonical trickle-ICE pattern.
          this.pendingCandidates.push(init);
        } else {
          try {
            await this.pc.addIceCandidate(init);
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("addIceCandidate failed", err);
          }
        }
        break;
      }

      case "end":
        this.endRemote(oneToOneEndReason(parsed.endReason));
        break;

      default:
        // Future-compat: drop unknown kinds silently.
        break;
    }
  }

  /**
   * Local user accepted an incoming call from the ringer
   * UI. Builds the SDP answer, emits the `Answer`
   * signaling event.
   */
  async accept(): Promise<void> {
    if (this.state !== "ringing" || this.isOfferer || this.accepting) return;
    const offer = this.pendingOffer;
    if (offer === null) {
      void this.endLocal("error");
      return;
    }
    this.accepting = true;
    // The ring timeout no longer applies; the connect window does (a capture
    // prompt left open, or a transport that never comes up).
    this.armConnectTimer();
    try {
      // Answerer capture is audio-only, regardless of the offerer's video
      // flag. The answerer's camera must NOT turn on just because the OFFERER
      // advertised video — capturing it here acquired-then-muted the track
      // (the camera-sync effect promptly disabled the freshly acquired
      // camera, so the peer saw no answerer video). The answerer opts into
      // SENDING video explicitly via "Accept with video" (accept → camOn=true
      // → the shipped `addLocalVideo` mid-call camera-on path). It still
      // RECEIVES the offerer's video either way (see `ontrack`).
      await this.openPeerConnection({ audio: offer.mediaAudio, video: false });
      // The caller hung up, or the establish timer fired, while the capture
      // was in flight: `openPeerConnection` already released it.
      if (this.state !== "ringing") return;
      const pc = this.pc;
      if (pc === null) return;
      await this.sealReceiversBeforeDescription(pc);
      if (this.state !== "ringing" || this.pc !== pc) return;
      await pc.setRemoteDescription({ type: "offer", sdp: offer.sdp });
      this.sealSendersOfTransceivers(pc);
      this.coverSFrame();
      this.pendingOffer = null;
      await this.applyQueuedCandidates();
      if (this.state !== "ringing" || this.pc !== pc) return;
      // An answer is only meaningful against an applied remote offer. Checked
      // rather than assumed: `createAnswer` in any other state throws, and
      // the old path that reached it in `stable` turned an Accept tap into
      // `End{error}`.
      if (pc.signalingState !== "have-remote-offer") {
        throw new Error(
          `cannot answer in signaling state ${pc.signalingState}`,
        );
      }
      this.armFrameDescriptor(pc);
      const answer = await pc.createAnswer();
      if (this.getState() !== "ringing" || this.pc !== pc) return;
      await pc.setLocalDescription(answer);
      // The caller hung up, or the establish timer fired, while the answer
      // was being made: an ended call does not come back as `connecting`.
      if (this.getState() !== "ringing" || this.pc !== pc) return;
      if (answer.sdp === undefined) {
        throw new Error("createAnswer produced no SDP body");
      }
      // Set before the next await, so an ICE `connected` that fires right
      // after the local description applies advances connecting → connected
      // rather than being overwritten back to connecting.
      this.setState("connecting");
      // And ask, in case it connected before this line ran at all.
      this.promoteIfTransportUp();
      // Arm onnegotiationneeded for future local track changes (B2b): the
      // answer is sent + our local description is set, so the initial
      // negotiation is applied on our side.
      this.negotiationReady = true;
      const wasm = await loadWasm();
      // No Answer is published for a call that ended meanwhile.
      if (this.pc !== pc) return;
      const bytes = wasm.composeCallAnswerSignaling(this.callId, answer.sdp);
      this.callbacks.onOutboundSignaling(bytes, "none");
    } catch (err) {
      // The call ended while the accept was in flight (a closed peer
      // connection may reject what was pending on it): nothing failed.
      const now = this.getState();
      if (now === "ending" || now === "done") return;
      // eslint-disable-next-line no-console
      console.warn("accept failed", err);
      void this.endLocal("error");
    } finally {
      this.accepting = false;
    }
  }

  /** Local user declined the incoming call. */
  async decline(): Promise<void> {
    this.endLocal("declined");
  }

  /** Local user hangs up an active call. */
  async hangUp(reason: WebCallEndReason = "hungUp"): Promise<void> {
    this.endLocal(reason);
  }

  /**
   * G3-3-web-call-controls — mute / unmute the local microphone by
   * flipping the captured audio track's `enabled` flag. This is a
   * local-only gate (the track stays in the PeerConnection; we just
   * stop sending real samples), so it needs no renegotiation and
   * takes effect immediately. The camera has the same-shaped local
   * gate ({@link setCameraEnabled}) plus a mid-call acquire path
   * ({@link addLocalVideo}) for a call that started audio-only.
   */
  setMicrophoneEnabled(on: boolean): void {
    if (this.localStream === null) return;
    for (const track of this.localStream.getAudioTracks()) {
      track.enabled = on;
    }
  }

  /**
   * Toggle the local camera by flipping the captured video track's
   * `enabled` flag — a local-only gate (the track stays in the
   * PeerConnection, no renegotiation), same shape as
   * {@link setMicrophoneEnabled}. A no-op on an audio-only call (no
   * video track was captured).
   */
  setCameraEnabled(on: boolean): void {
    if (this.localStream === null) return;
    for (const track of this.localStream.getVideoTracks()) {
      track.enabled = on;
    }
  }

  /**
   * Turn the camera ON mid-call (piece B2b) when the 1×1 call started
   * audio-only. Acquires the camera, adds the track, and renegotiates via
   * `onnegotiationneeded` (the 1×1 path is P2P DTLS-SRTP — no SFrame).
   * Idempotent (re-enables an existing sender) + in-flight-guarded. Fail-safe:
   * on a capture / attach failure the capture is torn down (never a half-added
   * sender) and it throws, so the caller reverts the toggle + surfaces a
   * message; the call stays up audio-only.
   *
   * @throws on capture failure (permission denied / no camera / attach error).
   */
  async addLocalVideo(): Promise<void> {
    const pc = this.pc;
    if (pc === null || this.localStream === null) return;
    // A live video sender already exists (a call started as video, or a prior
    // addLocalVideo): just re-enable — no capture, no renegotiation.
    const existing = pc.getSenders().find((s) => s.track?.kind === "video");
    if (existing?.track != null && existing.track.readyState === "live") {
      existing.track.enabled = true;
      this.setVideoPublishState("live");
      return;
    }
    if (this.addingVideo) return; // capture in flight — do not double-acquire
    this.addingVideo = true;
    // The acquire + renegotiation is async (permission prompt + capture + SDP);
    // report `starting` so the camera button shows a spinner + disables.
    this.setVideoPublishState("starting");
    let captured: MediaStream;
    try {
      captured = await navigator.mediaDevices.getUserMedia({ video: true });
    } catch (err) {
      this.addingVideo = false;
      this.setVideoPublishState("failed");
      throw err instanceof Error ? err : new Error(String(err));
    }
    const track = captured.getVideoTracks()[0];
    if (track === undefined) {
      this.addingVideo = false;
      this.setVideoPublishState("failed");
      throw new Error("no camera track captured");
    }
    try {
      // Re-read state after the await — the call may have ended while
      // getUserMedia was in flight.
      const localStream = this.localStream;
      const pc2 = this.pc;
      if (localStream === null || pc2 === null) {
        track.stop();
        this.addingVideo = false;
        this.setVideoPublishState("off");
        return;
      }
      localStream.addTrack(track);
      const sender = pc2.addTrack(track, localStream); // fires onnegotiationneeded -> reneg
      // The camera arriving mid-call is a new sender and needs its own seal;
      // without this the video would leave unsealed while the audio beside it
      // is sealed, and the peer would drop every frame of it.
      // A sender `addTrack` reused already carries the seal it was given when
      // its transceiver appeared (`sealSendersOfTransceivers`); it is not
      // replaced.
      const midCallKey = this.callKey;
      if (midCallKey !== null) this.sealSender(sender, midCallKey, "video");
      // Refresh the self-preview stream (it now carries a video track).
      this.callbacks.onLocalStream?.(localStream);
      // The local self-preview is live now (the track is captured + attached);
      // the SDP renegotiation continues in the background.
      this.setVideoPublishState("live");
    } catch (err) {
      track.stop();
      this.addingVideo = false;
      this.setVideoPublishState("failed");
      throw err instanceof Error ? err : new Error(String(err));
    }
    this.addingVideo = false;
  }

  /**
   * Switch the live microphone to a specific `deviceId` mid-call (from the
   * in-call device picker). Re-captures that one kind with the picked device
   * and `replaceTrack`s it on the existing sender — same-kind, so no
   * renegotiation. Best-effort: a capture / replace failure keeps the current
   * track. A no-op when there is no live audio track to replace.
   */
  async switchAudioInput(deviceId: string): Promise<void> {
    await this.switchInput("audio", deviceId);
  }

  /** Switch the live camera to a specific `deviceId` mid-call. Same shape as
   *  {@link switchAudioInput}; a no-op on an audio-only call. */
  async switchVideoInput(deviceId: string): Promise<void> {
    await this.switchInput("video", deviceId);
  }

  private async switchInput(
    kind: "audio" | "video",
    deviceId: string,
  ): Promise<void> {
    if (this.pc === null || this.localStream === null) return;
    if (deviceId.length === 0) return;
    const pc = this.pc;
    const localStream = this.localStream;
    // Only meaningful if a sender of this kind is already on the wire.
    const sender = pc
      .getSenders()
      .find((s) => s.track?.kind === kind);
    if (sender === undefined) return;

    const constraints: MediaStreamConstraints =
      kind === "audio"
        ? { audio: { deviceId: { exact: deviceId } } }
        : { video: { ...CAMERA_CAPTURE_SIZE, deviceId: { exact: deviceId } } };

    let captured: MediaStream;
    try {
      captured = await navigator.mediaDevices.getUserMedia(constraints);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`getUserMedia (${kind} device switch) failed`, err);
      return;
    }
    // The call ended while the capture (and its permission prompt) was open:
    // release the new device at once. `dispose()` nulled `this.localStream`,
    // so nothing below may touch it.
    if (this.pc !== pc || this.localStream !== localStream) {
      for (const t of captured.getTracks()) t.stop();
      return;
    }
    const oldTracks =
      kind === "audio"
        ? localStream.getAudioTracks()
        : localStream.getVideoTracks();
    const newTrack =
      kind === "audio"
        ? captured.getAudioTracks()[0]
        : captured.getVideoTracks()[0];
    if (newTrack === undefined) {
      for (const t of captured.getTracks()) t.stop();
      return;
    }
    // Preserve the current mute (enabled) state so a switch never unmutes.
    newTrack.enabled = oldTracks[0]?.enabled ?? true;

    try {
      await sender.replaceTrack(newTrack);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`replaceTrack (${kind} device switch) failed`, err);
      newTrack.stop();
      return;
    }
    if (this.pc !== pc || this.localStream !== localStream) {
      // Ended while the track was being replaced: `dispose()` stopped the
      // old tracks, and the new one is in no stream it knew of.
      newTrack.stop();
      return;
    }

    // Swap the track inside the local stream so the self-preview rebinds.
    for (const t of oldTracks) {
      localStream.removeTrack(t);
      t.stop();
    }
    localStream.addTrack(newTrack);
    this.callbacks.onLocalStream?.(localStream);
  }

  /** Idempotent tear-down — safe to call multiple times. */
  dispose(): void {
    this.clearPhaseTimer();
    this.clearIceGraceTimer();
    this.pendingOffer = null;
    this.pendingCandidates = [];
    // G3-3-conn-quality — stop the stats poll loop before
    // dropping the peer connection.
    if (this.statsTimer !== null) {
      clearInterval(this.statsTimer);
      this.statsTimer = null;
    }
    this.prevStatsBytes = null;
    this.inboundActivity = null;
    this.callPathLog = new CallPathLog();
    for (const line of this.inboundCounters.total()) {
      // eslint-disable-next-line no-console
      console.info(line);
    }
    if (this.pc !== null) {
      try {
        this.pc.close();
      } catch {
        // best-effort
      }
      this.pc = null;
    }
    if (this.localStream !== null) {
      for (const track of this.localStream.getTracks()) {
        track.stop();
      }
      this.localStream = null;
    }
    // Reset perfect-negotiation state so a late onnegotiationneeded after
    // teardown no-ops (piece B2b).
    this.negotiationReady = false;
    this.makingOffer = false;
    this.isSettingRemoteAnswerPending = false;
    this.addingVideo = false;
    // The share's tracks went with the peer connection; the capture is its
    // owner's to stop.
    this.screen = null;
    // Back to `off` so a reused instance (or the UI after teardown) starts
    // clean — the camera button drops any spinner.
    this.setVideoPublishState("off");
    if (this.state !== "done") {
      this.setState("done");
    }
  }

  // -----------------------------------------------------------------
  // Internal
  // -----------------------------------------------------------------

  private async openPeerConnection(media: CallMedia): Promise<void> {
    // Fail closed BEFORE any capture, the way the group session does: a call
    // that cannot seal its media does not open the microphone at all.
    const key = this.callKey;
    if (key === null) {
      // eslint-disable-next-line no-console
      console.warn("CallSession: no call key armed — refusing the call (invariant #4)");
      void this.endLocal("error");
      throw new Error("call key not armed");
    }
    if (!this.sframe.supported()) {
      // eslint-disable-next-line no-console
      console.warn(
        "CallSession: this browser can hand encoded frames to no worker " +
          "(no RTCRtpScriptTransform, no createEncodedStreams): refusing the call (invariant #4)",
      );
      void this.endLocal("error");
      throw new Error("SFrame transform unsupported");
    }
    // Which of the two interfaces carries this call's frames to the worker.
    // eslint-disable-next-line no-console
    console.info(sframeTransportLine());
    // Honour the picker-selected device at capture time: a non-null
    // `deviceId` becomes an `{ exact }` constraint, else a bare boolean
    // (OS default). A disabled kind stays `false`.
    const constraints: MediaStreamConstraints = {
      audio: audioTrackConstraint(media.audio, media.audioDeviceId),
      video: trackConstraint(media.video, media.videoDeviceId),
    };
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia(constraints);
    } catch (err) {
      // The call ended while the permission prompt was open, and the prompt
      // was then dismissed or refused: nothing was captured and nothing
      // failed that the person has not already chosen. The callers re-check
      // the state and stand down.
      if (this.callIsOver()) return;
      // eslint-disable-next-line no-console
      console.warn("getUserMedia denied / failed", err);
      // Placing a call: no Offer left this device, so there is nobody to tell.
      if (this.isOfferer) this.endBeforeOffer();
      else void this.endLocal("error");
      throw err;
    }
    // The call ended while the capture (and its permission prompt) was in
    // flight. Release the microphone / camera at once rather than attaching
    // it to a peer connection nobody will ever close.
    if (this.callIsOver()) {
      for (const track of stream.getTracks()) track.stop();
      return;
    }
    this.localStream = stream;
    // Surface the local capture so the UI can render a self-preview
    // (the video track, if any). Audio-only callers ignore it.
    this.callbacks.onLocalStream?.(stream);

    // Built through `sframePeerConnectionConfig`: on a browser that reaches
    // the SFrame worker by `createEncodedStreams()` the connection has to be
    // constructed for it (`./sframe-transport.ts`); elsewhere the
    // configuration is unchanged.
    const iceServers = await this.resolveIceServers();
    // The call ended while the relay credential was being fetched. `dispose()`
    // stopped the stream it knew of; stopped again here in case this is not
    // that stream. No peer connection is built for a call that is over.
    if (this.callIsOver() || this.localStream !== stream) {
      for (const track of stream.getTracks()) track.stop();
      return;
    }
    const pc = new RTCPeerConnection(
      sframePeerConnectionConfig({
        iceServers,
        bundlePolicy: "max-bundle",
        rtcpMuxPolicy: "require",
      }),
    );
    this.pc = pc;

    // Renegotiate on a future local track change (piece B2b: addLocalVideo).
    // Gated by `negotiationReady` so the manual initial offer/answer is not
    // duplicated; it flips true once the initial exchange is applied.
    pc.onnegotiationneeded = (): void => {
      void this.onNegotiationNeeded();
    };

    for (const track of stream.getTracks()) {
      const sender = pc.addTrack(track, stream);
      // Attached between `addTrack` and the SDP build, the ordering the native
      // shims keep (C-48): the sender's track decides the stream index the
      // KID carries, so it must already be set.
      this.sealSender(sender, key);
    }

    pc.onicecandidate = (ev): void => {
      if (ev.candidate === null) return;
      const c = ev.candidate;
      if (c.candidate.length === 0) return;
      // Trickled candidates flow out through signaling.
      void (async (): Promise<void> => {
        const wasm = await loadWasm();
        // No candidate is published for a call that ended meanwhile.
        if (this.pc !== pc) return;
        const bytes = wasm.composeCallIceCandidateSignaling(
          this.callId,
          c.candidate,
          c.sdpMid ?? "",
          c.sdpMLineIndex ?? 0,
        );
        this.callbacks.onOutboundSignaling(bytes, "none");
      })();
    };

    pc.ontrack = (ev): void => {
      // `ev.streams[0]` carries the remote peer's tracks (audio,
      // and video if they are sending it). The UI binds it to an
      // `<audio>` sink + a remote-video tile; a mid-call camera-on
      // by the peer arrives as a later `ontrack` on the same stream.
      // One decrypt transform per receiver: each frame's own KID selects the
      // sender leaf and stream, so audio and camera need no separate arming.
      // A frame that fails to open is dropped in the worker, never forwarded
      // as plaintext.
      this.sealReceiver(ev.receiver, key.seed);
      const remote = ev.streams[0];
      if (remote !== undefined) {
        // The other person's screen share (ADR-0083) is not their camera.
        if (screenStreamPeerOf(remote.id) !== null) {
          this.callbacks.onRemoteScreenStream?.(remote, ev.track);
          return;
        }
        this.callbacks.onRemoteStream(remote);
      }
    };

    pc.oniceconnectionstatechange = (): void => {
      // A late event from a peer connection this session already closed.
      if (this.pc !== pc) return;
      const s = pc.iceConnectionState;
      if (s === "connected" || s === "completed") {
        this.promoteIfTransportUp();
      } else if (s === "disconnected") {
        // Often transient. End only if it has not recovered after the grace;
        // a recovery (`connected` / `completed`) clears the timer above.
        if (
          this.iceGraceTimer === null &&
          (this.state === "connecting" || this.state === "connected")
        ) {
          this.iceGraceTimer = setTimeout(() => {
            this.iceGraceTimer = null;
            if (this.pc !== pc) return;
            const now = pc.iceConnectionState;
            if (now === "disconnected" || now === "failed") {
              void this.endWithTransport("ICE stayed " + now + " past the grace", () => {
                const still = pc.iceConnectionState;
                return this.pc === pc && (still === "disconnected" || still === "failed");
              });
            }
          }, this.timing.iceDisconnectedGraceMs);
        }
      } else if (s === "failed") {
        void this.endWithTransport(
          "ICE failed",
          () => this.pc === pc && pc.iceConnectionState === "failed",
        );
      }
    };
  }

  /** The call is ending or over. A method, so a check made after an await
   *  is not narrowed away by one made before it. */
  private callIsOver(): boolean {
    return this.state === "ending" || this.state === "done";
  }

  private async applyQueuedCandidates(): Promise<void> {
    if (this.pc === null) return;
    for (const init of this.pendingCandidates) {
      try {
        await this.pc.addIceCandidate(init);
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("addIceCandidate (queued) failed", err);
      }
    }
    this.pendingCandidates = [];
  }

  /**
   * Renegotiate on a local track change (piece B2b: addLocalVideo added a
   * camera track). The offer initiator, gated so it only fires after the
   * initial exchange (`negotiationReady`) and never overlaps an in-flight
   * offer. If the state is not `stable` this defers — the browser re-fires
   * `onnegotiationneeded` on the next stable transition while the change is
   * still pending (also how a glare-rolled-back offer gets re-sent).
   */
  /**
   * Put the generic frame descriptor on every video transceiver before the
   * description that negotiates it.
   *
   * Without it a native receiver reads the key-frame flag off the first byte
   * of a VP8 payload this session has already sealed whole, so it never
   * assembles a frame: measured 2026-09-21 on a 1x1 web-to-Android video call
   * as `framesDecoded=0` on the caller against 373 video packets received, and
   * `Frames received: 0` on the phone's remote renderer while its own
   * self-preview rendered 34 in the same four seconds.
   *
   * The group path has armed this since `1424bd8`; this 1x1 path was missed,
   * which is the whole of why a two-person video call showed no picture while
   * a group one did. See `frame-descriptor.ts` for what the API is and what
   * the descriptor puts on the wire in the clear.
   */
  private armFrameDescriptor(pc: RTCPeerConnection): void {
    // The codec first: a sealed frame travels only as VP8
    // (`./video-codec-preference.ts`), and this runs before every description.
    const codecs = preferVp8(
      pc.getTransceivers() as unknown as readonly CodecPreferenceTransceiver[],
      videoReceiveCapabilities(),
    );
    if (codecs.unsupported > 0 && !this.vp8Warned) {
      this.vp8Warned = true;
      // eslint-disable-next-line no-console
      console.warn(
        `CallSession: ${codecs.unsupported} video transceiver(s) could not be set to VP8; this browser's video may not reach the other person`,
      );
    }
    const outcome = armGenericFrameDescriptor(
      pc.getTransceivers() as unknown as readonly HeaderExtensionTransceiver[],
    );
    if (outcome.unsupported > 0 && !this.frameDescriptorWarned) {
      this.frameDescriptorWarned = true;
      // eslint-disable-next-line no-console
      console.warn(
        `CallSession: ${outcome.unsupported} video transceiver(s) cannot negotiate the generic frame descriptor; a phone may not decode this browser's video`,
      );
    }
  }

  private async onNegotiationNeeded(): Promise<void> {
    if (!this.negotiationReady) return;
    const pc = this.pc;
    if (pc === null) return;
    if (this.makingOffer || pc.signalingState !== "stable") return;
    try {
      this.makingOffer = true;
      // The camera that triggered this renegotiation is a NEW video
      // transceiver, and it starts with the descriptor stopped like every
      // other: arm it before the offer that first describes it.
      this.armFrameDescriptor(pc);
      // The camera's transceiver is new; its receiver gets the decrypt
      // transform now, before the offer that describes it.
      await this.sealReceiversBeforeDescription(pc);
      if (this.pc !== pc || pc.signalingState !== "stable") return;
      await pc.setLocalDescription(); // implicit createOffer
      const sdp = pc.localDescription?.sdp;
      if (sdp === undefined) return;
      const wasm = await loadWasm();
      const bytes = wasm.composeCallOfferSignaling(
        this.callId,
        this.outgoingSdp(sdp),
        BigInt(Date.now()),
        true,
        true,
      );
      // A renegotiation of a call already past its dial: the peer is in the
      // call. A wake here showed the peer's iPhone an incoming call.
      this.callbacks.onOutboundSignaling(bytes, "none");
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("renegotiation offer failed", err);
    } finally {
      this.makingOffer = false;
    }
  }

  /**
   * Apply a mid-call renegotiation offer from the peer (piece B2b). Canonical
   * perfect-negotiation glare guard: the OFFERER (caller) is impolite, the
   * ANSWERER (callee) is polite (`polite = !isOfferer`). A polite peer yields
   * on a collision (implicit rollback via setRemoteDescription) and answers;
   * an impolite peer drops the colliding offer.
   */
  private async applyRenegotiationOffer(sdp: string): Promise<void> {
    const pc = this.pc;
    if (pc === null) return;
    // A transceiver this side added that the peer now starts sending on.
    await this.sealReceiversBeforeDescription(pc);
    if (this.pc !== pc) return;
    const polite = !this.isOfferer;
    const readyForOffer =
      !this.makingOffer &&
      (pc.signalingState === "stable" || this.isSettingRemoteAnswerPending);
    const ignoreOffer = !polite && !readyForOffer;
    if (ignoreOffer) return;
    try {
      await pc.setRemoteDescription({ type: "offer", sdp });
      this.sealSendersOfTransceivers(pc);
      // The peer turning its camera on mid-call is a NEW video transceiver,
      // created by the line above, and a fresh transceiver starts with the
      // descriptor stopped. Arm it here, between the remote offer and the
      // answer, or the answer omits the extension for exactly the m-line
      // that carries the peer's camera INTO this browser -- and a browser
      // that has no descriptor to read assembles no frames at all from a
      // sealed payload (measured: packets arrive, `framesReceived` stays 0).
      // The other three sites arm the m-lines this browser SENDS on; this
      // one is the only site that arms the one it RECEIVES on.
      this.armFrameDescriptor(pc);
      this.coverSFrame();
      await pc.setLocalDescription(); // implicit createAnswer
      const answerSdp = pc.localDescription?.sdp;
      if (answerSdp === undefined) return;
      const wasm = await loadWasm();
      const bytes = wasm.composeCallAnswerSignaling(this.callId, this.outgoingSdp(answerSdp));
      this.callbacks.onOutboundSignaling(bytes, "none");
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("renegotiation offer apply failed", err);
    }
  }

  /** The window from the answer to a connected transport, on either side.
   *  Expiry is `error` (`Call failed`): the call was answered, so `No answer`
   *  would be a lie. */
  private armConnectTimer(): void {
    this.armPhaseTimer(this.timing.connectTimeoutMs, () => {
      if (this.state === "ringing" || this.state === "connecting") {
        void this.endWithTransport(
          `no connected transport within ${this.timing.connectTimeoutMs} ms`,
          () => this.state === "ringing" || this.state === "connecting",
        );
      }
    });
  }

  private armPhaseTimer(ms: number, onExpire: () => void): void {
    this.clearPhaseTimer();
    this.phaseTimer = setTimeout(() => {
      this.phaseTimer = null;
      onExpire();
    }, ms);
  }

  private clearPhaseTimer(): void {
    if (this.phaseTimer !== null) {
      clearTimeout(this.phaseTimer);
      this.phaseTimer = null;
    }
  }

  private clearIceGraceTimer(): void {
    if (this.iceGraceTimer !== null) {
      clearTimeout(this.iceGraceTimer);
      this.iceGraceTimer = null;
    }
  }

  /**
   * End the call from this side.
   *
   * `cause` is not decoration. A call that ends with `error` renders as
   * "Last call ended (error)." and nothing else, and three of the paths that
   * reach here used to be silent - the connect timeout, the ICE grace expiry
   * and an outright ICE failure - so a web-to-iOS call that never connected
   * produced an empty console and no way to tell those three apart. Every
   * caller passing `error` names its cause, and this logs it once here rather
   * than at each site.
   */
  /** The call ends before anything of it was sent: no End on the wire, which
   *  would wake the other person's devices for a call that never rang. */
  private endBeforeOffer(): void {
    if (this.state === "ending" || this.state === "done") return;
    this.placedNowhere = true;
    this.clearPhaseTimer();
    this.clearIceGraceTimer();
    this.setState("ending", "error");
    this.dispose();
    this.setState("done", "error");
  }

  private async endLocal(reason: WebCallEndReason, cause?: string): Promise<void> {
    // `ending` too: a timer firing while a hang-up is already publishing its
    // End must not publish a second one.
    if (this.state === "ending" || this.state === "done") return;
    // eslint-disable-next-line no-console
    console.warn(
      `call ending locally: reason=${reason}, state=${this.state}` +
        (cause === undefined ? "" : `, cause=${cause}`),
    );
    this.clearPhaseTimer();
    this.clearIceGraceTimer();
    this.setState("ending", reason);
    try {
      const wasm = await loadWasm();
      const bytes = wasm.composeCallEndSignaling(this.callId, reason);
      // An End for a call that never connected here stops a ring on the other
      // side's devices, including ones whose app is closed: it wakes. An End
      // for a connected call reaches a peer that is in the call: silent.
      this.callbacks.onOutboundSignaling(bytes, this.everConnected ? "none" : "call");
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("compose end signaling failed", err);
    }
    this.dispose();
    this.setState("done", reason);
  }

  /**
   * Become `connected` if the transport already is.
   *
   * This asks the peer connection rather than waiting to be told, and that is
   * the whole point. It used to live inside `oniceconnectionstatechange` and
   * promote only when the session was already `connecting` - but on the
   * offerer that state is set AFTER two awaits (`setRemoteDescription` for the
   * answer, then the queued candidates), and ICE can reach `connected` inside
   * that window. The event then arrived while the session still read
   * `outgoing`, the promotion was skipped, and no further ICE transition was
   * ever coming: the call sat in `connecting` over a working transport until
   * the connect timer killed it as a failure. Seen live on a web-to-iOS call
   * on 2026-09-22, where the diagnostic read `ice=connected conn=connected
   * pairs=[succeeded:1]` at the moment it gave up. Calling this after every
   * transition into `connecting` as well closes the window, and it is
   * idempotent, so calling it twice costs nothing.
   */
  private promoteIfTransportUp(): void {
    const pc = this.pc;
    if (pc === null) return;
    const s = pc.iceConnectionState;
    if (s !== "connected" && s !== "completed") return;
    this.clearIceGraceTimer();
    if (this.state !== "connecting") return;
    this.clearPhaseTimer();
    this.setState("connected");
  }

  /**
   * End with `error`, naming the transport as it was before the teardown.
   *
   * Describing the transport awaits `getStats()`, and the call goes on
   * living during it: it can connect, recover, or receive media again.
   * `stillHolds` is the condition that brought the caller here, asked again
   * after the await; a call for which it no longer holds is not ended.
   */
  private async endWithTransport(what: string, stillHolds: () => boolean): Promise<void> {
    const transport = await this.describeTransport();
    if (!stillHolds()) return;
    await this.endLocal("error", `${what}; ${transport}`);
  }

  /**
   * What the transport looked like at the moment a call gave up, as one line.
   *
   * The counts are what separate the failure modes that all surface as "the
   * call did not connect": no remote candidates at all means signalling never
   * delivered them; remote candidates present with no succeeded pair means
   * connectivity checks failed, which is where a missing TURN relay or a
   * candidate the other stack cannot resolve shows up. Read before ending,
   * because ending disposes the peer connection.
   */
  private async describeTransport(): Promise<string> {
    const pc = this.pc;
    if (pc === null) return "no peer connection";
    const head =
      `ice=${pc.iceConnectionState} conn=${pc.connectionState} ` +
      `gathering=${pc.iceGatheringState} signaling=${pc.signalingState}`;
    try {
      const report = await pc.getStats();
      const local = new Map<string, number>();
      const remote = new Map<string, number>();
      const pairs = new Map<string, number>();
      let inPackets = 0;
      let outPackets = 0;
      let inBytes = 0;
      let outBytes = 0;
      const num = (v: unknown): number => (typeof v === "number" ? v : 0);
      const bump = (m: Map<string, number>, k: string): void => {
        m.set(k, (m.get(k) ?? 0) + 1);
      };
      report.forEach((stat: Record<string, unknown>) => {
        const type = stat["type"];
        if (type === "local-candidate") {
          bump(local, String(stat["candidateType"] ?? "?"));
        } else if (type === "remote-candidate") {
          bump(remote, String(stat["candidateType"] ?? "?"));
        } else if (type === "candidate-pair") {
          bump(pairs, String(stat["state"] ?? "?"));
        } else if (type === "inbound-rtp") {
          inPackets += num(stat["packetsReceived"]);
          inBytes += num(stat["bytesReceived"]);
        } else if (type === "outbound-rtp") {
          outPackets += num(stat["packetsSent"]);
          outBytes += num(stat["bytesSent"]);
        }
      });
      const fmt = (m: Map<string, number>): string =>
        m.size === 0 ? "none" : [...m].map(([k, n]) => `${k}:${n}`).join(",");
      // The SSRC the peer SAID it would send on. With BUNDLE, a peer that
      // sends on a different one cannot be demuxed and its packets are
      // dropped before any inbound stream exists - which looks exactly like
      // receiving nothing, so the expected value belongs in the same line.
      const sdp = pc.currentRemoteDescription?.sdp ?? "";
      const expect =
        sdp
          .split("\r\n")
          .find((l) => l.startsWith("a=ssrc:"))
          ?.slice("a=ssrc:".length)
          .split(" ")[0] ?? "none";
      return (
        `${head} local=[${fmt(local)}] remote=[${fmt(remote)}] pairs=[${fmt(pairs)}] ` +
        `rtpIn=${inPackets}pkt/${inBytes}B rtpOut=${outPackets}pkt/${outBytes}B ` +
        `peerSsrcSignalled=${expect}`
      );
    } catch (err) {
      return `${head} stats unavailable: ${String(err)}`;
    }
  }

  private endRemote(reason: WebCallEndReason): void {
    if (this.state === "idle" || this.state === "done") return;
    this.dispose();
    this.setState("done", reason);
  }

  private setState(state: WebCallState, reason?: WebCallEndReason): void {
    this.state = state;
    // G3-3-conn-quality — arm the stats poll loop once, on the
    // first transition into `connected`.
    if (state === "connected") {
      this.everConnected = true;
      this.startStatsPolling();
    }
    this.callbacks.onStateChange(state, reason);
  }

  /**
   * G3-3-conn-quality — poll `getStats()` every 2 s while
   * connected and push a derived quality sample to the caller.
   * The poll runs whether or not the caller wired an
   * `onConnectionStats` handler: it used to return early without
   * one, and since the media watchdog (`media-watchdog.ts`) reads
   * the same samples, skipping the poll would silently skip the
   * one thing that ends a call whose peer has gone. Only the
   * handler call is conditional. Cleared in `dispose()`.
   */
  private startStatsPolling(): void {
    if (this.statsTimer !== null) return;
    const onStats = this.callbacks.onConnectionStats;
    this.statsTimer = setInterval(() => {
      void this.sampleConnectionStats().then((sample) => {
        if (sample !== null && onStats !== undefined) onStats(sample);
        this.endIfMediaHasGoneSilent();
      });
    }, 2000);
  }

  /**
   * End a connected call that has received nothing for
   * {@link CallSessionTiming.mediaSilenceTimeoutMs}.
   *
   * Only while `connected`: a call still being set up has not received
   * anything YET, which is a different thing from having stopped.
   */
  private endIfMediaHasGoneSilent(): void {
    if (!this.mediaHasGoneSilent()) return;
    void this.endWithTransport(
      `nothing received for ${this.timing.mediaSilenceTimeoutMs} ms`,
      () => this.mediaHasGoneSilent(),
    );
  }

  /** A connected call whose inbound media has been silent for the whole of
   *  {@link CallSessionTiming.mediaSilenceTimeoutMs}, as last sampled. */
  private mediaHasGoneSilent(): boolean {
    return (
      this.state === "connected" &&
      inboundHasGoneSilent(
        this.inboundActivity,
        performance.now(),
        this.timing.mediaSilenceTimeoutMs,
      )
    );
  }

  /**
   * Read the active ICE candidate-pair out of `getStats()` and
   * derive a [`WebConnectionStats`]. Prefers the nominated
   * succeeded pair, falling back to any succeeded pair (some
   * stacks don't flag `nominated`). Down/up bitrate is the
   * candidate-pair byte delta against a monotonic clock
   * (`performance.now()`), in bits per second; `null` until a
   * second sample lands. Deltas are clamped `>= 0` to tolerate
   * counter resets on renegotiation.
   */
  private async sampleConnectionStats(): Promise<WebConnectionStats | null> {
    const pc = this.pc;
    if (pc === null) return null;
    let report: RTCStatsReport;
    try {
      report = await pc.getStats();
    } catch {
      return null;
    }
    // Ended while the report was being read: no sample for a call that is
    // over (the caller's stats handler belongs to whatever call is current).
    if (this.pc !== pc) return null;
    const byId = new Map<string, unknown>();
    const pairs: unknown[] = [];
    // The watchdog's meter is INBOUND RTP, not the candidate pair. The pair's
    // byte counters include ICE consent checks, which a peer's ICE agent keeps
    // answering for as long as its process is alive whether or not there is a
    // call left: metering the pair would have watched the 45-minute call climb
    // and called it healthy.
    let mediaPackets = 0;
    const entries: Record<string, unknown>[] = [];
    report.forEach((value: unknown, id: string) => {
      byId.set(id, value);
      if (typeof value === "object" && value !== null) entries.push(value as Record<string, unknown>);
      const type = statString(value, "type");
      if (type === "candidate-pair") pairs.push(value);
      if (type === "inbound-rtp") mediaPackets += statNumber(value, "packetsReceived") ?? 0;
    });
    this.inboundActivity = noteInboundMedia(
      this.inboundActivity,
      mediaPackets,
      performance.now(),
    );
    for (const line of this.inboundCounters.interval(entries)) {
      // eslint-disable-next-line no-console
      console.info(line);
    }
    const pathLine = this.callPathLog.note(entries);
    if (pathLine !== null) {
      // eslint-disable-next-line no-console
      console.info(pathLine);
    }
    const succeeded = (s: unknown): boolean =>
      statString(s, "state") === "succeeded";
    const isNominated = (s: unknown): boolean => {
      if (typeof s !== "object" || s === null) return false;
      return (s as Record<string, unknown>)["nominated"] === true;
    };
    const active =
      pairs.find((s) => succeeded(s) && isNominated(s)) ?? pairs.find(succeeded);
    if (active === undefined) {
      this.prevStatsBytes = null;
      return { rttMs: null, relayed: false, downBps: null, upBps: null };
    }
    const rttSec = statNumber(active, "currentRoundTripTime");
    const rttMs = rttSec === null ? null : Math.round(rttSec * 1000);
    const candType = (id: unknown): string | null =>
      typeof id === "string" ? statString(byId.get(id), "candidateType") : null;
    const relayed =
      candType(statString(active, "localCandidateId")) === "relay" ||
      candType(statString(active, "remoteCandidateId")) === "relay";
    const received = statNumber(active, "bytesReceived") ?? 0;
    const sent = statNumber(active, "bytesSent") ?? 0;
    const nowMs = performance.now();
    let downBps: number | null = null;
    let upBps: number | null = null;
    const prev = this.prevStatsBytes;
    if (prev !== null) {
      const dt = (nowMs - prev.atMs) / 1000;
      if (dt > 0.1) {
        downBps = Math.round((Math.max(0, received - prev.received) * 8) / dt);
        upBps = Math.round((Math.max(0, sent - prev.sent) * 8) / dt);
      }
    }
    this.prevStatsBytes = { received, sent, atMs: nowMs };
    return { rttMs, relayed, downBps, upBps };
  }
}
