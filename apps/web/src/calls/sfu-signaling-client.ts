// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * SFU WebSocket signaling client (SFU multi-party arc, subsprint 3a,
 * web).
 *
 * Connects to the Rust SFU's `lakko-sfu-v1` WebSocket
 * (`/v1/connect?token=<jwt>`), parses the typed server frames, and
 * sends the typed client frames. Wire framing goes through the
 * shared `lakko-sfu-proto` crate via wasm-bindgen
 * (`validateSfuServerFrame` + `composeSfu*`), so the web client and
 * the SFU agree on the wire shape from one definition.
 *
 * This is the **transport + framing** layer only — it does not touch
 * the `RTCPeerConnection`. The CallSession SFU-mode wiring (route the
 * local SDP offer through `sendSdpOffer`, apply the `SdpAnswer`,
 * trickle ICE via `sendIceCandidate`, drive SFU-initiated renegotiation
 * from `onSdpOffer`) is built on top of this client in
 * `sfu-call-session.ts`.
 *
 * ## Inbound parsing
 *
 * Every inbound text frame is first handed to the shared
 * `validateSfuServerFrame` (size cap + fail-fast on an unknown
 * `kind`); the returned `kind` then selects a typed view over the
 * native `JSON.parse` of the same text. The wasm side does the
 * load-bearing validation; the field read is a plain TypeScript cast
 * (the web is the one platform with first-class JSON + types — see
 * `crates/lakko-ffi-wasm/src/sfu_signaling.rs` for why the surface
 * differs from the UniFFI typed-enum on mobile).
 */

import { loadWasm } from "../wasm";
import {
  claimsVideoGiven,
  readVideoGivenFrame,
  VIDEO_GIVEN_MALFORMED_WARNING,
  type VideoGivenEntry,
} from "./video-given";
import {
  claimsVideoNeeded,
  readVideoNeededFrame,
  VIDEO_NEEDED_MALFORMED_WARNING,
  type VideoNeeded,
} from "./sender-activity";

// ---- Typed server frames ------------------------------------------

/** Roster entry in a {@link SfuWelcome}. */
export interface SfuPeerSnapshot {
  readonly peer_id: string;
  readonly account_pseudonym: string;
  /** Per-call peer tag (ADR-0021). Re-derive `sfuPeerTag(call_id,
   *  account_id)` over a contact's account id and match to resolve this
   *  entry to a name. Optional — absent from a pre-ADR-0021 SFU. */
  readonly peer_tag?: string;
}

/** Machine-readable SFU error code (kebab-case wire form). */
export type SfuErrorCode =
  | "bad-frame"
  | "frame-too-large"
  | "payload-too-large"
  | "unknown-peer"
  | "backpressure-drop"
  | "rate-limited"
  | "bad-sdp"
  | "bad-ice-candidate"
  /** The call ended before this socket arrived (`services/sfu` call
   *  lifetime): sent in place of the Welcome, then the socket closes. */
  | "call-ended";

export interface SfuWelcome {
  readonly kind: "welcome";
  readonly peer_id: string;
  readonly call_id: string;
  readonly roster: readonly SfuPeerSnapshot[];
  readonly server_ts: number;
}
export interface SfuPeerJoined {
  readonly kind: "peer-joined";
  readonly peer_id: string;
  readonly account_pseudonym: string;
  /** Per-call peer tag (ADR-0021) — see {@link SfuPeerSnapshot.peer_tag}. */
  readonly peer_tag?: string;
}
export interface SfuPeerLeft {
  readonly kind: "peer-left";
  readonly peer_id: string;
}
export interface SfuSignalRelay {
  readonly kind: "signal-relay";
  readonly from: string;
  readonly payload_b64: string;
}
export interface SfuErrorFrame {
  readonly kind: "error";
  readonly code: SfuErrorCode;
  readonly detail: string;
}
export interface SfuPong {
  readonly kind: "pong";
  readonly nonce: number;
}
export interface SfuSdpAnswer {
  readonly kind: "sdp-answer";
  readonly sdp: string;
}
export interface SfuSdpOffer {
  readonly kind: "sdp-offer";
  readonly sdp: string;
}

