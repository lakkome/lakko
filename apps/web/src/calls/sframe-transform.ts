// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web SFrame attach machinery — the browser mirror of the native
 * `apps/shared/sframe-bridge` C++ shim.
 *
 * On native (iOS / Android) every outgoing media frame is sealed and
 * every incoming frame opened inside libwebrtc's
 * `FrameEncryptorInterface` / `FrameDecryptorInterface`. On the web
 * the equivalent hook is the W3C **Encoded Transform** API
 * (`RTCRtpScriptTransform`): a `Worker` sees a `TransformStream` of
 * `RTCEncodedAudioFrame` / `RTCEncodedVideoFrame` and rewrites each
 * frame's `.data` in place.
 *
 * This module runs on the MAIN thread. It owns the single shared
 * transform `Worker` and exposes two attach helpers that the SFU
 * call path calls right after it adds a track to (or receives a
 * track from) the `RTCPeerConnection` — exactly the
 * "attach-between-addTrack-and-SDP-build" sequencing the native
 * shim follows.
 *
 * The actual seal / open is `sealFramed` / `openFramed` from
 * `lakko-ffi-wasm`, called inside the worker. The AEAD AAD is the
 * SFrame wire header (KID + CTR) — NOT any host RTP metadata — so a
 * frame sealed here decrypts byte-for-byte on iOS / Android and vice
 * versa (ADR-0013).
 *
 * Security invariant #4 (no fallback to non-E2EE): if the browser
 * does not expose `RTCRtpScriptTransform`, the attach helpers THROW.
 * A caller that cannot attach the transform MUST refuse to open the
 * call rather than send plaintext media through the SFU.
 *
 * Scope: this is the standalone transform building block. Wiring it
 * into a live multi-party `CallSession` is the SFU-multiparty step;
 * the 1×1 P2P `CallSession` deliberately does NOT use it (that path
 * is end-to-end DTLS-SRTP with no SFU in the middle).
 */

/** Operation the worker performs on the attached RTP stream. */
export type SFrameOperation = "encrypt" | "decrypt";

/**
 * Options handed to the worker via the `RTCRtpScriptTransform`
 * constructor. Structured-cloned across the thread boundary, so every
 * field must be cloneable (a `Uint8Array` is).
 */
export interface SFrameTransformOptions {
  /** `"encrypt"` for a sender stream, `"decrypt"` for a receiver. */
  readonly operation: SFrameOperation;
  /**
   * 32-byte per-call seed — the MLS exporter output
   * `Group.exportSecret("lakko/call/v1", callId, 32)`. Identical on
   * every participant in the same MLS epoch.
   */
  readonly seed: Uint8Array;
  /**
   * Local leaf index — the KID this client stamps into every frame it
   * seals. Required for `"encrypt"`; ignored for `"decrypt"` (the
   * receiver reads the KID off each inbound frame's header and derives
   * the matching per-sender key lazily).
   */
  readonly localLeaf?: number;
}

/**
 * `true` when the browser exposes the Encoded Transform API. Chromium
 * (114+) and Safari (Technology Preview) ship `RTCRtpScriptTransform`;
 * older / non-conforming engines return `false` and the attach
 * helpers below will refuse.
 */
export function isSFrameTransformSupported(): boolean {
  return typeof RTCRtpScriptTransform !== "undefined";
}

/** Lazily-constructed shared worker. One worker fans out to every
 *  sender / receiver transform in the page. */
let sharedWorker: Worker | null = null;

function transformWorker(): Worker {
  if (sharedWorker === null) {
    sharedWorker = new Worker(
      new URL("./sframe-transform-worker.ts", import.meta.url),
      { type: "module", name: "lakko-sframe" },
    );
  }
  return sharedWorker;
}

function refuseIfUnsupported(): void {
  if (!isSFrameTransformSupported()) {
    // Invariant #4: never silently fall back to plaintext media.
    throw new Error(
      "RTCRtpScriptTransform is unavailable in this browser — refusing " +
        "to open the call without SFrame end-to-end media encryption " +
        "(security invariant #4: no fallback to non-E2EE)",
    );
  }
}

/**
 * Attach the encrypting SFrame transform to an outgoing RTP sender.
 * Every frame the sender emits is sealed into an RFC 9605 wire frame
 * (`SframeHeader(kid=localLeaf, ctr) || ciphertext`) before it reaches
 * the SFU.
 *
 * Throws if the browser lacks `RTCRtpScriptTransform` (invariant #4)
 * or if `localLeaf` is omitted.
 */
export function attachSenderSFrame(
  sender: RTCRtpSender,
  opts: { readonly seed: Uint8Array; readonly localLeaf: number },
): void {
  refuseIfUnsupported();
  const options: SFrameTransformOptions = {
    operation: "encrypt",
    seed: opts.seed,
    localLeaf: opts.localLeaf,
  };
  sender.transform = new RTCRtpScriptTransform(transformWorker(), options);
}

/**
 * Attach the decrypting SFrame transform to an incoming RTP receiver.
 * Each inbound frame's header KID selects (and lazily derives) the
 * per-sender key; frames that fail the AEAD open are dropped, never
 * forwarded as plaintext.
 *
 * Throws if the browser lacks `RTCRtpScriptTransform` (invariant #4).
 */
export function attachReceiverSFrame(
  receiver: RTCRtpReceiver,
  opts: { readonly seed: Uint8Array },
): void {
  refuseIfUnsupported();
  const options: SFrameTransformOptions = {
    operation: "decrypt",
    seed: opts.seed,
  };
  receiver.transform = new RTCRtpScriptTransform(transformWorker(), options);
}

/**
 * Tear down the shared worker. Call when the last call ends so the
 * worker thread (and its loaded wasm instance) can be reclaimed. Safe
 * to call when no worker exists.
 */
export function disposeSFrameWorker(): void {
  if (sharedWorker !== null) {
    sharedWorker.terminate();
    sharedWorker = null;
  }
}
