// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Multi-party SFU-backed call session on the web client
 * (SFU multi-party arc, web).
 *
 * Where the 1×1 `call-session.ts` connects two browsers peer-to-peer
 * over native DTLS-SRTP (no SFU, no SFrame), THIS session connects a
 * browser to the Rust SFU and runs SFrame end-to-end media encryption
 * on top — because once a server sits between the endpoints, the
 * media MUST be encrypted above the transport (the SFU only ever sees
 * SFrame ciphertext, security invariant #2).
 *
 * It composes the three pieces built earlier in the arc:
 *
 *   * `SfuSignalingClient` — the `lakko-sfu-v1` WebSocket (typed
 *     server / client frames through `lakko-sfu-proto`).
 *   * The call-admission JWT (minted by `CallJwtClient.mint`) — passed
 *     in as a string; the SFU validates it at the WS upgrade.
 *   * The SFrame transform worker (`sframe-transform.ts`) — every
 *     local sender is `attachSenderSFrame`'d, every remote receiver is
 *     `attachReceiverSFrame`'d.
 *
 * ## Signaling flow
 *
 * The client owns ONE `RTCPeerConnection` to the SFU. Two SDP
 * directions multiplex over it:
 *
 *   * **Publish** (client-initiated, once at connect): the client
 *     `createOffer()`s with its local mic / camera tracks, sets it
 *     local, and `sendSdpOffer`s; the SFU replies with an
 *     `SdpAnswer`.
 *   * **Subscribe** (SFU-initiated, ongoing): when another participant
 *     publishes, the SFU `drive_renegotiations` dispatches an
 *     `SdpOffer` to this client; the client answers
 *     (`createAnswer` → `setLocalDescription` → `sendSdpAnswer`) and
 *     the new remote track surfaces via `ontrack`.
 *
 * Server-initiated offers are processed through a serialized queue so
 * two `setRemoteDescription(offer)` calls never overlap.
 *
 * ## Scope / deferred
 *
 * One publish offer at connect; SFU-driven subscribe renegotiation;
 * client-initiated renegotiation when the camera is turned on mid-call;
 * SFrame on every track; trickle ICE both ways. The camera is published as
 * three ordinary senders, a main one, a light one and a middle one (ADR-0070
 * steps 1 and 5); every offer this client makes names the `mid`s of the last
 * two to the SFU. The middle one sends only from three people in the call
 * on. The session also tells the SFU which picture of each person this
 * device shows (`video-wants.ts`), so the SFU forwards a heavy track only
 * where it is shown.
 * Deferred (a follow-up, not a security gap):
 *   * Per-remote connection-quality stats.
 *
 * Mapping a remote `ontrack` to its source participant IS resolved:
 * the SFU pins the downstream MSID stream-id to the source peer id
 * (`services/sfu/src/media.rs` `add_media(.., Some(source_peer), ..)`),
 * and `PeerId Display == the wire peer_id`, so the `streamId` this
 * session surfaces is exactly that peer id. `sfu-call-ui.ts` keys the
 * roster tile off it (`peer-<streamId>`).
 *
 * Security invariant #4 (no non-E2EE fallback): if the browser can hand
 * encoded frames to the SFrame worker by neither `RTCRtpScriptTransform`
 * nor `createEncodedStreams()` (`./sframe-transport.ts`), `connect()`
 * throws BEFORE any media is captured: the call never opens unencrypted.
 * On the `createEncodedStreams()` path the peer connection is built for it
 * and every sender and receiver has its streams taken (`coverSFrame`).
 */

import {
  armGenericFrameDescriptor,
  type HeaderExtensionTransceiver,
} from "./frame-descriptor";
import {
  SfuSignalingClient,
  type SfuErrorFrame,
  type SfuPeerJoined,
  type SfuPeerLeft,
  type SfuPeerSnapshot,
  type SfuSdpAnswer,
  type SfuSdpOffer,
  type SfuSignalRelay,
  type SfuWelcome,
} from "./sfu-signaling-client";
import { loadWasm } from "../wasm";
import { describeLocalCamera } from "./local-preview";
import {
  attachReceiverSFrame,
  attachSenderSFrame,
  coverEncodedStreams,
  syncSenderCodecs,
  rekeySFrame,
  isSFrameTransformSupported,
  SFRAME_STREAM_CAMERA_LIGHT,
  SFRAME_STREAM_CAMERA_MIDDLE,
  type SFrameStreamIndex,
} from "./sframe-transform";
import {
  sframePeerConnectionConfig,
  sframeTransport,
  sframeTransportLine,
} from "./sframe-transport";
import {
  audioTrackConstraint,
  CAMERA_CAPTURE_SIZE,
  trackConstraint,
  type VideoPublishState,
} from "./call-session";
import { captureWithFallback, type CaptureFallback } from "./capture-fallback";
import { SFU_KEEPALIVE_TICK_MS, sfuKeepaliveAction } from "./sfu-keepalive";
import {
  addCameraSenderMidCall,
  addCopySender,
  addGroupCallSender,
  CAMERA_COPY_KINDS,
  configureCopySender,
  removeCopySender,
  type CameraCopyKind,
  type CopySender,
} from "./camera-sender";
import {
  CAMERA_SENDER_KINDS,
  describeVideoNeededChange,
  neededWord,
  SenderActivity,
  senderActive,
  sendersActive,
  type CameraSenderKind,
  type SenderActivityNote,
  type VideoNeeded,
} from "./sender-activity";
import {
  deltaOf,
  describeDeltas,
  describeOutboundSenders,
  describeGivenChanges,
  describeSender,
  describeShownPeers,
  readMedia,
  readSender,
  type MediaReading,
  type SenderReading,
} from "./media-counters";
import { shownLayerLedger } from "./layer-display";
import { splitStreamId } from "./light-stream";
import { registerVideoFrameSource } from "./video-frame-watch";
import { VideoGivenBook, videoGivenChanges, type VideoGivenEntry } from "./video-given";
import {
  VideoWantsBook,
  VideoWantsSender,
  type VideoQuality,
  type VideoWantsFrame,
} from "./video-wants";

/** Lifecycle phases the SFU session can be in. */
export type SfuCallState =
  | "idle"
  | "connecting"
  | "connected"
  | "failed"
  | "closed";

/** Audio / video capture request. */
export interface SfuCallMedia {
  readonly audio: boolean;
  readonly video: boolean;
  /** Preferred microphone `deviceId` (from the in-call device picker).
   *  `null`/absent → OS default. Honoured at publish; a mid-call change goes
   *  through {@link SfuCallSession.switchAudioInput}. */
  readonly audioDeviceId?: string | null;
  /** Preferred camera `deviceId`. `null`/absent → OS default. */
  readonly videoDeviceId?: string | null;
}

/** Configuration handed to a session at construction. */
export interface SfuCallConfig {
  /** SFU origin, e.g. `wss://sfu.lakko.me` (no trailing path). */
  readonly sfuBaseUrl: string;
  /** Call-admission JWT (JWS Compact) from `CallJwtClient.mint`. */
  readonly jwt: string;
  /**
   * 32-byte per-call SFrame seed — the MLS exporter output
   * `Group.exportSecret("lakko/call/v1", callId, 32)`. Identical for
   * every participant in the same MLS epoch.
   *
   * Optional to support the ADR-0032 multi-joiner path: a link joiner
   * connects signaling-only (`deferPublish`) and does NOT know the seed
   * until it has joined the MLS group via the committer's Welcome. It
   * then calls {@link SfuCallSession.publish} with the derived seed. On
   * the normal group-call path the seed is present at construction.
   */
  readonly seed?: Uint8Array;
  /**
   * This client's MLS leaf index — the KID it stamps into every SFrame
   * it seals. Remote receivers read it off the header to pick the
   * matching per-sender key. Optional for the same reason as
   * {@link seed} (a link joiner learns it only after joining).
   */
  readonly localLeaf?: number;
  /** Mic / camera request. */
  readonly media: SfuCallMedia;
  /**
   * ICE servers (STUN / TURN). Defaults to public STUN; production
   * supplies the self-hosted TURN credentials from
   * `services/auth` `POST /call/turn-credential`.
   */
  readonly iceServers?: readonly RTCIceServer[];
  /**
   * ADR-0032: when `true`, {@link SfuCallSession.connect} opens the SFU
   * signaling socket (so the peer can relay + receive the KeyPackage /
   * Welcome handshake and appear on the roster) but does NOT capture or
   * publish media. The caller drives media on later via
   * {@link SfuCallSession.publish} once it holds a valid seed — the
   * fail-closed "join media only after the Welcome" posture (invariant
   * #4). Defaults to `false` (publish immediately, the normal path).
   */
  readonly deferPublish?: boolean;
  /**
   * Clock the signaling keepalive reads (`sfu-keepalive.ts`). Defaults to
   * `Date.now`; injected by tests so fake timers drive it.
   */
  readonly nowMs?: () => number;
}

/** A remote participant's media surfaced to the caller. */
export interface SfuRemoteTrack {
  /**
   * The MSID stream id the SFU tagged the track with. The SFU pins
   * this to the source peer id (see the module doc), so the caller
   * maps it straight to a participant (`peer-<streamId>`). The light
   * companion track of a publisher's camera arrives under the peer id plus
   * `LIGHT_STREAM_SUFFIX` and its middle track under the peer id plus
   * `MIDDLE_STREAM_SUFFIX`; the caller reads all three with `splitStreamId`
   * (`light-stream.ts`) and never makes a participant of either.
   */
  readonly streamId: string;
  readonly track: MediaStreamTrack;
  readonly stream: MediaStream;
}

// Mid-call camera-on lifecycle (piece B) — `off` / `starting` / `live` /
// `failed`. Defined canonically in `call-session.ts` so the 1×1 and SFU paths
// share ONE type + one videoPublishState surface; re-exported here for existing
// importers (`sfu-call-ui.ts`).
export type { VideoPublishState };

/** Callbacks the caller wires in. */
export interface SfuCallCallbacks {
  /** Lifecycle tick. */
  onStateChange?(state: SfuCallState): void;
  /** Initial roster (from the SFU `Welcome`). */
  onWelcome?(selfPeerId: string, roster: readonly SfuPeerSnapshot[]): void;
  /** A participant joined the call. `peerTag` is the ADR-0021 per-call
   *  tag (absent from a pre-ADR-0021 SFU). */
  onPeerJoined?(
    peerId: string,
    accountPseudonym: string,
    peerTag: string | undefined,
  ): void;
  /** A participant left. */
  onPeerLeft?(peerId: string): void;
  /**
   * What the SFU gives this device of each person's video changed
   * (ADR-0070 step 6, `video-given.ts`): the whole set as it stands, by SFU
   * peer id. A person it does not name is given what is asked for. Empty
   * until an SFU with downlink control on says something, and again once
   * the call is torn down.
   */
  onVideoGiven?(given: ReadonlyMap<string, VideoQuality>): void;
  /** A remote media track arrived (already SFrame-decrypting). */
  onRemoteTrack?(remote: SfuRemoteTrack): void;
  /** The local capture stream is ready (fired once, at publish time). The
   *  UI mirrors this into the local self-preview tile — the group analog of
   *  the 1×1 `onLocalStream`. Present whether the call is audio-only or
   *  video; the tile shows the camera-off placeholder until a video track
   *  is live. */
  onLocalStream?(stream: MediaStream): void;
  /** Mid-call camera-on lifecycle (piece B). Drives the camera button's
   *  spinner + the fail-safe revert in the UI. */
  onVideoPublishState?(state: VideoPublishState): void;
  /** The SFU reported a protocol-level error. */
  onServerError?(frame: SfuErrorFrame): void;
  /**
   * ADR-0032: an opaque peer-to-peer relay payload arrived over the SFU
   * `signal-relay` frame. `from` is the sending peer's SFU peer id;
   * `payload` is the decoded bytes (the SFU never inspects them —
   * invariant #2). The multi-joiner orchestrator classifies these with
   * `dispatchIncomingEnvelope` (CallKeyPackage / CallWelcome / commit).
   *
   * A `MediaState` relay (camera-off-cleanup) is NOT delivered here — the
   * session classifies + consumes it internally and fires
   * {@link onRemoteMediaState} instead. Every other kind falls through to
   * this callback unchanged.
   */
  onSignalRelay?(from: string, payload: Uint8Array): void;
  /**
   * camera-off-cleanup: a peer broadcast its own camera on/off state over the
   * opaque `signal-relay`. `peerId` is the sending peer's SFU peer id (the
   * `peer-<peerId>` roster tile key); `cameraOn` is its new camera state. A
   * mute (`track.enabled=false`) fires no track `ended` event, so without this
   * signal the tile keeps painting a frozen frame — the receiver flips the
   * tile's `camOn` deterministically off it. UI-only (invariant #2: the SFU
   * never parses the payload; a peer only speaks for its own outbound frames).
   */
  onRemoteMediaState?(peerId: string, cameraOn: boolean): void;
  /**
   * The device did not have everything the call asked for, so the person
   * joined with less (`docs/ux/calls.md` section 4.4): the camera failed and
   * only the microphone is live, or neither could be captured and they
   * listen and watch. Fired once, before the offer is built; the UI turns
   * the matching controls off and shows the one-line notice.
   */
  onCaptureFallback?(fallback: CaptureFallback): void;
}

const DEFAULT_ICE_SERVERS: readonly RTCIceServer[] = [
  { urls: "stun:stun.l.google.com:19302" },
  { urls: "stun:stun1.l.google.com:19302" },
];

/**
 * One instance per active SFU call. Construct, then `connect()`. Tear
 * down with `hangUp()` (idempotent).
 */
/** Constant-time-agnostic byte compare — used only to skip a no-op rekey
 *  (identical seeds), never on a secret-vs-attacker path. */
function seedsEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

/** How often the media counters are logged (ADR-0067 step 1). */
const MEDIA_COUNTERS_INTERVAL_MS = 2_000;

/** The SFrame stream index each copy of the camera is sealed under. Stated,
 *  never derived from the track kind: each is a further video sender of the
 *  participant and must not share the camera's key, nor the other's. */
function copyStreamIndex(kind: CameraCopyKind): SFrameStreamIndex {
  return kind === "light" ? SFRAME_STREAM_CAMERA_LIGHT : SFRAME_STREAM_CAMERA_MIDDLE;
}

/** One copy of the camera, as the session keeps it. */
interface CopySlot {
  /** The copy's sender; `null` while there is no camera sender, and when a
   *  browser would not take one. Never more than one per kind. */
  sender: CopySender | null;
  /** The SFrame transform id of {@link sender}. */
  transformId: string | null;
  /** A browser that will not take this copy is named once per call. */
  refusalWarned: boolean;
}

function emptyCopySlot(): CopySlot {
  return { sender: null, transformId: null, refusalWarned: false };
}

export class SfuCallSession {
  private state: SfuCallState = "idle";
  private pc: RTCPeerConnection | null = null;
  private localStream: MediaStream | null = null;
  private signaling: SfuSignalingClient | null = null;
  private selfPeerId: string | null = null;

  /** Serializes SFU-initiated offers so two `setRemoteDescription`
   *  calls never overlap. Each entry is the offer SDP. */
  private offerQueue: string[] = [];
  private drainingOffers = false;

  /** Ids of every attached SFrame transform (senders + receivers), so a
   *  mid-call rekey can target them all (G-18, ADR-0029). */
  private readonly transformIds = new Set<string>();
  /** Said once per call when a sender is found with no SFrame transform. */
  private unsealedSenderWarned = false;
  /** Call-key seed of the CURRENT MLS epoch. Starts at `config.seed` (or
   *  `null` on the ADR-0032 deferred-publish path, set later by
   *  {@link publish}) and advances on `rekey` so a late-arriving receiver
   *  attaches at the current epoch. */
  private activeSeed: Uint8Array | null;
  /** This client's SFrame KID (MLS leaf). From `config.localLeaf`, or set
   *  by {@link publish} on the deferred-publish path. */
  private publishLeaf: number;

  // ---- Perfect-negotiation state (piece B: mid-call add-track) ----------
  // The client is the POLITE peer relative to the SFU: on an offer glare it
  // rolls back its own offer and accepts the SFU's, then `onnegotiationneeded`
  // re-fires to re-send its pending change. `negotiationReady` gates the
  // `onnegotiationneeded` handler off during the manual initial publish (so
  // the first offer is not sent twice); it flips true once the initial offer
  // is answered.
  private readonly polite = true;
  private frameDescriptorWarned = false;
  private endedByServer = false;
  private makingOffer = false;
  private isSettingRemoteAnswerPending = false;
  private negotiationReady = false;
  /** Mid-call camera-on lifecycle. */
  private videoPublishState: VideoPublishState = "off";

  /**
   * The camera's copies (ADR-0070 steps 1 and 5): each a clone of the camera
   * track on a transceiver of its own, sealed under its own SFrame stream
   * index. The light one is a quarter of the size and always sent; the middle
   * one is half the size and sent only while {@link middleWanted}. A copy a
   * browser would not take is absent and the call goes on without it.
   */
  private readonly copies: Record<CameraCopyKind, CopySlot> = {
    light: emptyCopySlot(),
    middle: emptyCopySlot(),
  };
  /**
   * The SFU's last `video-needed` frame (`sender-activity.ts`): which of this
   * device's camera senders somebody is being sent. `null` before the first
   * frame, which means every sender is needed.
   */
  private videoNeeded: VideoNeeded | null = null;
  /**
   * Keeps each camera sender's one encoding active or not, as
   * {@link sendersActiveNow} says. One per sender, so the `setParameters`
   * calls on a sender never overlap (a browser refuses one made while
   * another is pending) and the last decision wins.
   */
  private readonly senderActivity: Record<CameraSenderKind, SenderActivity> = {
    main: new SenderActivity("main"),
    middle: new SenderActivity("middle"),
    light: new SenderActivity("light"),
  };

  /** The shared rules, once loaded (`connect()`). */
  private wasm: Awaited<ReturnType<typeof loadWasm>> | null = null;

  /**
   * What this device shows of each person (`video-wants.ts`): the people of
   * the call as the SFU names them, and the picture every mounted tile
   * reports. Sent to the SFU as a whole whenever it changes.
   */
  private readonly wantsBook = new VideoWantsBook();
  private readonly wantsSender: VideoWantsSender;
  /**
   * What the SFU says it gives of each person (`video-given.ts`). Read by the
   * tiles and the media counters; never fed back into {@link wantsBook}, so
   * the SFU keeps being asked for what the tiles really want.
   */
  private readonly givenBook = new VideoGivenBook();
  /** The publish offer is out: the SFU has media state for this peer, and
   *  takes a `video-wants` frame only from then on. */
  private publishOfferSent = false;

  /**
   * The local camera on/off state, broadcast to peers over the opaque SFU
   * `signal-relay` (camera-off-cleanup). Initialised from the initial media
   * request; updated by every path that flips the local camera
   * ({@link setCameraEnabled} / {@link publishVideo}). Receivers flip the
   * matching roster tile's `camOn` off this signal — a mute
   * (`track.enabled=false`) fires no track event, so the tile would otherwise
   * keep painting a frozen frame. UI-only (invariant #2: the SFU never parses
   * the payload; a peer only ever speaks for its own outbound frames).
   */
  private localCameraOn: boolean;

  /**
   * Signaling keepalive (`sfu-keepalive.ts`): a coarse tick pings the SFU on
   * the interval and fails the call once nothing inbound (Welcome or Pong)
   * has arrived within the deadline, so a dead link ends the call instead of
   * leaving the timer counting on a socket nobody answers.
   */
  private keepaliveTimer: ReturnType<typeof setInterval> | null = null;
  /** ADR-0067 step 1: the media counters poll and its last reading (the
   *  base of the next interval's line and of the total at the end). */
  private mediaCountersTimer: ReturnType<typeof setInterval> | null = null;
  private mediaCountersLast: MediaReading | null = null;
  private senderCountersLast: SenderReading | null = null;
  /** The ids of the remote tracks that are light companions and middle
   *  tracks (`light-stream.ts`), so the media counters can say which receiver
   *  is which. */
  /** The audio receiver of each remote peer, by peer id (`audioReceiverFor`). */
  private readonly audioReceivers = new Map<string, RTCRtpReceiver>();
  private readonly copyTrackIds: Record<CameraCopyKind, Set<string>> = {
    light: new Set<string>(),
    middle: new Set<string>(),
  };
  private lastInboundMs = 0;
  private lastPingMs = 0;
  private pingNonce = 0;
  private readonly nowMs: () => number;

  constructor(
    private readonly config: SfuCallConfig,
    private readonly callbacks: SfuCallCallbacks = {},
  ) {
    this.activeSeed = config.seed ?? null;
    this.publishLeaf = config.localLeaf ?? 0;
    this.localCameraOn = config.media.video;
    this.nowMs = config.nowMs ?? Date.now;
    this.wantsSender = new VideoWantsSender(this.wantsBook, (frame) => this.sendVideoWants(frame), {
      now: () => this.nowMs(),
      setTimeout: (fn, ms) => setTimeout(fn, ms),
      clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    });
  }

  /**
   * A tile says which picture of `peerId` it shows (`video-wants.ts`), or
   * `null` when it stops showing one. `tile` is any object that stays the
   * same for the tile's lifetime. The session asks the SFU for the highest
   * quality any tile of a person wants, `light` for a person no tile shows,
   * and tells it whenever that changes.
   */
  reportTileVideoQuality(tile: object, peerId: string, quality: VideoQuality | null): void {
    if (this.wantsBook.setTile(tile, peerId, quality)) this.wantsSender.changed();
  }

  /**
   * The receiver that decodes `peerId`'s audio, or `undefined` before their
   * audio track arrived and after it ended. The call screen reads the
   * person's sound level from it (`audio-level.ts`), on this device, for the
   * speaking ring and the large tile; nothing about it is sent anywhere.
   */
  audioReceiverFor(peerId: string): RTCRtpReceiver | undefined {
    const receiver = this.audioReceivers.get(peerId);
    if (receiver === undefined) return undefined;
    // A track that ended (the peer left or stopped sending) has no level.
    if (receiver.track?.readyState === "ended") {
      this.audioReceivers.delete(peerId);
      return undefined;
    }
    return receiver;
  }

  /** The picture of `peerId` this device asks the SFU for now. */
  videoQualityOf(peerId: string): VideoQuality {
    return this.wantsBook.qualityOf(peerId);
  }

  /** What the SFU gives of `peerId` now; `undefined` when it has not said
   *  (downlink control off, or a person its last frame did not name). */
  videoGivenOf(peerId: string): VideoQuality | undefined {
    return this.givenBook.givenOf(peerId);
  }

  /**
   * A `video-given` frame: the SFU's whole set replaces the last one. The
   * tiles are told, and one line says whose shown picture it changed. What
   * this device asks for is left as it is.
   */
  private onVideoGivenFrame(entries: readonly VideoGivenEntry[]): void {
    const before = this.givenBook.snapshot();
    this.givenBook.replace(entries);
    const line = describeGivenChanges(
      "call video given",
      videoGivenChanges(
        this.wantsBook.wants(),
        (peerId) => before.get(peerId),
        (peerId) => this.givenBook.givenOf(peerId),
      ),
    );
    if (line !== null) {
      // eslint-disable-next-line no-console
      console.info(line);
    }
    this.callbacks.onVideoGiven?.(this.givenBook.snapshot());
  }

  /** Put one `video-wants` frame on the wire; `false` when it cannot go yet
   *  (the sender then tries again at its next call). */
  private sendVideoWants(frame: VideoWantsFrame): boolean {
    const signaling = this.signaling;
    if (signaling === null || !this.publishOfferSent) return false;
    try {
      signaling.sendVideoWants(frame.peers, frame.qualities);
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Whether the middle sender sends now: the shared rule
   * (`middle_sender_wanted`) over the people in the call, this device's own
   * person included. `false` while the rule cannot be asked.
   */
  private middleWanted(): boolean {
    try {
      return this.wasm?.middleSenderWanted(this.wantsBook.people()) ?? false;
    } catch {
      return false;
    }
  }

  /**
   * The "camera is on" condition of the sender-activity decision: this
   * device has a live camera track.
   *
   * Deliberately NOT `track.enabled`. The person turning the camera off is
   * `track.enabled = false` on the camera and its clones, exactly as before
   * this rule existed: the senders go on sending (black frames, at very
   * little bitrate) and a viewer who joins meanwhile still gets its key
   * frame. The SFU's own bookkeeping for a silent sender (no key frame
   * request used up, no retransmission asked, no burst at its return) covers
   * only a sender it told `false`; a sender this client silenced by itself
   * would be an unannounced stop on the SFU's side. So the person's camera
   * toggle and the SFU's word compose without touching each other: the
   * toggle changes the tracks, the SFU's word changes the encodings, and a
   * camera turned on while the SFU's last word was `false` finds that
   * encoding still inactive and the light one active.
   */
  private cameraIsOn(): boolean {
    const track = this.localStream?.getVideoTracks()[0];
    return track !== undefined && track.readyState === "live";
  }

  /** This device's main camera sender, if it has one. */
  private mainCameraSender(): RTCRtpSender | undefined {
    const pc = this.pc;
    return pc === null ? undefined : this.mainSenderOf(pc, "video");
  }

  /**
   * Which camera senders encode and send now (`sender-activity.ts`): the
   * camera is on, the sender exists, the people count allows the middle one,
   * and the SFU's last word for the sender is not `false`.
   */
  private sendersActiveNow(): Record<CameraSenderKind, boolean> {
    const main = this.mainCameraSender();
    return sendersActive(
      { cameraOn: this.cameraIsOn(), middleWanted: this.middleWanted(), needed: this.videoNeeded },
      (kind) => (kind === "main" ? main !== undefined : this.copies[kind].sender !== null),
    );
  }

  /**
   * Make every camera sender's encoding active or not, as
   * {@link sendersActiveNow} says now. `setParameters` only: the media lines,
   * the SSRCs and the SFrame transforms stay and nothing is renegotiated.
   *
   * Called after everything that can change the answer or reset what the
   * browser holds: a `video-needed` frame, a change of the roster, the
   * camera turned off or on, a sender added, a track replaced (device
   * switch, re-publish after an ended track), a renegotiation, and every
   * counters tick (which is also when a refused switch-on is asked again).
   * A no-op for a sender whose encoding already says so.
   */
  private syncSenderActivity(): void {
    const main = this.mainCameraSender();
    this.senderActivity.main.bind(main ?? null);
    this.senderActivity.middle.bind(this.copies.middle.sender?.sender ?? null);
    this.senderActivity.light.bind(this.copies.light.sender?.sender ?? null);
    const active = this.sendersActiveNow();
    for (const kind of CAMERA_SENDER_KINDS) {
      const activity = this.senderActivity[kind];
      if (activity.bound) activity.want(active[kind]);
    }
  }

  /**
   * A `video-needed` frame: the SFU's whole word replaces the last one, the
   * senders follow at once, and one line says so when it changed which of
   * them is active.
   */
  private onVideoNeededFrame(needed: VideoNeeded): void {
    const before = this.sendersActiveNow();
    this.videoNeeded = needed;
    const line = describeVideoNeededChange(needed, before, this.sendersActiveNow());
    if (line !== null) {
      // eslint-disable-next-line no-console
      console.info(line);
    }
    this.syncSenderActivity();
  }

  /** What the `send video` line says of one sender: the SFU's last word and
   *  whether its encoding is asked to be active. */
  private senderActivityNote(kind: CameraSenderKind): SenderActivityNote {
    return { needed: neededWord(kind, this.videoNeeded), active: this.sendersActiveNow()[kind] };
  }

  /**
   * Rotate the call key on a mid-call MLS epoch change (G-18, ADR-0029). The
   * caller re-derives `newSeed = group.exportSecret("lakko/call/v1", callId,
   * 32)` after processing a commit that advanced the epoch and passes it here;
   * every attached transform retains its previous key for the overlap window
   * so in-flight frames still open, then drops it. Late-arriving receivers
   * attach at the new epoch. No-op if the seed is unchanged.
   */
  rekey(newSeed: Uint8Array): void {
    if (this.activeSeed !== null && seedsEqual(this.activeSeed, newSeed)) return;
    this.activeSeed = newSeed;
    for (const id of this.transformIds) {
      rekeySFrame(id, newSeed);
    }
  }

  /** Current lifecycle phase. */
  getState(): SfuCallState {
    return this.state;
  }

  /** Self peer id assigned by the SFU `Welcome` (null until then). */
  getSelfPeerId(): string | null {
    return this.selfPeerId;
  }

  /**
   * Open the SFU WebSocket, build the peer connection, publish the
   * local tracks (SFrame-sealed), and start subscribing. Resolves once
   * the publish offer has been sent; remote tracks arrive
   * asynchronously through `onRemoteTrack`.
   *
   * Throws (before any media capture) if the browser can hand encoded
   * frames to the SFrame worker by neither interface
   * (`./sframe-transport.ts`) - invariant #4, no non-E2EE fallback.
   */
  async connect(): Promise<void> {
    if (this.state !== "idle") {
      throw new Error("SfuCallSession.connect() called twice");
    }
    if (!isSFrameTransformSupported()) {
      throw new Error(
        "Neither RTCRtpScriptTransform nor createEncodedStreams is available: " +
          "refusing to join the SFU call without SFrame media encryption " +
          "(security invariant #4)",
      );
    }
    // Which of the two interfaces carries this call's frames to the worker.
    // eslint-disable-next-line no-console
    console.info(sframeTransportLine());
    this.setState("connecting");

    // 1. Open signaling first so the PC's trickled ICE + the publish
    //    offer have a live socket to ride.
    this.signaling = new SfuSignalingClient(
      this.config.sfuBaseUrl,
      this.config.jwt,
      {
        onWelcome: (f): void => this.onWelcome(f),
        onPeerJoined: (f): void => this.onPeerJoined(f),
        onPeerLeft: (f): void => this.onPeerLeft(f),
        onSignalRelay: (f): void => this.onSignalRelayFrame(f),
        onSdpAnswer: (f): void => void this.onSdpAnswer(f),
        onSdpOffer: (f): void => this.onSdpOffer(f),
        onVideoGiven: (given): void => this.onVideoGivenFrame(given),
        onVideoNeeded: (needed): void => this.onVideoNeededFrame(needed),
        onPong: (): void => {
          this.lastInboundMs = this.nowMs();
        },
        onServerError: (f): void => {
          if (f.code === "call-ended") {
            // The SFU's answer in place of the Welcome (`sfu-call-state.ts`):
            // the room stood empty past its grace before this device
            // arrived. Not an error of this device's; the host closes the
            // chat's record and says the call ended.
            this.endedByServer = true;
            this.fail("the call ended before this device joined");
            return;
          }
          this.callbacks.onServerError?.(f);
        },
        onClose: (): void => this.onSignalingClosed(),
        onSocketError: (): void => this.onSignalingClosed(),
      },
    );
    try {
      await this.signaling.connect();
    } catch (err) {
      this.fail("signaling connect failed", err);
      throw err;
    }

    // The shared rules (the signaling client has loaded the module already).
    try {
      this.wasm = await loadWasm();
    } catch {
      this.wasm = null;
    }

    // The socket is open and the Welcome has arrived: start the keepalive.
    this.lastInboundMs = this.lastPingMs = this.nowMs();
    this.keepaliveTimer = setInterval(() => this.onKeepaliveTick(), SFU_KEEPALIVE_TICK_MS);

    // 2. Build the peer connection + capture + publish — UNLESS the caller
    //    asked to defer (ADR-0032 link joiner: stay signaling-only until it
    //    holds a Welcome + a derived seed, then call `publish()`).
    if (this.config.deferPublish === true) return;
    try {
      await this.openPeerConnectionAndPublish();
    } catch (err) {
      this.fail("publish failed", err);
      throw err;
    }
  }

  /**
   * ADR-0032: publish local media over the already-open signaling socket,
   * arming SFrame on the supplied (or previously-set) seed + leaf. Used by
   * the deferred-publish path once the client has joined the MLS group and
   * derived the per-call seed — media only ever starts AFTER a valid seed
   * is in hand (invariant #4, no plaintext-media fallback). Idempotent:
   * a second call while a peer connection already exists is a no-op.
   *
   * @throws if no seed is available (never publishes unencrypted).
   */
  async publish(seed?: Uint8Array, localLeaf?: number): Promise<void> {
    if (this.pc !== null) return;
    if (seed !== undefined) this.activeSeed = seed;
    if (localLeaf !== undefined) this.publishLeaf = localLeaf;
    if (this.activeSeed === null) {
      throw new Error(
        "SfuCallSession.publish() called without a seed — refusing to " +
          "publish media without SFrame encryption (security invariant #4)",
      );
    }
    try {
      await this.openPeerConnectionAndPublish();
    } catch (err) {
      this.fail("publish failed", err);
      throw err;
    }
  }

  /** ADR-0032: relay an opaque payload to one peer (the KeyPackage / Welcome
   *  handshake). No-op if signaling is not open. */
  sendSignalRelayToPeer(peerId: string, payload: Uint8Array): void {
    this.signaling?.sendSignalRelayToPeer(peerId, payload);
  }

  /** ADR-0032: relay an opaque payload to every other peer in the call (a
   *  broadcast KeyPackage request or an MLS commit). No-op if not open. */
  sendSignalRelayToCall(payload: Uint8Array): void {
    this.signaling?.sendSignalRelayToCall(payload);
  }

  /** Mute / unmute the local microphone (local-only gate, no
   *  renegotiation). */
  setMicrophoneEnabled(on: boolean): void {
    if (this.localStream === null) return;
    for (const track of this.localStream.getAudioTracks()) {
      track.enabled = on;
    }
  }

  /** Enable / disable the local camera (local-only gate). Also broadcasts the
   *  new camera state to peers (camera-off-cleanup) so their roster tile flips
   *  deterministically — a mute fires no track event. On the web path the
   *  Chat.tsx camera effect routes camera-ON through {@link publishVideo}, so
   *  in practice this is the OFF edge; the state update is symmetric regardless. */
  setCameraEnabled(on: boolean): void {
    if (this.localStream !== null) {
      for (const track of this.localStream.getVideoTracks()) {
        track.enabled = on;
      }
    }
    // The copies follow the camera.
    this.setCopiesEnabled(on);
    this.setLocalCameraOn(on);
    // The toggle is the tracks' `enabled`, as before; the encodings stay as
    // the people count and the SFU's last word have them (asked again here
    // in case this browser reset one).
    this.syncSenderActivity();
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

  /** Record the local camera state and, when it changed, broadcast it to
   *  peers (camera-off-cleanup). */
  private setLocalCameraOn(on: boolean): void {
    if (this.localCameraOn === on) return;
    this.localCameraOn = on;
    this.announceCameraState();
  }

  /**
   * Broadcast the CURRENT local camera state to every other peer over the
   * opaque SFU `signal-relay` (camera-off-cleanup). Best-effort + fire-and-
   * forget: a compose failure or a closed socket is swallowed (the signal is
   * a UI hint, never on the E2EE / media path — invariant #2). No-op when
   * signaling is not open. `composeMediaState` needs the wasm module, which
   * `connect()` has already loaded, so the cached module resolves immediately.
   */
  private announceCameraState(): void {
    if (this.signaling === null) return;
    const on = this.localCameraOn;
    void (async (): Promise<void> => {
      let bytes: Uint8Array;
      try {
        const wasm = await loadWasm();
        bytes = wasm.composeMediaState(on);
      } catch {
        return;
      }
      try {
        // Re-check after the await — the call may have torn down.
        this.signaling?.sendSignalRelayToCall(bytes);
      } catch {
        // socket gone / not open — the signal is best-effort.
      }
    })();
  }

  /**
   * Turn the camera ON mid-call (piece B). When the call started audio-only
   * there is no video sender, so this ACQUIRES the camera, adds the track
   * (SFrame-sealed like the audio sender), and renegotiates via
   * `onnegotiationneeded`. Idempotent + re-entrant-safe:
   *   * a live video sender already exists -> just re-enable its track;
   *   * a capture is already in flight      -> no-op (do not double-acquire).
   * Fail-safe (invariant #4): on a getUserMedia / attach failure the capture
   * is torn down (never a half-added sender) and the state goes `failed`, so
   * the caller reverts the toggle + surfaces a message; the call stays up
   * audio-only.
   *
   * @throws on capture failure (permission denied / no camera / attach error),
   *         so the caller can distinguish it from "already publishing".
   */
  async publishVideo(): Promise<void> {
    const pc = this.pc;
    if (pc === null || this.localStream === null || this.activeSeed === null) {
      return;
    }
    // A live video sender already exists (a call started as video, or a prior
    // publishVideo): just re-enable — no capture, no renegotiation.
    const existing = this.mainSenderOf(pc, "video");
    if (existing?.track != null && existing.track.readyState === "live") {
      existing.track.enabled = true;
      this.setCopiesEnabled(true);
      this.syncSenderActivity();
      this.setVideoPublishState("live");
      // Camera is live again — tell peers (camera-off-cleanup).
      this.setLocalCameraOn(true);
      return;
    }
    // A capture is already in flight — do not double-acquire.
    if (this.videoPublishState === "starting") return;

    this.setVideoPublishState("starting");
    let captured: MediaStream;
    try {
      captured = await navigator.mediaDevices.getUserMedia({
        video: trackConstraint(true, this.config.media.videoDeviceId),
      });
    } catch (err) {
      this.setVideoPublishState("failed");
      throw err instanceof Error ? err : new Error(String(err));
    }
    const track = captured.getVideoTracks()[0];
    if (track === undefined) {
      this.setVideoPublishState("failed");
      throw new Error("no camera track captured");
    }
    try {
      // Re-read state after the await — the call may have been torn down or
      // rekeyed while getUserMedia was in flight; attach at the CURRENT seed.
      const localStream = this.localStream;
      const pc2 = this.pc;
      const seed = this.activeSeed;
      if (localStream === null || pc2 === null || seed === null) {
        track.stop();
        this.setVideoPublishState("off");
        return;
      }
      // The sender of a camera track that ended by itself (device unplugged,
      // permission withdrawn) takes the new track in place: one sender, one
      // SFrame transform and one frame counter per stream, and no
      // renegotiation.
      const ended = this.mainSenderOf(pc2, "video", "ended");
      if (ended?.track != null) {
        const old = ended.track;
        await ended.replaceTrack(track);
        localStream.removeTrack(old);
        localStream.addTrack(track);
        // The copies' clones ended with the camera they were cloned from:
        // each takes a clone of the new track, in place as well.
        await this.followCamera(pc2, track, seed);
        this.setVideoPublishState("live");
        this.setLocalCameraOn(true);
        this.callbacks.onLocalStream?.(localStream);
        return;
      }
      localStream.addTrack(track);
      // The main sender: `addTrack`, which mid-call may reuse a video
      // transceiver the SFU already negotiated; the layers are asked for
      // separately.
      const sender = addCameraSenderMidCall(pc2, track, localStream);
      this.transformIds.add(
        attachSenderSFrame(sender, { seed, localLeaf: this.publishLeaf }),
      );
      // The copies, added and sealed in the same turn as the main one, so
      // the renegotiation `addTrack` asked for finds all of them.
      await this.followCamera(pc2, track, seed);
      if (this.pc !== pc2) {
        // Torn down while the copies were being set up.
        track.stop();
        this.setVideoPublishState("off");
        return;
      }
      // Adding the sender fired onnegotiationneeded -> the SDP renegotiation runs in the
      // background. The self-preview is live now (the local track is live), so
      // report `live`; a background renegotiation failure only affects what
      // REMOTE peers see, not the local preview.
      this.setVideoPublishState("live");
      // Camera is live — tell peers (camera-off-cleanup) so a peer that was
      // shown camera-off flips back on.
      this.setLocalCameraOn(true);
      // Refresh the self-preview stream (it now carries a video track).
      this.callbacks.onLocalStream?.(localStream);
    } catch (err) {
      // Never leave a half-added sender: release the capture.
      track.stop();
      this.setVideoPublishState("failed");
      throw err instanceof Error ? err : new Error(String(err));
    }
  }

  /**
   * Switch the published microphone to a specific `deviceId` mid-call (from
   * the in-call device picker). Re-captures that one kind with the picked
   * device and `replaceTrack`s it on the existing sender. Crucially the SFrame
   * transform lives on the SENDER, not the track, so it survives the
   * replacement — the media stays end-to-end encrypted through the SFU
   * (invariant #4), no re-attach + no renegotiation (same kind). Best-effort:
   * a capture / replace failure keeps the current track; a no-op when there is
   * no live sender of that kind (e.g. switching the camera on an audio-only
   * group call).
   */
  async switchAudioInput(deviceId: string): Promise<void> {
    await this.switchInput("audio", deviceId);
  }

  /** Switch the published camera to a specific `deviceId` mid-call. Same shape
   *  as {@link switchAudioInput}; a no-op while the group call is audio-only. */
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
    const sender = this.mainSenderOf(pc, kind);
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
      console.warn(`getUserMedia (SFU ${kind} device switch) failed`, err);
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
      // replaceTrack keeps the sender's RTCRtpScriptTransform (the SFrame
      // encryptor) bound — no re-attach needed.
      await sender.replaceTrack(newTrack);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn(`replaceTrack (SFU ${kind} device switch) failed`, err);
      newTrack.stop();
      return;
    }

    for (const t of oldTracks) {
      this.localStream.removeTrack(t);
      t.stop();
    }
    this.localStream.addTrack(newTrack);
    // The copies follow the camera to the new device. Their old clones are
    // stopped there; the old camera track was stopped just above. A copy the
    // browser would not take is not tried again here.
    if (kind === "video" && this.pc === pc && this.activeSeed !== null) {
      await this.followCamera(pc, newTrack, this.activeSeed, false);
    }
    // A replaced track keeps the sender and its parameters; asked again all
    // the same, in case this browser reset the encoding.
    if (kind === "video") this.syncSenderActivity();
  }

  /** The copies' clones are enabled and disabled with the camera. */
  private setCopiesEnabled(on: boolean): void {
    for (const kind of CAMERA_COPY_KINDS) {
      const copy = this.copies[kind].sender;
      if (copy !== null) copy.track.enabled = on;
    }
  }

  /**
   * This device's microphone sender or main camera sender: the sender of
   * `kind` that is not one of the camera's copies. With `readyState`, only
   * one whose track is in that state.
   */
  private mainSenderOf(
    pc: RTCPeerConnection,
    kind: "audio" | "video",
    readyState?: MediaStreamTrackState,
  ): RTCRtpSender | undefined {
    const copySenders = CAMERA_COPY_KINDS.map((k) => this.copies[k].sender?.sender);
    return pc
      .getSenders()
      .find(
        (s) =>
          !copySenders.includes(s) &&
          s.track?.kind === kind &&
          (readyState === undefined || s.track.readyState === readyState),
      );
  }

  /**
   * Make every copy of the camera carry a copy of `cameraTrack` (ADR-0070
   * steps 1 and 5).
   *
   * A copy that is not there yet is added (unless `addMissing` is `false`):
   * a clone of the camera on a transceiver of its own, with its size and its
   * bitrate ceiling, sealed under its own SFrame stream index before any
   * offer can be built (security invariant #4). Every missing copy is added
   * and sealed in this same turn, before anything is awaited. A copy already
   * there has its clone replaced in place: same sender, same transform, same
   * frame counter, and no renegotiation. There is never a second one of a
   * kind.
   *
   * Never fails the call. A browser that takes no such sender, takes none
   * with the right size, or cannot seal it leaves the call without that copy,
   * said once.
   */
  private async followCamera(
    pc: RTCPeerConnection,
    cameraTrack: MediaStreamTrack,
    seed: Uint8Array,
    addMissing = true,
  ): Promise<void> {
    await Promise.all(
      CAMERA_COPY_KINDS.map((kind) => this.followCameraWith(kind, pc, cameraTrack, seed, addMissing)),
    );
    // The roster may have crossed three people, or the SFU's word may have
    // changed, while a copy was set up; and a new or re-tracked sender is
    // given its flag again.
    this.syncSenderActivity();
  }

  /** {@link followCamera} for one copy. Runs up to the copy being added and
   *  sealed without awaiting anything. */
  private async followCameraWith(
    kind: CameraCopyKind,
    pc: RTCPeerConnection,
    cameraTrack: MediaStreamTrack,
    seed: Uint8Array,
    addMissing: boolean,
  ): Promise<void> {
    const slot = this.copies[kind];
    const existing = slot.sender;
    if (existing !== null) {
      let clone: MediaStreamTrack;
      try {
        clone = cameraTrack.clone();
      } catch (err) {
        this.dropCopySender(kind, pc, "the camera track cannot be cloned", err);
        return;
      }
      clone.enabled = cameraTrack.enabled;
      const old = existing.track;
      try {
        await existing.sender.replaceTrack(clone);
      } catch (err) {
        clone.stop();
        this.dropCopySender(kind, pc, `the ${kind} sender would not take the new camera`, err);
        return;
      }
      old.stop();
      if (slot.sender !== existing) {
        // Torn down (or removed) while the track was being replaced.
        clone.stop();
        return;
      }
      existing.track = clone;
      return;
    }
    if (!addMissing) return;

    // The middle sender is created sending or not as the roster and the SFU
    // say now; the light one is created sending.
    const active =
      kind === "middle"
        ? senderActive("middle", {
            cameraOn: cameraTrack.readyState === "live",
            exists: true,
            middleWanted: this.middleWanted(),
            needed: this.videoNeeded,
          })
        : undefined;
    const added = addCopySender(
      pc,
      cameraTrack,
      kind,
      (what, err) => this.warnNoCopySender(kind, what, err),
      active,
    );
    if (added === null) return;
    const copy = added.copy;
    copy.track.enabled = cameraTrack.enabled;
    slot.sender = copy;
    try {
      slot.transformId = attachSenderSFrame(copy.sender, {
        seed,
        localLeaf: this.publishLeaf,
        streamIndex: copyStreamIndex(kind),
      });
      this.transformIds.add(slot.transformId);
    } catch (err) {
      // An unsealed sender is never left in place (invariant #4).
      this.dropCopySender(kind, pc, "its SFrame transform could not be attached", err);
      return;
    }
    if (!added.configured) {
      // The browser refused the size and the ceiling with the transceiver;
      // ask again on the sender. Without them this would be another
      // full-size camera, so a refusal here removes the sender.
      const ok = await configureCopySender(copy.sender, kind, active);
      if (slot.sender !== copy) return;
      if (!ok) {
        this.dropCopySender(kind, pc, "its size and bitrate ceiling were refused", undefined);
      }
    }
  }

  /** Remove a copy's sender and say why, once per call. */
  private dropCopySender(kind: CameraCopyKind, pc: RTCPeerConnection, what: string, err: unknown): void {
    this.warnNoCopySender(kind, what, err);
    const slot = this.copies[kind];
    const copy = slot.sender;
    if (copy === null) return;
    slot.sender = null;
    if (slot.transformId !== null) this.transformIds.delete(slot.transformId);
    slot.transformId = null;
    removeCopySender(pc, copy);
  }

  private warnNoCopySender(kind: CameraCopyKind, what: string, err: unknown): void {
    const slot = this.copies[kind];
    if (slot.refusalWarned) return;
    slot.refusalWarned = true;
    // eslint-disable-next-line no-console
    console.warn(`SfuCallSession: publishing the camera without a ${kind} sender: ${what}`, err ?? "");
  }

  /**
   * The `mid`s of the offer's m-lines that carry the `kind` copy of the
   * camera: one while there is such a sender with a `mid` (it has one once a
   * local description that includes it is set), none otherwise. Sent with
   * every offer this client makes, so the SFU forwards that m-line as the
   * camera's light companion, or as its middle track.
   */
  private copyMids(kind: CameraCopyKind): string[] {
    const mid = this.copies[kind].sender?.transceiver.mid;
    return typeof mid === "string" && mid.length > 0 ? [mid] : [];
  }

  /**
   * The stats ids of the `outbound-rtp` entries of the `kind` copy's sender,
   * read from the sender's own report, so the media counters can name each
   * camera sender. Empty without that sender or when the browser does not
   * answer.
   */
  private async copyOutboundIds(kind: CameraCopyKind): Promise<Set<string>> {
    const ids = new Set<string>();
    const copy = this.copies[kind].sender;
    if (copy === null) return ids;
    try {
      const report = await copy.sender.getStats();
      for (const e of report.values() as Iterable<Record<string, unknown>>) {
        const id = e["id"];
        if (e["type"] === "outbound-rtp" && typeof id === "string") ids.add(id);
      }
    } catch {
      // The lines then name that sender `main`.
    }
    return ids;
  }

  /**
   * ADR-0067 step 1: log what the receivers report, once per interval and as
   * a total when the call ends, so a change to how calls survive packet loss
   * can be measured. Diagnostics only: counts and durations.
   */
  private startMediaCounters(pc: RTCPeerConnection): void {
    this.stopMediaCounters();
    // The tiles of this call start from nothing.
    shownLayerLedger.reset();
    this.localCameraSaid = false;
    this.mediaCountersTimer = setInterval(() => {
      void this.sampleMediaCounters(pc);
    }, MEDIA_COUNTERS_INTERVAL_MS);
  }

  /** Whether the `call local camera` line was written for this call. */
  private localCameraSaid = false;

  /**
   * Say once, at the first counters interval at which there is a camera, the
   * state of the camera track and of each copy of it that is sent
   * (`local-preview.ts`): whether the track the own preview plays still
   * gives frames once it has clones.
   */
  private sayLocalCameraOnce(): void {
    if (this.localCameraSaid) return;
    const main = this.localStream?.getVideoTracks()[0];
    if (main === undefined) return;
    this.localCameraSaid = true;
    // eslint-disable-next-line no-console
    console.info(
      describeLocalCamera(main, [
        ["light", this.copies.light.sender?.track],
        ["middle", this.copies.middle.sender?.track],
      ]),
    );
  }

  private async sampleMediaCounters(pc: RTCPeerConnection): Promise<void> {
    if (this.pc !== pc) return;
    let reading: MediaReading;
    let sender: SenderReading | null;
    try {
      const lightOutbound = await this.copyOutboundIds("light");
      const middleOutbound = await this.copyOutboundIds("middle");
      const report = await pc.getStats();
      const entries = [...(report.values() as Iterable<Record<string, unknown>>)];
      reading = readMedia(
        entries,
        this.copyTrackIds.light,
        lightOutbound,
        this.copyTrackIds.middle,
        middleOutbound,
      );
      sender = readSender(entries, lightOutbound, middleOutbound);
    } catch {
      return;
    }
    if (this.pc !== pc) return;
    this.sayLocalCameraOnce();
    // A sender the browser refused to start is asked again, and a flag a
    // renegotiation reset is put back.
    this.syncSenderActivity();
    for (const line of describeDeltas("call media interval", deltaOf(this.mediaCountersLast, reading))) {
      // eslint-disable-next-line no-console
      console.info(line);
    }
    for (const line of describeOutboundSenders(
      "call media interval",
      reading.outboundVideoSenders,
      (kind) => this.senderActivityNote(kind),
      CAMERA_SENDER_KINDS.filter((kind) => this.senderActivity[kind].bound),
    )) {
      // eslint-disable-next-line no-console
      console.info(line);
    }
    if (sender !== null) {
      // eslint-disable-next-line no-console
      console.info(describeSender("call media interval", this.senderCountersLast, sender));
      this.senderCountersLast = sender;
    }
    // What each remote tile showed over the interval, and the frames it
    // observed on its two elements (`layer-display.ts`).
    const tileFrames = shownLayerLedger.takeFrames();
    for (const line of describeShownPeers(
      "call media interval",
      shownLayerLedger.takeInterval(),
      (peerId) => this.wantsBook.qualityOf(peerId),
      (peerId) => this.givenBook.givenOf(peerId) ?? null,
      (peerId) => tileFrames.get(peerId),
    )) {
      // eslint-disable-next-line no-console
      console.info(line);
    }
    this.mediaCountersLast = reading;
  }

  /** Stop the poll and log the whole call's totals from what was read. */
  private stopMediaCounters(): void {
    if (this.mediaCountersTimer !== null) {
      clearInterval(this.mediaCountersTimer);
      this.mediaCountersTimer = null;
    }
    const last = this.mediaCountersLast;
    this.mediaCountersLast = null;
    this.senderCountersLast = null;
    if (last === null) return;
    // Counted from zero: the counters are cumulative from the stream's start.
    for (const line of describeDeltas("call media total", deltaOf(null, last))) {
      // eslint-disable-next-line no-console
      console.info(line);
    }
    for (const line of describeShownPeers(
      "call media total",
      shownLayerLedger.totals(),
      (peerId) => this.wantsBook.qualityOf(peerId),
      (peerId) => this.givenBook.givenOf(peerId) ?? null,
    )) {
      // eslint-disable-next-line no-console
      console.info(line);
    }
    shownLayerLedger.reset();
  }

  /** User-initiated tear-down. Idempotent. Moves to `closed` unless the
   *  session already reached a terminal state (`closed` / `failed`). */
  hangUp(): void {
    this.releaseResources();
    if (this.state !== "closed" && this.state !== "failed") {
      this.setState("closed");
    }
  }

  /** Release sockets / peer connection / media without touching the
   *  lifecycle state — the state transition is the caller's
   *  (`hangUp` → closed, `fail` → failed). Idempotent. */
  private releaseResources(): void {
    this.stopMediaCounters();
    if (this.keepaliveTimer !== null) {
      clearInterval(this.keepaliveTimer);
      this.keepaliveTimer = null;
    }
    if (this.signaling !== null) {
      this.signaling.close();
      this.signaling = null;
    }
    this.audioReceivers.clear();
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
    // The copies' clones are not in the local stream (the self-preview shows
    // one picture), so they are stopped here.
    for (const kind of CAMERA_COPY_KINDS) {
      const slot = this.copies[kind];
      if (slot.sender !== null) {
        try {
          slot.sender.track.stop();
        } catch {
          // best-effort
        }
      }
      // In place: a copy still being set up compares against this very slot.
      slot.sender = null;
      slot.transformId = null;
      slot.refusalWarned = false;
      this.copyTrackIds[kind].clear();
    }
    this.videoNeeded = null;
    for (const kind of CAMERA_SENDER_KINDS) this.senderActivity[kind].reset();
    // Nothing more is asked of the SFU, and the tiles of a later call start
    // from nothing.
    this.wantsSender.reset();
    this.wantsBook.clear();
    if (this.givenBook.size() > 0) {
      this.givenBook.clear();
      this.callbacks.onVideoGiven?.(this.givenBook.snapshot());
    }
    this.publishOfferSent = false;
    this.offerQueue = [];
    this.transformIds.clear();
    this.unsealedSenderWarned = false;
    // Reset perfect-negotiation + video-publish state so a reused instance
    // (or a late callback after teardown) starts clean.
    this.makingOffer = false;
    this.isSettingRemoteAnswerPending = false;
    this.negotiationReady = false;
    this.setVideoPublishState("off");
  }

  // -----------------------------------------------------------------
  // Peer connection + publish
  // -----------------------------------------------------------------

  /** Set by the call-host layer; `null` means STUN-only ICE. */
  private turnProvider: (() => Promise<RTCIceServer | null>) | null = null;

  /**
   * Bind the TURN-credential source used when the peer connection is built.
   *
   * The SFU has a public address, so a client usually reaches it on a
   * server-reflexive candidate - but a peer behind a symmetric NAT or an
   * outbound-UDP-blocking network has no such candidate, and for it the relay
   * is the only way in. Fail-soft: a provider that resolves `null`, or throws,
   * leaves the call on STUN.
   */
  setTurnProvider(provider: (() => Promise<RTCIceServer | null>) | null): void {
    this.turnProvider = provider;
  }

  /** STUN always - explicit config wins over the default - plus TURN when
   *  one can be minted. */
  private async resolveIceServers(): Promise<RTCIceServer[]> {
    const base = [...(this.config.iceServers ?? DEFAULT_ICE_SERVERS)];
    const provider = this.turnProvider;
    if (provider === null) return base;
    try {
      const turn = await provider();
      return turn === null ? base : [...base, turn];
    } catch {
      return base;
    }
  }

  private async openPeerConnectionAndPublish(): Promise<void> {
    if (this.activeSeed === null) {
      throw new Error(
        "openPeerConnectionAndPublish without a seed — refusing to publish " +
          "media without SFrame encryption (security invariant #4)",
      );
    }
    const seed = this.activeSeed;
    // Capture what the device has (section 4.4): the camera and microphone
    // when both work, the microphone alone when the camera fails, nothing
    // when neither can be had — the person joins in every case. Honour the
    // picker-selected device (an `{ exact }` deviceId constraint, else the
    // OS default).
    const captured = await captureWithFallback(
      this.config.media,
      (audio, video) =>
        navigator.mediaDevices.getUserMedia({
          audio: audioTrackConstraint(audio, this.config.media.audioDeviceId),
          video: trackConstraint(video, this.config.media.videoDeviceId),
        }),
    );
    // An empty stream when nothing was captured, so a camera turned on later
    // (`publishVideo`) has a stream to join.
    const stream = captured.stream ?? new MediaStream();
    this.localStream = stream;
    // Surface the capture to the UI (group self-preview tile), mirroring the
    // 1×1 `onLocalStream`. Fired once, before the offer is built.
    this.callbacks.onLocalStream?.(stream);
    if (captured.fallback !== null) {
      // eslint-disable-next-line no-console
      console.warn("SfuCallSession: joining with less than asked", captured.fallback);
      this.callbacks.onCaptureFallback?.(captured.fallback);
    }

    // Built through `sframePeerConnectionConfig`: on a browser that reaches
    // the SFrame worker by `createEncodedStreams()` the connection has to be
    // constructed for it (`./sframe-transport.ts`); elsewhere the
    // configuration is unchanged.
    const pc = new RTCPeerConnection(
      sframePeerConnectionConfig({
        iceServers: await this.resolveIceServers(),
        bundlePolicy: "max-bundle",
        rtcpMuxPolicy: "require",
      }),
    );
    this.pc = pc;
    this.startMediaCounters(pc);

    // Renegotiate on any future local track change (piece B: mid-call
    // `publishVideo` adds a camera track after the initial offer). Gated by
    // `negotiationReady` so the manual initial publish offer below is not
    // duplicated; it flips true once that offer is answered.
    pc.onnegotiationneeded = (): void => {
      void this.onNegotiationNeeded();
    };

    // Publish each local track; SFrame-seal it before it reaches the
    // SFU. Attach the transform AFTER addTrack and BEFORE the offer is
    // built (the same sequencing the native shim follows).
    for (const track of stream.getTracks()) {
      const sender = addGroupCallSender(pc, track, stream);
      this.transformIds.add(
        attachSenderSFrame(sender, {
          seed,
          localLeaf: this.publishLeaf,
        }),
      );
    }
    // The camera's copies (ADR-0070 steps 1 and 5), sealed like every sender
    // before the offer is built.
    const camera = stream.getVideoTracks()[0];
    if (camera !== undefined) await this.followCamera(pc, camera, seed);
    if (this.pc !== pc) return;
    if (stream.getTracks().length === 0) {
      // Nothing to send, but the offer still needs a media line to carry the
      // ICE/DTLS transport the SFU's own offers will reuse for what this
      // person receives. Receive-only: no sender, so nothing to seal.
      pc.addTransceiver("audio", { direction: "recvonly" });
    }

    pc.onicecandidate = (ev): void => {
      const c = ev.candidate;
      if (c === null || c.candidate.length === 0) return;
      try {
        this.signaling?.sendIceCandidate(c.candidate);
      } catch {
        // socket gone — ignore; teardown handles the rest
      }
    };

    pc.ontrack = (ev): void => {
      const stream0 = ev.streams[0];
      const remoteStream = stream0 ?? new MediaStream([ev.track]);
      // Decrypt incoming media; the worker reads the per-frame KID to
      // pick the right per-sender key, so one seed serves every sender.
      // Arm at the CURRENT seed, not the one this connection published
      // with: a track that arrives after a mid-call rekey (a link-call
      // admission advancing the epoch) is sealed under the new epoch, and
      // a receiver armed with the publish seed drops every frame.
      this.transformIds.add(
        attachReceiverSFrame(ev.receiver, { seed: this.activeSeed ?? seed }),
      );
      // Where the browser has no per-frame callback, the tile reads this
      // receiver's statistics instead (`video-frame-watch.ts`).
      if (ev.track.kind === "video") registerVideoFrameSource(ev.track, ev.receiver);
      // The receiver of a light companion or of a middle track is named as
      // such in the media counters.
      const role = splitStreamId(remoteStream.id).role;
      if (role !== "main") {
        const trackId = ev.track.id;
        const ids = this.copyTrackIds[role];
        ids.add(trackId);
        ev.track.addEventListener("ended", () => {
          ids.delete(trackId);
        });
      }
      // A peer's own audio: kept before the track is announced, so whoever
      // hears of the track can already ask for its receiver.
      if (role === "main" && ev.track.kind === "audio") {
        this.audioReceivers.set(remoteStream.id, ev.receiver);
      }
      this.callbacks.onRemoteTrack?.({
        streamId: remoteStream.id,
        track: ev.track,
        stream: remoteStream,
      });
    };

    pc.oniceconnectionstatechange = (): void => {
      const s = pc.iceConnectionState;
      if (s === "connected" || s === "completed") {
        if (this.state === "connecting") this.setState("connected");
        // Say again what this device shows, now that media flows.
        this.wantsSender.resend();
      } else if (s === "failed") {
        this.fail("ICE connection failed");
      }
    };

    // Publish offer (client-initiated). The video transceivers negotiate the
    // generic frame descriptor first, or a native phone cannot find the key
    // frames in the sealed VP8 this browser sends (`frame-descriptor.ts`).
    this.armFrameDescriptor(pc);
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    if (offer.sdp === undefined) {
      throw new Error("createOffer produced no SDP body");
    }
    // The copies' transceivers have their `mid`s now.
    this.signaling?.sendSdpOffer(offer.sdp, this.copyMids("light"), this.copyMids("middle"));
    // The SFU has media state for this peer from this offer on: tell it what
    // this device shows of everyone.
    this.publishOfferSent = true;
    this.wantsSender.resend();

    // Announce the initial camera state to any peers already in the call
    // (camera-off-cleanup) — so a peer that joined camera-off is shown off
    // from the start, not only after the first toggle. A newcomer that joins
    // LATER learns it via the peer-join re-announce in `onPeerJoined`.
    this.announceCameraState();
  }

  // -----------------------------------------------------------------
  // Signaling handlers
  // -----------------------------------------------------------------

  private onWelcome(frame: SfuWelcome): void {
    this.lastInboundMs = this.nowMs();
    this.selfPeerId = frame.peer_id;
    this.wantsBook.setPeers(
      frame.roster.map((p) => p.peer_id).filter((id) => id !== frame.peer_id),
    );
    this.onRosterChanged(false);
    this.callbacks.onWelcome?.(frame.peer_id, frame.roster);
  }

  /** Whether the SFU refused this device because the call had already
   *  ended (`ErrorCode::CallEnded`). Read by the host after a failed
   *  `connect()` to tell that from a connection failure. */
  callEndedByServer(): boolean {
    return this.endedByServer;
  }

  /** The last time the SFU proved it was there (the Welcome or a Pong), on
   *  this session's clock; 0 before the socket opened. What the call record
   *  ends at when the link died before the device noticed
   *  (`callEndedAtMs`). */
  lastInboundAtMs(): number {
    return this.lastInboundMs;
  }

  /** One keepalive tick (`SFU_KEEPALIVE_TICK_MS`): ping when due, fail the
   *  call when the deadline has passed with nothing inbound. */
  private onKeepaliveTick(): void {
    if (this.state === "failed" || this.state === "closed") return;
    switch (sfuKeepaliveAction(this.nowMs(), this.lastInboundMs, this.lastPingMs)) {
      case "ping":
        this.pingNonce += 1;
        this.signaling?.sendPing(this.pingNonce);
        this.lastPingMs = this.nowMs();
        break;
      case "dead":
        // One line, from `fail`: it names every failure, so the detail
        // rides in the reason rather than in a second warning.
        this.fail(
          "keepalive dead (no pong for " +
            Math.round((this.nowMs() - this.lastInboundMs) / 1000) +
            " s)",
        );
        break;
      case "wait":
        break;
    }
  }

  /**
   * The people of the call changed: the middle sender starts or stops with
   * the count, and the SFU is told what this device shows of the people now
   * in it (again, whatever it is, after a join).
   */
  private onRosterChanged(joined: boolean): void {
    this.syncSenderActivity();
    if (joined) this.wantsSender.resend();
    else this.wantsSender.changed();
  }

  private onPeerJoined(frame: SfuPeerJoined): void {
    if (frame.peer_id !== this.selfPeerId) {
      this.wantsBook.addPeer(frame.peer_id);
      this.onRosterChanged(true);
    }
    this.callbacks.onPeerJoined?.(
      frame.peer_id,
      frame.account_pseudonym,
      frame.peer_tag,
    );
    // Re-announce our own current camera state so the newcomer is shown the
    // right thing immediately (camera-off-cleanup) — deterministic initial
    // sync for a peer who toggled off/on before this peer arrived. The relay
    // is a broadcast; existing peers re-applying an unchanged state is a
    // harmless no-op.
    this.announceCameraState();
  }

  private onPeerLeft(frame: SfuPeerLeft): void {
    this.wantsBook.removePeer(frame.peer_id);
    // What the SFU gave of them goes with them: someone who later joins
    // under the same id starts with nothing said.
    if (this.givenBook.removePeer(frame.peer_id)) {
      this.callbacks.onVideoGiven?.(this.givenBook.snapshot());
    }
    this.onRosterChanged(false);
    this.callbacks.onPeerLeft?.(frame.peer_id);
  }

  /** ADR-0032 + camera-off-cleanup: decode the opaque relay payload, classify
   *  it, and route it. The SFU payload is base64; `sfuDecodePayload` (wasm)
   *  recovers the raw bytes; `dispatchIncomingEnvelope` classifies them. wasm
   *  is already loaded (connect() awaited signaling.connect), so the cached
   *  module resolves immediately.
   *
   *  A `MediaState` envelope (camera on/off) is CONSUMED here — it fires
   *  {@link SfuCallCallbacks.onRemoteMediaState} to flip the peer's roster
   *  tile. Every other kind (the ADR-0032 CallWelcome / CallKeyPackage /
   *  commit) falls through to {@link SfuCallCallbacks.onSignalRelay} verbatim,
   *  so the multi-joiner orchestrator classifies it itself. */
  private onSignalRelayFrame(frame: SfuSignalRelay): void {
    const mediaCb = this.callbacks.onRemoteMediaState;
    const relayCb = this.callbacks.onSignalRelay;
    if (mediaCb === undefined && relayCb === undefined) return;
    void (async (): Promise<void> => {
      let wasm: Awaited<ReturnType<typeof loadWasm>>;
      let payload: Uint8Array;
      try {
        wasm = await loadWasm();
        payload = wasm.sfuDecodePayload(frame.payload_b64);
      } catch {
        // A malformed relay payload is dropped, not fatal.
        return;
      }
      // Classify: consume a MediaState signal (camera-off-cleanup); everything
      // else passes through to onSignalRelay unchanged.
      if (mediaCb !== undefined) {
        let isMediaState = false;
        let cameraOn: boolean | undefined;
        try {
          const env = wasm.dispatchIncomingEnvelope(payload);
          isMediaState = env.kind === wasm.IncomingEnvelopeKind.MediaState;
          cameraOn = env.cameraOn;
          env.free();
        } catch {
          // A domain-separator match with a malformed body throws; treat the
          // payload as opaque and fall through to onSignalRelay.
          isMediaState = false;
        }
        if (isMediaState) {
          if (cameraOn !== undefined) mediaCb(frame.from, cameraOn);
          return; // consumed — never double-dispatch to onSignalRelay
        }
      }
      relayCb?.(frame.from, payload);
    })();
  }

  /**
   * Insertable-streams browsers only (`./sframe-transport.ts`); nothing
   * elsewhere. There a sender or receiver whose encoded streams were never
   * taken passes no media, so after every negotiation step every receiver
   * without a transform is given one at the current seed, and a sender left
   * without one (the microphone, the camera, its light or middle copy) is
   * said out loud. Nothing here can let a frame through unsealed: an
   * uncovered endpoint is silent, not plain.
   */
  private coverSFrame(pc: RTCPeerConnection): void {
    if (sframeTransport() !== "encoded-streams") return;
    const seed = this.activeSeed;
    if (seed === null || this.pc !== pc) return;
    const cover = coverEncodedStreams(pc, seed);
    for (const id of cover.attachedReceiverIds) this.transformIds.add(id);
    if (cover.unsealedSenders.length > 0 && !this.unsealedSenderWarned) {
      this.unsealedSenderWarned = true;
      // eslint-disable-next-line no-console
      console.warn(
        `SfuCallSession: ${cover.unsealedSenders.length} sender(s) have no SFrame transform; ` +
          "their media does not flow",
      );
    }
  }

  /** SFU's answer to our publish offer (initial or a mid-call renegotiation). */
  private async onSdpAnswer(frame: SfuSdpAnswer): Promise<void> {
    const pc = this.pc;
    if (pc === null) return;
    try {
      this.isSettingRemoteAnswerPending = true;
      await pc.setRemoteDescription({ type: "answer", sdp: frame.sdp });
      this.isSettingRemoteAnswerPending = false;
      // The senders' codecs are negotiated now: the SFrame worker needs them
      // to leave the right codec header in the clear (ADR-0071).
      syncSenderCodecs();
      this.coverSFrame(pc);
      // A flag this negotiation reset on a sender is put back.
      this.syncSenderActivity();
      // The initial publish (or a mid-call renegotiation) is now answered —
      // arm `onnegotiationneeded` for future local track changes (piece B).
      this.negotiationReady = true;
      // A downstream-track offer that arrived while our offer was out is
      // parked in the queue; it can be answered now.
      void this.drainOfferQueue();
    } catch (err) {
      this.isSettingRemoteAnswerPending = false;
      this.fail(`applying the SFU's answer failed (signalingState=${pc.signalingState})`, err);
    }
  }

  /** See `frame-descriptor.ts`. Logged once per session when a browser
   *  cannot do it, because the symptom (a smeared or blank picture on the
   *  phones) shows nowhere on this side. */
  private armFrameDescriptor(pc: RTCPeerConnection): void {
    const outcome = armGenericFrameDescriptor(
      pc.getTransceivers() as unknown as readonly HeaderExtensionTransceiver[],
    );
    if (outcome.unsupported > 0 && !this.frameDescriptorWarned) {
      this.frameDescriptorWarned = true;
      console.warn(
        `SfuCallSession: ${outcome.unsupported} video transceiver(s) cannot negotiate the generic frame descriptor; native receivers may not decode this browser's video`,
      );
    }
  }

  /**
   * Renegotiate on a local track change (piece B: publishVideo added a
   * camera track). The POLITE offer initiator: gated so it only fires after
   * the initial publish is answered (`negotiationReady`) and never overlaps an
   * in-flight offer or a mid-drain answer. If the state is not `stable` this
   * defers — the browser re-fires `onnegotiationneeded` on the next stable
   * transition while the change is still pending, so it is not lost (that is
   * also how a glare-rolled-back offer gets re-sent).
   */
  private async onNegotiationNeeded(): Promise<void> {
    if (!this.negotiationReady) return;
    const pc = this.pc;
    const signaling = this.signaling;
    if (pc === null || signaling === null) return;
    if (this.makingOffer || pc.signalingState !== "stable") return;
    try {
      this.makingOffer = true;
      // A camera added mid-call is a new video transceiver: same rule as the
      // publish offer, or its frames reach the phones with no descriptor.
      this.armFrameDescriptor(pc);
      await pc.setLocalDescription(); // implicit createOffer
      const sdp = pc.localDescription?.sdp;
      if (sdp !== undefined) {
        signaling.sendSdpOffer(sdp, this.copyMids("light"), this.copyMids("middle"));
      }
    } catch {
      // A failed renegotiation leaves the call up as-is; the publishVideo
      // caller already surfaced the local self-preview.
    } finally {
      this.makingOffer = false;
    }
  }

  /** SFU-initiated offer (a new subscription). Queue + drain so two
   *  offers never overlap. */
  private onSdpOffer(frame: SfuSdpOffer): void {
    this.offerQueue.push(frame.sdp);
    void this.drainOfferQueue();
  }

  private async drainOfferQueue(): Promise<void> {
    if (this.drainingOffers) return;
    this.drainingOffers = true;
    try {
      for (;;) {
        const sdp = this.offerQueue.shift();
        if (sdp === undefined) break;
        const pc = this.pc;
        const signaling = this.signaling;
        if (pc === null || signaling === null) break;
        // Canonical perfect-negotiation collision check. The client is the
        // POLITE peer relative to the SFU, so `ignoreOffer` is always false —
        // it never drops a colliding offer, it yields: applying the remote
        // offer while we have a local offer pending (piece B's publishVideo)
        // implicitly rolls OUR offer back first, then we answer, and
        // `onnegotiationneeded` re-fires afterward to re-send the rolled-back
        // local change. The branch is kept for the textbook shape.
        const readyForOffer =
          !this.makingOffer &&
          (pc.signalingState === "stable" || this.isSettingRemoteAnswerPending);
        const ignoreOffer = !this.polite && !readyForOffer;
        if (ignoreOffer) continue;
        // Our own offer is still out (the initial publish, or a mid-call
        // renegotiation) and the SFU's answer to it has not been applied.
        // The textbook polite move here, applying the remote offer over a
        // rollback of ours, is wrong against the SFU: the SFU never rolls
        // back, it answers, and it answers first. Live on 2026-09-19 the SFU
        // sent its answer and a downstream-track offer in the same
        // millisecond; applying the offer first rolled the publish offer
        // back, the answer then carried fresh ICE credentials (refused by
        // the SFU as an ICE restart) and the SFU's own answer failed with
        // "Called in wrong state: have-remote-offer", killing the join.
        // Park the offer; the answer handler re-drains once it has applied.
        if (pc.signalingState === "have-local-offer" && !this.isSettingRemoteAnswerPending) {
          this.offerQueue.unshift(sdp);
          break;
        }
        const stateBefore = pc.signalingState;
        const ufragBefore = SfuCallSession.iceUfrag(pc.localDescription?.sdp);
        try {
          // No-arg setLocalDescription() so the (polite) rollback + answer is
          // atomic.
          await pc.setRemoteDescription({ type: "offer", sdp });
          // An SFU-initiated offer IS a new subscription: the line above
          // creates the video transceiver another participant's camera
          // arrives on, and it starts with the descriptor stopped. Arm it
          // before the answer, or this browser negotiates no descriptor on
          // every remote video m-line and assembles no frames from the
          // sealed payload -- a blank tile for a peer who is sending fine.
          this.armFrameDescriptor(pc);
          await pc.setLocalDescription();
          // Harmless when nothing changed for the senders: the worker keeps
          // the newest table (ADR-0071).
          syncSenderCodecs();
          // The offer may have created transceivers: every receiver has its
          // streams taken (insertable-streams browsers only).
          this.coverSFrame(pc);
          this.syncSenderActivity();
          const answerSdp = pc.localDescription?.sdp;
          if (answerSdp === undefined) {
            throw new Error("setLocalDescription produced no answer SDP");
          }
          const ufragAfter = SfuCallSession.iceUfrag(answerSdp);
          if (ufragBefore !== undefined && ufragAfter !== ufragBefore) {
            // The SFU refuses an answer that changes the ICE credentials
            // ("Ice restart in answer without one in the offer"), and then
            // drops the peer. Say so where it can be seen.
            // eslint-disable-next-line no-console
            console.warn(
              `SfuCallSession: the answer to the SFU's offer changed the ICE ufrag (${ufragBefore} -> ${ufragAfter}); signalingState before the offer was ${stateBefore}`,
            );
          }
          signaling.sendSdpAnswer(answerSdp);
        } catch (err) {
          // A single bad renegotiation drops that subscription; the
          // call stays up. Continue draining the rest.
          // eslint-disable-next-line no-console
          console.warn(`SfuCallSession: could not answer the SFU's offer (signalingState before=${stateBefore}, now=${pc.signalingState})`, err);
        }
      }
    } finally {
      this.drainingOffers = false;
    }
  }

  private onSignalingClosed(): void {
    if (this.state === "closed" || this.state === "failed") return;
    this.fail("signaling socket closed");
  }

  // -----------------------------------------------------------------
  // State
  // -----------------------------------------------------------------

  private setState(state: SfuCallState): void {
    this.state = state;
    this.callbacks.onStateChange?.(state);
  }

  private fail(reason: string, err?: unknown): void {
    if (this.state === "failed" || this.state === "closed") return;
    // Every failure names itself: the live 2026-09-19 web join died in
    // silence three times before the reason was found in the SFU's log.
    // eslint-disable-next-line no-console
    console.warn(`SfuCallSession: failing the call: ${reason}`, err ?? "");
    this.setState("failed");
    this.hangUp();
  }

  /** The `a=ice-ufrag` of an SDP, for the ICE-credential check below. */
  private static iceUfrag(sdp: string | undefined): string | undefined {
    if (sdp === undefined) return undefined;
    const m = /a=ice-ufrag:([^\r\n]+)/.exec(sdp);
    return m === null ? undefined : m[1];
  }
}
