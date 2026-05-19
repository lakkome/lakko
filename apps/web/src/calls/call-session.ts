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
 * MVP scope (Phase 3a8):
 *
 *   * Audio-only.
 *   * 1×1 only — no SFU, no SFrame insertable streams (the
 *     P2P path uses native DTLS-SRTP; SFrame layers in once
 *     a SFU sits between the peers in Phase 3b).
 *   * No video, no screen share, no echo cancellation
 *     tuning beyond `getUserMedia` defaults.
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

/** Callback set the caller wires in. Each fires on a
 *  state-machine transition. */
export interface CallSessionCallbacks {
  /** Audio + video tracks the platform-native PeerConnection
   *  produces for the remote side. UI binds them to
   *  `<audio>` / `<video>` elements. */
  onRemoteStream(stream: MediaStream): void;
  /** Outbound signaling bytes to PUT to the chat group's DS
   *  journal. The composer wraps in MLS application
   *  message; transport stays on the existing Phase 1
   *  pipeline. */
  onOutboundSignaling(plaintextBytes: Uint8Array): void;
  /** State transition tick. UI re-renders. */
  onStateChange(state: WebCallState, reason?: WebCallEndReason): void;
}

/** Audio / video request flags. Phase 3a8 MVP is audio-only
 *  on both sides; the field is here for forward-compat. */
export interface CallMedia {
  readonly audio: boolean;
  readonly video: boolean;
}

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

  constructor(
    callId: Uint8Array,
    private readonly callbacks: CallSessionCallbacks,
  ) {
    this.callId = callId;
  }

  /** Current state — UI re-reads after every callback. */
  getState(): WebCallState {
    return this.state;
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
    this.isOfferer = true;
    this.setState("outgoing");
    await this.openPeerConnection(media);
    if (this.pc === null) throw new Error("peer connection not initialised");
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
   * call_id reach this method) and feeds them in order.
   */
  async applyIncomingSignaling(parsed: ParsedCallSignaling): Promise<void> {
    switch (parsed.kind) {
      case "offer":
        // We're the answerer — fresh inbound call.
        if (this.state !== "idle") {
          // Reject double-offer; the caller's session-
          // registry layer is responsible for not routing
          // an offer to an already-active session.
          return;
        }
        this.isOfferer = false;
        this.setState("ringing");
        await this.openPeerConnection({
          audio: parsed.mediaAudio,
          video: parsed.mediaVideo,
        });
        if (this.pc === null) return;
        try {
          await this.pc.setRemoteDescription({
            type: "offer",
            sdp: parsed.sdp,
          });
          await this.applyQueuedCandidates();
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("setRemoteDescription(offer) failed", err);
          this.endLocal("error");
        }
        break;

      case "answer":
        if (this.state !== "outgoing") return;
        if (this.pc === null) return;
        try {
          await this.pc.setRemoteDescription({
            type: "answer",
            sdp: parsed.sdp,
          });
          await this.applyQueuedCandidates();
          this.setState("connecting");
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("setRemoteDescription(answer) failed", err);
          this.endLocal("error");
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
    if (this.state !== "ringing" || this.isOfferer) return;
    if (this.pc === null) {
      this.endLocal("error");
      return;
    }
    try {
      const answer = await this.pc.createAnswer();
      await this.pc.setLocalDescription(answer);
      if (answer.sdp === undefined) {
        throw new Error("createAnswer produced no SDP body");
      }
      const wasm = await loadWasm();
      const bytes = wasm.composeCallAnswerSignaling(this.callId, answer.sdp);
      this.callbacks.onOutboundSignaling(bytes);
      this.setState("connecting");
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("accept failed", err);
      this.endLocal("error");
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

  /** Idempotent tear-down — safe to call multiple times. */
  dispose(): void {
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
    if (this.state !== "done") {
      this.setState("done");
    }
  }

  // -----------------------------------------------------------------
  // Internal
  // -----------------------------------------------------------------

  private async openPeerConnection(media: CallMedia): Promise<void> {
    // Phase 3a8 MVP: audio-only. The field is plumbed
    // through so 3a-followups can switch to AudioVideo
    // without an API churn.
    const constraints: MediaStreamConstraints = {
      audio: media.audio,
      video: media.video,
    };
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia(constraints);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("getUserMedia denied / failed", err);
      this.endLocal("error");
      throw err;
    }
    this.localStream = stream;

    const pc = new RTCPeerConnection({
      iceServers: PUBLIC_ICE_SERVERS,
      // P2P 1×1 uses native DTLS-SRTP. SFrame insertable
      // streams land in 3b when an SFU sits between peers.
      bundlePolicy: "max-bundle",
      rtcpMuxPolicy: "require",
    });
    this.pc = pc;

    for (const track of stream.getTracks()) {
      pc.addTrack(track, stream);
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
      // Audio-only MVP — `ev.streams[0]` is the remote
      // audio stream. UI binds it to an `<audio>` element.
      const remote = ev.streams[0];
      if (remote !== undefined) {
        this.callbacks.onRemoteStream(remote);
      }
    };

    pc.oniceconnectionstatechange = (): void => {
      const s = pc.iceConnectionState;
      if (s === "connected" || s === "completed") {
        if (this.state === "connecting" || this.state === "ringing") {
          this.setState("connected");
        }
      } else if (s === "failed") {
        this.endLocal("error");
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

  private async endLocal(reason: WebCallEndReason): Promise<void> {
    if (this.state === "done") return;
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
    if (this.state === "done") return;
    this.dispose();
    this.setState("done", reason);
  }

  private setState(state: WebCallState, reason?: WebCallEndReason): void {
    this.state = state;
    this.callbacks.onStateChange(state, reason);
  }
}