/** Discriminated union of every inbound server frame. */
export type SfuServerFrame =
  | SfuWelcome
  | SfuPeerJoined
  | SfuPeerLeft
  | SfuSignalRelay
  | SfuErrorFrame
  | SfuPong
  | SfuSdpAnswer
  | SfuSdpOffer;

// ---- Callbacks -----------------------------------------------------

/** Handlers the caller wires in. Every field is optional; an
 *  unhandled frame is silently ignored (the client never throws into
 *  the caller from the socket's message handler). */
export interface SfuSignalingCallbacks {
  onWelcome?(frame: SfuWelcome): void;
  onPeerJoined?(frame: SfuPeerJoined): void;
  onPeerLeft?(frame: SfuPeerLeft): void;
  onSignalRelay?(frame: SfuSignalRelay): void;
  onSdpAnswer?(frame: SfuSdpAnswer): void;
  onSdpOffer?(frame: SfuSdpOffer): void;
  /**
   * What the SFU gives this device of each person's video (ADR-0070 step 6,
   * `video-given.ts`): the whole set, in the frame's order. Sent only by an
   * SFU with downlink control on, when the set changes and after a
   * `video-wants`. A malformed frame never reaches this.
   */
  onVideoGiven?(given: readonly VideoGivenEntry[]): void;
  /**
   * Which of this device's own camera senders somebody is being sent
   * (`sender-activity.ts`): the whole state each time, the last frame wins.
   * `false` for a sender: stop encoding and sending on it; `true`: send it.
   * The light sender is always needed and is not named. Sent only to a
   * publisher that declared a light sender. A malformed frame never reaches
   * this.
   */
  onVideoNeeded?(needed: VideoNeeded): void;
  onPong?(frame: SfuPong): void;
  onServerError?(frame: SfuErrorFrame): void;
  /** A malformed / unknown-kind frame the shared validator rejected.
   *  The connection is NOT closed automatically — the caller decides. */
  onParseError?(raw: string, error: unknown): void;
  /** The socket closed (clean or not). `code` / `reason` are the
   *  WebSocket close frame's. */
  onClose?(code: number, reason: string): void;
  /** Low-level socket error (transport). */
  onSocketError?(): void;
}

// ---- Errors --------------------------------------------------------

/** Caller misuse — bad arg shape before any I/O. */
export class SfuSignalingArgError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SfuSignalingArgError";
  }
}

// ---- Client --------------------------------------------------------

/** The WebSocket subprotocol the SFU requires. */
export const SFU_SUBPROTOCOL = "lakko-sfu-v1";

/**
 * One instance per call. Construct with the SFU base URL (a
 * `ws://` / `wss://` origin, no path) + the call-admission JWT
 * (minted via `CallJwtClient`), then `connect()`. Frames arrive on
 * the wired callbacks; send with the `send*` methods.
 */
export class SfuSignalingClient {
  private socket: WebSocket | null = null;
  private wasm: Awaited<ReturnType<typeof loadWasm>> | null = null;

  constructor(
    /** SFU origin, e.g. `wss://sfu.lakko.me`. No trailing path. */
    private readonly baseUrl: string,
    /** Call-admission JWT (JWS Compact) from `CallJwtClient.mint`. */
    private readonly jwt: string,
    private readonly callbacks: SfuSignalingCallbacks = {},
  ) {
    if (baseUrl.length === 0) {
      throw new SfuSignalingArgError("baseUrl is empty");
    }
    if (jwt.length === 0) {
      throw new SfuSignalingArgError("jwt is empty");
    }
  }

  /** True once the socket is OPEN. */
  get isOpen(): boolean {
    return this.socket?.readyState === WebSocket.OPEN;
  }

