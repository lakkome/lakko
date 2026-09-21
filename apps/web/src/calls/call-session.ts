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
import {
  armGenericFrameDescriptor,
  type HeaderExtensionTransceiver,
} from "./frame-descriptor";
import {
  attachReceiverSFrame,
  attachSenderSFrame,
  isSFrameTransformSupported,
} from "./sframe-transform";

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
    opts: { readonly seed: Uint8Array; readonly localLeaf: number },
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
import { loadWasm } from "../wasm";
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
   *  pipeline. */
  onOutboundSignaling(plaintextBytes: Uint8Array): void;
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
    return { deviceId: { exact: deviceId } };
  }
  return true;
}

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
   *  call as `timedOut` (`No answer`). */
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
}

/** Default {@link CallSessionTiming}. Chosen, not measured: 45 s is the
 *  order of a phone's ring before voicemail; 30 s is several times what a
 *  direct connection needs, so a call that has not connected by then is not
 *  going to; 10 s is long enough to ride out a roam and short enough that a
 *  dead call does not look alive. */
export const DEFAULT_CALL_SESSION_TIMING: CallSessionTiming = {
  ringTimeoutMs: 45_000,
  connectTimeoutMs: 30_000,
  iceDisconnectedGraceMs: 10_000,
};

/** Public STUN fallback. Replaced by self-hosted in Phase
 *  3a2. */
