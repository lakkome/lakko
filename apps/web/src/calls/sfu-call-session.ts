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
  rekeySFrame,
  isSFrameTransformSupported,
} from "./sframe-transform";
import { trackConstraint } from "./call-session";

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
   * The MSID stream id the SFU tagged the track with. The SFU pins
   * this to the source peer id (see the module doc), so the caller
   * maps it straight to a participant (`peer-<streamId>`).
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
  /** Call-key seed of the CURRENT MLS epoch. Starts at `config.seed` and
   *  advances on `rekey` so a late-arriving receiver attaches at the current
   *  epoch. */
  private activeSeed: Uint8Array;

  constructor(
    private readonly config: SfuCallConfig,
    private readonly callbacks: SfuCallCallbacks = {},
  ) {
    this.activeSeed = config.seed;
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
    if (seedsEqual(this.activeSeed, newSeed)) return;
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
  }

  // -----------------------------------------------------------------
  // Peer connection + publish
  // -----------------------------------------------------------------

  private async openPeerConnectionAndPublish(): Promise<void> {
    // Honour the picker-selected device at publish time (an `{ exact }`
    // deviceId constraint, else the OS default).
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: trackConstraint(
        this.config.media.audio,
        this.config.media.audioDeviceId,
      ),
      video: trackConstraint(
        this.config.media.video,
        this.config.media.videoDeviceId,
      ),
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
      this.transformIds.add(
        attachSenderSFrame(sender, {
          seed: this.activeSeed,
          localLeaf: this.config.localLeaf,
        }),
      );
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
      this.transformIds.add(
        attachReceiverSFrame(ev.receiver, { seed: this.activeSeed }),
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
    this.callbacks.onPeerJoined?.(
      frame.peer_id,
      frame.account_pseudonym,
      frame.peer_tag,
    );
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
