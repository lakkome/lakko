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
 * MVP: one publish offer at connect; SFU-driven subscribe
 * renegotiation; SFrame on every track; trickle ICE both ways.
 * Deferred (each a follow-up, none a security gap):
 *   * Client-initiated mid-call renegotiation (add / drop a track
 *     after the first offer) — needs perfect-negotiation glare
 *     handling.
 *   * Per-remote connection-quality stats.
 *
 * Mapping a remote `ontrack` to its source participant IS resolved:
 * the SFU pins the downstream MSID stream-id to the source peer id
 * (`services/sfu/src/media.rs` `add_media(.., Some(source_peer), ..)`),
 * and `PeerId Display == the wire peer_id`, so the `streamId` this
 * session surfaces is exactly that peer id. `sfu-call-ui.ts` keys the
 * roster tile off it (`peer-<streamId>`).
 *
 * Security invariant #4 (no non-E2EE fallback): if the browser lacks
 * `RTCRtpScriptTransform`, `connect()` throws BEFORE any media is
 * published — the call never opens unencrypted.
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
import {
  attachReceiverSFrame,
  attachSenderSFrame,
  rekeySFrame,
  isSFrameTransformSupported,
} from "./sframe-transform";
import { audioTrackConstraint, trackConstraint, type VideoPublishState } from "./call-session";
import { captureWithFallback, type CaptureFallback } from "./capture-fallback";
import { SFU_KEEPALIVE_TICK_MS, sfuKeepaliveAction } from "./sfu-keepalive";

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
   * maps it straight to a participant (`peer-<streamId>`).
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
   * Throws (before any media capture) if the browser lacks
   * `RTCRtpScriptTransform` — invariant #4, no non-E2EE fallback.
   */
  async connect(): Promise<void> {
    if (this.state !== "idle") {
      throw new Error("SfuCallSession.connect() called twice");
    }
    if (!isSFrameTransformSupported()) {
      throw new Error(
        "RTCRtpScriptTransform is unavailable — refusing to join the SFU " +
          "call without SFrame media encryption (security invariant #4)",
      );
    }
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
    this.setLocalCameraOn(on);
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
    const existing = pc.getSenders().find((s) => s.track?.kind === "video");
    if (existing?.track != null && existing.track.readyState === "live") {
      existing.track.enabled = true;
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
      localStream.addTrack(track);
      const sender = pc2.addTrack(track, localStream);
      this.transformIds.add(
        attachSenderSFrame(sender, { seed, localLeaf: this.publishLeaf }),
      );
      // addTrack fired onnegotiationneeded -> the SDP renegotiation runs in the
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
    const sender = this.pc.getSenders().find((s) => s.track?.kind === kind);
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
    if (this.keepaliveTimer !== null) {
      clearInterval(this.keepaliveTimer);
      this.keepaliveTimer = null;
    }
    if (this.signaling !== null) {
      this.signaling.close();
      this.signaling = null;
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
    this.offerQueue = [];
    this.transformIds.clear();
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

    const pc = new RTCPeerConnection({
      iceServers: [...(this.config.iceServers ?? DEFAULT_ICE_SERVERS)],
      bundlePolicy: "max-bundle",
      rtcpMuxPolicy: "require",
    });
    this.pc = pc;

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
      const sender = pc.addTrack(track, stream);
      this.transformIds.add(
        attachSenderSFrame(sender, {
          seed,
          localLeaf: this.publishLeaf,
        }),
      );
    }
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
    this.signaling?.sendSdpOffer(offer.sdp);

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

  private onPeerJoined(frame: SfuPeerJoined): void {
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

  /** SFU's answer to our publish offer (initial or a mid-call renegotiation). */
  private async onSdpAnswer(frame: SfuSdpAnswer): Promise<void> {
    const pc = this.pc;
    if (pc === null) return;
    try {
      this.isSettingRemoteAnswerPending = true;
      await pc.setRemoteDescription({ type: "answer", sdp: frame.sdp });
      this.isSettingRemoteAnswerPending = false;
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
      if (sdp !== undefined) signaling.sendSdpOffer(sdp);
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
          await pc.setLocalDescription();
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