const PUBLIC_ICE_SERVERS: RTCIceServer[] = [
  { urls: "stun:stun.l.google.com:19302" },
  { urls: "stun:stun1.l.google.com:19302" },
];

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
  /** The per-call SFrame seed and this client's MLS leaf, armed by
   *  {@link setCallKey} before the peer connection opens. `null` means the
   *  call refuses rather than sending anything unsealed (invariant #4). */
  private callKey: { readonly seed: Uint8Array; readonly localLeaf: number } | null = null;

  /** Logged once per session; see {@link armFrameDescriptor}. */
  private frameDescriptorWarned = false;

  /** In-flight guard for {@link addLocalVideo} (no double-acquire). */
  private addingVideo = false;
  /** Mid-call camera-on lifecycle (piece B). */
  private videoPublishState: VideoPublishState = "off";

  /** G3-3-conn-quality — `getStats()` poll loop handle, armed on
   *  the first `connected` transition, cleared in `dispose()`. */
  private statsTimer: ReturnType<typeof setInterval> | null = null;
  /** Previous candidate-pair byte counters + monotonic timestamp
   *  (ms) used to derive the down/up bitrate. */
  private prevStatsBytes: {
    received: number;
    sent: number;
    atMs: number;
  } | null = null;

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

  constructor(
    callId: Uint8Array,
    private readonly callbacks: CallSessionCallbacks,
    timing: Partial<CallSessionTiming> = {},
    private readonly sframe: SFrameAttachers = DEFAULT_SFRAME_ATTACHERS,
  ) {
    this.callId = callId;
    this.timing = { ...DEFAULT_CALL_SESSION_TIMING, ...timing };
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
    if (this.pc === null) throw new Error("peer connection not initialised");
    this.armFrameDescriptor(this.pc);
    const offer = await this.pc.createOffer();
    await this.pc.setLocalDescription(offer);
    if (offer.sdp === undefined) {
      throw new Error("createOffer produced no SDP body");
    }
    const wasm = await loadWasm();
    const bytes = wasm.composeCallOfferSignaling(
      this.callId,
      offer.sdp,
      BigInt(Date.now()),
      media.audio,
      media.video,
    );
    this.callbacks.onOutboundSignaling(bytes);
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
          try {
            await this.pc.setRemoteDescription({
              type: "answer",
              sdp: parsed.sdp,
            });
            await this.applyQueuedCandidates();
            this.setState("connecting");
            // Answered: the ring clock is over, the connect clock starts now.
            this.armConnectTimer();
            // Arm onnegotiationneeded for future local track changes (B2b).
            this.negotiationReady = true;
          } catch (err) {
            // eslint-disable-next-line no-console
            console.warn("setRemoteDescription(answer) failed", err);
            this.endLocal("error");
          }
        } else if (this.state === "connecting" || this.state === "connected") {
          // Mid-call renegotiation answer (to a reneg offer we sent).
          try {
            this.isSettingRemoteAnswerPending = true;
            await this.pc.setRemoteDescription({
              type: "answer",
              sdp: parsed.sdp,
            });
            this.isSettingRemoteAnswerPending = false;
          } catch (err) {
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
        this.endRemote(parsed.endReason as WebCallEndReason);
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
      await pc.setRemoteDescription({ type: "offer", sdp: offer.sdp });
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
      await pc.setLocalDescription(answer);
      if (answer.sdp === undefined) {
        throw new Error("createAnswer produced no SDP body");
      }
      // Set before the next await, so an ICE `connected` that fires right
      // after the local description applies advances connecting → connected
      // rather than being overwritten back to connecting.
      this.setState("connecting");
      // Arm onnegotiationneeded for future local track changes (B2b): the
      // answer is sent + our local description is set, so the initial
      // negotiation is applied on our side.
      this.negotiationReady = true;
      const wasm = await loadWasm();
      const bytes = wasm.composeCallAnswerSignaling(this.callId, answer.sdp);
      this.callbacks.onOutboundSignaling(bytes);
    } catch (err) {
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
      const midCallKey = this.callKey;
      if (midCallKey !== null) {
        this.sframe.attachSender(sender, {
          seed: midCallKey.seed,
          localLeaf: midCallKey.localLeaf,
        });
      }
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
    // Only meaningful if a sender of this kind is already on the wire.
    const sender = this.pc
      .getSenders()
      .find((s) => s.track?.kind === kind);
    if (sender === undefined) return;

    const constraints: MediaStreamConstraints =
      kind === "audio"
        ? { audio: { deviceId: { exact: deviceId } } }
        : { video: { deviceId: { exact: deviceId } } };

    let captured: MediaStream;
    try {
      captured = await navigator.mediaDevices.getUserMedia(constraints);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`getUserMedia (${kind} device switch) failed`, err);
      return;
    }
    const oldTracks =
      kind === "audio"
        ? this.localStream.getAudioTracks()
        : this.localStream.getVideoTracks();
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

    // Swap the track inside the local stream so the self-preview rebinds.
    for (const t of oldTracks) {
      this.localStream.removeTrack(t);
      t.stop();
    }
    this.localStream.addTrack(newTrack);
    this.callbacks.onLocalStream?.(this.localStream);
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
        "CallSession: this browser has no RTCRtpScriptTransform — refusing the call (invariant #4)",
      );
      void this.endLocal("error");
      throw new Error("SFrame transform unsupported");
    }
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
      // eslint-disable-next-line no-console
      console.warn("getUserMedia denied / failed", err);
      void this.endLocal("error");
      throw err;
    }
    // The call ended while the capture (and its permission prompt) was in
    // flight. Release the microphone / camera at once rather than attaching
    // it to a peer connection nobody will ever close.
    if (this.state === "ending" || this.state === "done") {
      for (const track of stream.getTracks()) track.stop();
      return;
    }
    this.localStream = stream;
    // Surface the local capture so the UI can render a self-preview
    // (the video track, if any). Audio-only callers ignore it.
    this.callbacks.onLocalStream?.(stream);

    const pc = new RTCPeerConnection({
      iceServers: PUBLIC_ICE_SERVERS,
      // P2P 1×1 uses native DTLS-SRTP. SFrame insertable
      // streams land in 3b when an SFU sits between peers.
      bundlePolicy: "max-bundle",
      rtcpMuxPolicy: "require",
    });
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
      this.sframe.attachSender(sender, { seed: key.seed, localLeaf: key.localLeaf });
    }

    pc.onicecandidate = (ev): void => {
      if (ev.candidate === null) return;
      const c = ev.candidate;
      if (c.candidate.length === 0) return;
      // Trickled candidates flow out through signaling.
      void (async (): Promise<void> => {
        const wasm = await loadWasm();
        const bytes = wasm.composeCallIceCandidateSignaling(
          this.callId,
          c.candidate,
          c.sdpMid ?? "",
          c.sdpMLineIndex ?? 0,
        );
        this.callbacks.onOutboundSignaling(bytes);
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
      this.sframe.attachReceiver(ev.receiver, { seed: key.seed });
      const remote = ev.streams[0];
      if (remote !== undefined) {
        this.callbacks.onRemoteStream(remote);
      }
    };

    pc.oniceconnectionstatechange = (): void => {
      // A late event from a peer connection this session already closed.
      if (this.pc !== pc) return;
      const s = pc.iceConnectionState;
      if (s === "connected" || s === "completed") {
        this.clearIceGraceTimer();
        if (this.state === "connecting") {
          this.clearPhaseTimer();
          this.setState("connected");
        }
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
              void this.endLocal("error");
            }
          }, this.timing.iceDisconnectedGraceMs);
        }
      } else if (s === "failed") {
        void this.endLocal("error");
      }
    };
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
      await pc.setLocalDescription(); // implicit createOffer
      const sdp = pc.localDescription?.sdp;
      if (sdp === undefined) return;
      const wasm = await loadWasm();
      const bytes = wasm.composeCallOfferSignaling(
        this.callId,
        sdp,
        BigInt(Date.now()),
        true,
        true,
      );
      this.callbacks.onOutboundSignaling(bytes);
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
    const polite = !this.isOfferer;
    const readyForOffer =
      !this.makingOffer &&
      (pc.signalingState === "stable" || this.isSettingRemoteAnswerPending);
    const ignoreOffer = !polite && !readyForOffer;
    if (ignoreOffer) return;
    try {
      await pc.setRemoteDescription({ type: "offer", sdp });
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
      await pc.setLocalDescription(); // implicit createAnswer
      const answerSdp = pc.localDescription?.sdp;
      if (answerSdp === undefined) return;
      const wasm = await loadWasm();
      const bytes = wasm.composeCallAnswerSignaling(this.callId, answerSdp);
      this.callbacks.onOutboundSignaling(bytes);
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
        void this.endLocal("error");
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

  private async endLocal(reason: WebCallEndReason): Promise<void> {
    // `ending` too: a timer firing while a hang-up is already publishing its
    // End must not publish a second one.
    if (this.state === "ending" || this.state === "done") return;
    this.clearPhaseTimer();
    this.clearIceGraceTimer();
    this.setState("ending", reason);
    try {
      const wasm = await loadWasm();
      const bytes = wasm.composeCallEndSignaling(this.callId, reason);
      this.callbacks.onOutboundSignaling(bytes);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("compose end signaling failed", err);
    }
    this.dispose();
    this.setState("done", reason);
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
      this.startStatsPolling();
    }
    this.callbacks.onStateChange(state, reason);
  }

  /**
   * G3-3-conn-quality — poll `getStats()` every 2 s while
   * connected and push a derived quality sample to the caller.
   * No-op when the caller wired no `onConnectionStats` handler
   * (the poll never starts, so there's zero cost for callers
   * that don't render a quality indicator). Cleared in
   * `dispose()`.
   */
  private startStatsPolling(): void {
    if (this.statsTimer !== null) return;
    const onStats = this.callbacks.onConnectionStats;
    if (onStats === undefined) return;
    this.statsTimer = setInterval(() => {
      void this.sampleConnectionStats().then((sample) => {
        if (sample !== null) onStats(sample);
      });
    }, 2000);
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
    const byId = new Map<string, unknown>();
    const pairs: unknown[] = [];
    report.forEach((value: unknown, id: string) => {
      byId.set(id, value);
      if (statString(value, "type") === "candidate-pair") pairs.push(value);
    });
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