  /**
   * Open the WebSocket and start dispatching frames. Resolves when
   * the socket reaches OPEN (the SFU's `Welcome` frame follows on the
   * `onWelcome` callback); rejects if the socket errors / closes
   * before opening. Idempotent-guarded: throws if already connected.
   */
  async connect(): Promise<void> {
    if (this.socket !== null) {
      throw new SfuSignalingArgError("connect() called twice");
    }
    // Pre-load wasm so the synchronous message handler can call the
    // shared validator without an await per frame.
    this.wasm = await loadWasm();
    const url = `${this.baseUrl}/v1/connect?token=${encodeURIComponent(this.jwt)}`;
    const socket = new WebSocket(url, SFU_SUBPROTOCOL);
    this.socket = socket;

    return new Promise<void>((resolve, reject) => {
      let settled = false;
      socket.onopen = (): void => {
        settled = true;
        resolve();
      };
      socket.onmessage = (ev: MessageEvent): void => {
        if (typeof ev.data === "string") {
          this.handleInbound(ev.data);
        }
        // Binary frames are not part of the lakko-sfu-v1 protocol —
        // ignore them rather than choke.
      };
      socket.onerror = (): void => {
        this.callbacks.onSocketError?.();
        if (!settled) {
          settled = true;
          reject(new Error("SFU WebSocket errored before opening"));
        }
      };
      socket.onclose = (ev: CloseEvent): void => {
        this.callbacks.onClose?.(ev.code, ev.reason);
        if (!settled) {
          settled = true;
          reject(
            new Error(`SFU WebSocket closed before opening (code ${ev.code})`),
          );
        }
      };
    });
  }

  /** Close the socket. Idempotent. */
  close(code = 1000, reason = "client closed"): void {
    const socket = this.socket;
    this.socket = null;
    if (socket !== null) {
      socket.onopen = null;
      socket.onmessage = null;
      socket.onerror = null;
      socket.onclose = null;
      try {
        socket.close(code, reason);
      } catch {
        // best-effort
      }
    }
  }

  // ---- Send (client → SFU) ----------------------------------------

  /**
   * Send a client → SFU SDP offer (publish path). `lightMids` names the
   * `mid` of every video m-line of this offer that carries the light sender
   * of this client's camera (ADR-0070 step 1), so the SFU forwards it as the
   * camera's light companion; `middleMids` those that carry its middle sender
   * (step 5), which the SFU forwards only to a viewer that asks for it. Each
   * is empty when there is none.
   */
  sendSdpOffer(sdp: string, lightMids: readonly string[], middleMids: readonly string[] = []): void {
    this.sendRaw(
      this.requireWasm().composeSfuSdpOfferWithMiddle(sdp, [...lightMids], [...middleMids]),
    );
  }

  /**
   * Tell the SFU which picture of each publisher this client shows (ADR-0070
   * step 5): `qualities[i]` is `main`, `middle` or `light` for `peers[i]`.
   * The frame replaces the whole set; a publisher it leaves out is `main`.
   * Throws when the lists are outside the protocol's bounds.
   */
  sendVideoWants(peers: readonly string[], qualities: readonly string[]): void {
    this.sendRaw(this.requireWasm().composeSfuVideoWants([...peers], [...qualities]));
  }

  /** Send a client → SFU SDP answer (responds to a SFU-initiated
   *  offer). */
  sendSdpAnswer(sdp: string): void {
    this.sendRaw(this.requireWasm().composeSfuSdpAnswer(sdp));
  }

  /** Send a trickle-ICE candidate. `candidateSdp` is the value of
   *  `RTCIceCandidate.candidate`. */
  sendIceCandidate(candidateSdp: string): void {
    this.sendRaw(this.requireWasm().composeSfuIceCandidate(candidateSdp));
  }

  /** Relay an opaque payload (e.g. an SFrame-key envelope) to a
   *  single peer. */
  sendSignalRelayToPeer(peerId: string, payload: Uint8Array): void {
    this.sendRaw(
      this.requireWasm().composeSfuSignalRelayToPeer(peerId, payload),
    );
  }

  /** Relay an opaque payload to every other peer in the call. */
  sendSignalRelayToCall(payload: Uint8Array): void {
    this.sendRaw(this.requireWasm().composeSfuSignalRelayToCall(payload));
  }

