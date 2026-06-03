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
 *   * Mapping a remote `ontrack` to its source participant: the SFU's
 *     MSID stream-id convention (= source peer id) is not yet pinned,
 *     so we surface the raw stream id and let the caller resolve it.
 *   * Per-remote connection-quality stats.
 *
 * Security invariant #4 (no non-E2EE fallback): if the browser lacks
 * `RTCRtpScriptTransform`, `connect()` throws BEFORE any media is
 * published — the call never opens unencrypted.
 */

import {
  SfuSignalingClient,
  type SfuErrorFrame,
  type SfuPeerJoined,
  type SfuPeerLeft,
  type SfuPeerSnapshot,
  type SfuSdpAnswer,
  type SfuSdpOffer,
  type SfuWelcome,
} from "./sfu-signaling-client";
import {
  attachReceiverSFrame,
  attachSenderSFrame,
  isSFrameTransformSupported,
} from "./sframe-transform";

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
   */
  readonly seed: Uint8Array;
  /**
   * This client's MLS leaf index — the KID it stamps into every SFrame
   * it seals. Remote receivers read it off the header to pick the
   * matching per-sender key.
   */
  readonly localLeaf: number;
  /** Mic / camera request. */
  readonly media: SfuCallMedia;
  /**
   * ICE servers (STUN / TURN). Defaults to public STUN; production
   * supplies the self-hosted TURN credentials from
   * `services/auth` `POST /call/turn-credential`.
   */
  readonly iceServers?: readonly RTCIceServer[];
}

/** A remote participant's media surfaced to the caller. */
export interface SfuRemoteTrack {
  /**
   * The MSID stream id the SFU tagged the track with. The caller maps
   * this to a participant (see the deferred note in the module doc).
   */
  readonly streamId: string;
  readonly track: MediaStreamTrack;
  readonly stream: MediaStream;
}

/** Callbacks the caller wires in. */
export interface SfuCallCallbacks {
  /** Lifecycle tick. */
  onStateChange?(state: SfuCallState): void;
  /** Initial roster (from the SFU `Welcome`). */
  onWelcome?(selfPeerId: string, roster: readonly SfuPeerSnapshot[]): void;
  /** A participant joined the call. */
  onPeerJoined?(peerId: string, accountPseudonym: string): void;
  /** A participant left. */
  onPeerLeft?(peerId: string): void;
  /** A remote media track arrived (already SFrame-decrypting). */
  onRemoteTrack?(remote: SfuRemoteTrack): void;
  /** The SFU reported a protocol-level error. */
  onServerError?(frame: SfuErrorFrame): void;
}

const DEFAULT_ICE_SERVERS: readonly RTCIceServer[] = [
  { urls: "stun:stun.l.google.com:19302" },
  { urls: "stun:stun1.l.google.com:19302" },
];

/**
 * One instance per active SFU call. Construct, then `connect()`. Tear
 * down with `hangUp()` (idempotent).
 */
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

  constructor(
    private readonly config: SfuCallConfig,
    private readonly callbacks: SfuCallCallbacks = {},
  ) {}

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
        onSdpAnswer: (f): void => void this.onSdpAnswer(f),
        onSdpOffer: (f): void => this.onSdpOffer(f),
        onServerError: (f): void => this.callbacks.onServerError?.(f),
        onClose: (): void => this.onSignalingClosed(),
        onSocketError: (): void => this.onSignalingClosed(),
      },
    );
    try {
      await this.signaling.connect();
    } catch (err) {
      this.fail();
      throw err;
    }

    // 2. Build the peer connection + capture + publish.
    try {
      await this.openPeerConnectionAndPublish();
    } catch (err) {
      this.fail();
      throw err;
    }
  }

  /** Mute / unmute the local microphone (local-only gate, no
   *  renegotiation). */
  setMicrophoneEnabled(on: boolean): void {
    if (this.localStream === null) return;
    for (const track of this.localStream.getAudioTracks()) {
      track.enabled = on;
    }
  }

  /** Enable / disable the local camera (local-only gate). */
  setCameraEnabled(on: boolean): void {
    if (this.localStream === null) return;
    for (const track of this.localStream.getVideoTracks()) {
      track.enabled = on;
    }
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
  }

  // -----------------------------------------------------------------
  // Peer connection + publish
  // -----------------------------------------------------------------

  private async openPeerConnectionAndPublish(): Promise<void> {
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: this.config.media.audio,
      video: this.config.media.video,
    });
    this.localStream = stream;

    const pc = new RTCPeerConnection({
      iceServers: [...(this.config.iceServers ?? DEFAULT_ICE_SERVERS)],
      bundlePolicy: "max-bundle",
      rtcpMuxPolicy: "require",
    });
    this.pc = pc;

    // Publish each local track; SFrame-seal it before it reaches the
    // SFU. Attach the transform AFTER addTrack and BEFORE the offer is
    // built (the same sequencing the native shim follows).
    for (const track of stream.getTracks()) {
      const sender = pc.addTrack(track, stream);
      attachSenderSFrame(sender, {
        seed: this.config.seed,
        localLeaf: this.config.localLeaf,
      });
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
      attachReceiverSFrame(ev.receiver, { seed: this.config.seed });
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
        this.fail();
      }
    };

    // Publish offer (client-initiated).
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    if (offer.sdp === undefined) {
      throw new Error("createOffer produced no SDP body");
    }
    this.signaling?.sendSdpOffer(offer.sdp);
  }

  // -----------------------------------------------------------------
  // Signaling handlers
  // -----------------------------------------------------------------

  private onWelcome(frame: SfuWelcome): void {
    this.selfPeerId = frame.peer_id;
    this.callbacks.onWelcome?.(frame.peer_id, frame.roster);
  }

  private onPeerJoined(frame: SfuPeerJoined): void {
    this.callbacks.onPeerJoined?.(frame.peer_id, frame.account_pseudonym);
  }

  private onPeerLeft(frame: SfuPeerLeft): void {
    this.callbacks.onPeerLeft?.(frame.peer_id);
  }

  /** SFU's answer to our publish offer. */
  private async onSdpAnswer(frame: SfuSdpAnswer): Promise<void> {
    const pc = this.pc;
    if (pc === null) return;
    try {
      await pc.setRemoteDescription({ type: "answer", sdp: frame.sdp });
    } catch {
      this.fail();
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
        try {
          await pc.setRemoteDescription({ type: "offer", sdp });
          const answer = await pc.createAnswer();
          await pc.setLocalDescription(answer);
          if (answer.sdp === undefined) {
            throw new Error("createAnswer produced no SDP body");
          }
          signaling.sendSdpAnswer(answer.sdp);
        } catch {
          // A single bad renegotiation drops that subscription; the
          // call stays up. Continue draining the rest.
        }
      }
    } finally {
      this.drainingOffers = false;
    }
  }

  private onSignalingClosed(): void {
    if (this.state === "closed" || this.state === "failed") return;
    this.fail();
  }

  // -----------------------------------------------------------------
  // State
  // -----------------------------------------------------------------

  private setState(state: SfuCallState): void {
    this.state = state;
    this.callbacks.onStateChange?.(state);
  }

  private fail(): void {
    if (this.state === "failed" || this.state === "closed") return;
    this.setState("failed");
    this.hangUp();
  }
}