  /** Application-layer keepalive; the SFU echoes a `Pong`. */
  sendPing(nonce: number): void {
    this.sendRaw(this.requireWasm().composeSfuPing(BigInt(nonce)));
  }

  // ---- Internal ----------------------------------------------------

  private requireWasm(): Awaited<ReturnType<typeof loadWasm>> {
    if (this.wasm === null) {
      throw new SfuSignalingArgError("wasm not loaded — call connect() first");
    }
    return this.wasm;
  }

  private sendRaw(json: string): void {
    const socket = this.socket;
    if (socket === null || socket.readyState !== WebSocket.OPEN) {
      throw new SfuSignalingArgError("socket is not open");
    }
    socket.send(json);
  }

  private handleInbound(raw: string): void {
    const wasm = this.wasm;
    if (wasm === null) return;
    let kind: string;
    try {
      // Shared validator: size cap + fail-fast on unknown kind.
      kind = wasm.validateSfuServerFrame(raw);
    } catch (err) {
      // A `video-given` frame outside the protocol's bounds (an unknown
      // quality, an empty peer) is refused here: ignored, with one warning.
      if (claimsVideoGiven(raw)) {
        // eslint-disable-next-line no-console
        console.warn(VIDEO_GIVEN_MALFORMED_WARNING);
      } else if (claimsVideoNeeded(raw)) {
        // A `video-needed` frame without both booleans: ignored, with one
        // warning. The senders stay as the last good frame left them.
        // eslint-disable-next-line no-console
        console.warn(VIDEO_NEEDED_MALFORMED_WARNING);
      }
      this.callbacks.onParseError?.(raw, err);
      return;
    }
    if (kind === "video-given") {
      // Read through the shared helper (bounds and quality names are the
      // protocol crate's), not from the untrusted `JSON.parse`.
      const given = readVideoGivenFrame(wasm, raw);
      if (given === null) {
        // eslint-disable-next-line no-console
        console.warn(VIDEO_GIVEN_MALFORMED_WARNING);
        return;
      }
      try {
        this.callbacks.onVideoGiven?.(given);
      } catch {
        // A handler's failure never reaches the socket's message handler.
      }
      return;
    }
    if (kind === "video-needed") {
      // Read through the shared helper, not from the untrusted `JSON.parse`.
      const needed = readVideoNeededFrame(wasm, raw);
      if (needed === null) {
        // eslint-disable-next-line no-console
        console.warn(VIDEO_NEEDED_MALFORMED_WARNING);
        return;
      }
      try {
        this.callbacks.onVideoNeeded?.(needed);
      } catch {
        // A handler's failure never reaches the socket's message handler.
      }
      return;
    }
    let frame: SfuServerFrame;
    try {
      frame = JSON.parse(raw) as SfuServerFrame;
    } catch (err) {
      this.callbacks.onParseError?.(raw, err);
      return;
    }
    // `kind` is already validated against the shared enum; dispatch on
    // it (not on the untrusted JSON.parse alone).
    switch (kind) {
      case "welcome":
        this.callbacks.onWelcome?.(frame as SfuWelcome);
        break;
      case "peer-joined":
        this.callbacks.onPeerJoined?.(frame as SfuPeerJoined);
        break;
      case "peer-left":
        this.callbacks.onPeerLeft?.(frame as SfuPeerLeft);
        break;
      case "signal-relay":
        this.callbacks.onSignalRelay?.(frame as SfuSignalRelay);
        break;
      case "error":
        this.callbacks.onServerError?.(frame as SfuErrorFrame);
        break;
      case "pong":
        this.callbacks.onPong?.(frame as SfuPong);
        break;
      case "sdp-answer":
        this.callbacks.onSdpAnswer?.(frame as SfuSdpAnswer);
        break;
      case "sdp-offer":
        this.callbacks.onSdpOffer?.(frame as SfuSdpOffer);
        break;
      default:
        // `validateSfuServerFrame` already rejects unknown kinds, so
        // this is unreachable; keep it total for the type-checker.
        this.callbacks.onParseError?.(raw, `unhandled kind: ${kind}`);
    }
  }
}
