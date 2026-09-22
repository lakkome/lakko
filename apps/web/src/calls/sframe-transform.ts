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
 * ADR-0035 per-stream index — the low 8 bits of the SFrame KID
 * `(sender_leaf << 8) | stream_index`. Unlike ADR-0034's local media-type
 * byte, this rides ON the wire (it is part of the KID) so the receiver reads
 * the whole KID off each frame's header and never needs a separate
 * discriminator. v1 has at most one video stream per participant, so:
 *   * `0` = the participant's audio stream,
 *   * `1` = the participant's (single) camera / video stream.
 * A second video / screen-share would take `2`, but v1 does not publish one.
 * Each `(sender_leaf, stream_index)` pair derives a distinct per-participant
 * key + nonce-salt space, so two concurrent streams from one leaf can never
 * collide on a `(key, nonce)` pair — the gap ADR-0034's per-media-TYPE tag left
 * open for two same-kind streams.
 */
export type SFrameStreamIndex = number;

/** Map an `RTCRtpSender` track `kind` to the ADR-0035 stream index. Anything
 *  that is not `"video"` seals as the audio stream (`0`); a WebRTC track kind is
 *  only ever `"audio"` or `"video"`. v1 publishes at most one video track per
 *  participant, so `"video" -> 1` is unambiguous. */
function streamIndexOfKind(kind: string): SFrameStreamIndex {
  return kind === "video" ? 1 : 0;
}

/**
 * Options handed to the worker via the `RTCRtpScriptTransform`
 * constructor. Structured-cloned across the thread boundary, so every
 * field must be cloneable (a `Uint8Array` is).
 */
export interface SFrameTransformOptions {
  /** `"encrypt"` for a sender stream, `"decrypt"` for a receiver. */
  readonly operation: SFrameOperation;
  /**
   * Unique id for this transform, assigned by the attach helper. The
   * worker keys its per-transform state on it so a mid-call `rekey`
   * message can target the right stream (G-18, ADR-0029).
   */
  readonly id: string;
  /**
   * 32-byte per-call seed — the MLS exporter output
   * `Group.exportSecret("lakko/call/v1", callId, 32)`. Identical on
   * every participant in the same MLS epoch.
   */
  readonly seed: Uint8Array;
  /**
   * Local leaf index — the high bits of the KID this client stamps into every
   * frame it seals. Required for `"encrypt"`; ignored for `"decrypt"` (the
   * receiver reads the whole KID off each inbound frame's header and derives
   * the matching per-stream key lazily).
   */
  readonly localLeaf?: number;
  /**
   * ADR-0035 stream index of this transform's RTP stream — `0 = audio`,
   * `1 = video`. Fixed for the transform's whole lifetime (each
   * `RTCRtpScriptTransform` carries exactly one track, hence one kind), so the
   * worker reads it once and composes the seal-side KID
   * `(localLeaf << 8) | streamIndex` from it. Required for `"encrypt"`; unset
   * for `"decrypt"` (the receiver takes the whole KID straight off the wire).
   */
  readonly streamIndex?: SFrameStreamIndex;
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
    // The worker's own console goes to a context the page's console does not
    // show and a devtools session has to be attached to separately, so a
    // receive side that discards every frame says nothing where anyone is
    // looking. Diagnostics are relayed here instead, where they sit beside
    // the call's own lines.
    sharedWorker.addEventListener("message", (ev: MessageEvent<unknown>) => {
      const data = ev.data;
      if (
        typeof data === "object" &&
        data !== null &&
        (data as { kind?: unknown }).kind === "sframe-diag"
      ) {
        // eslint-disable-next-line no-console
        console.warn(String((data as { text?: unknown }).text ?? ""));
      }
    });
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
 * (`SframeHeader(kid=(localLeaf << 8) | streamIndex, ctr) || ciphertext`)
 * before it reaches the SFU (ADR-0035).
 *
 * Throws if the browser lacks `RTCRtpScriptTransform` (invariant #4)
 * or if `localLeaf` is omitted.
 */
export function attachSenderSFrame(
  sender: RTCRtpSender,
  opts: { readonly seed: Uint8Array; readonly localLeaf: number },
): string {
  refuseIfUnsupported();
  const id = crypto.randomUUID();
  const options: SFrameTransformOptions = {
    operation: "encrypt",
    id,
    seed: opts.seed,
    localLeaf: opts.localLeaf,
    // The sender's track is set by the caller's `addTrack` before this attach
    // (the native-shim sequencing), so its kind is the authoritative stream
    // for every frame this transform seals: audio -> 0, video -> 1 (ADR-0035).
    streamIndex: streamIndexOfKind(sender.track?.kind ?? "audio"),
  };
  sender.transform = new RTCRtpScriptTransform(transformWorker(), options);
  return id;
}

/**
 * Attach the decrypting SFrame transform to an incoming RTP receiver.
 * Each inbound frame's header KID `(sender_leaf << 8) | stream_index`
 * selects (and lazily derives) the per-stream key; frames that fail the
 * AEAD open are dropped, never forwarded as plaintext.
 *
 * Throws if the browser lacks `RTCRtpScriptTransform` (invariant #4).
 */
export function attachReceiverSFrame(
  receiver: RTCRtpReceiver,
  opts: { readonly seed: Uint8Array },
): string {
  refuseIfUnsupported();
  const id = crypto.randomUUID();
  const options: SFrameTransformOptions = {
    operation: "decrypt",
    id,
    seed: opts.seed,
    // No stream index on the receive side: each inbound frame's header carries
    // the whole KID `(sender_leaf << 8) | stream_index`, which the worker reads
    // per frame to select (and lazily derive) the right per-stream key. One
    // decrypt transform therefore serves every sender + every stream (ADR-0035).
  };
  receiver.transform = new RTCRtpScriptTransform(transformWorker(), options);
  return id;
}

/** Default SFrame overlap window in ms — mirrors `OVERLAP_DEFAULT_MS` in the
 *  Rust core (ADR-0008 / ADR-0029). */
const SFRAME_OVERLAP_MS = 1000;

/**
 * Rotate a previously-attached transform's call key on a mid-call MLS epoch
 * change (G-18, ADR-0029). The worker retains the previous key for the overlap
 * window so in-flight frames sealed under the old epoch still open, then drops
 * it. `id` is the value returned by `attachSenderSFrame` /
 * `attachReceiverSFrame`. No-op if the SFrame worker was never started.
 */
export function rekeySFrame(
  id: string,
  seed: Uint8Array,
  overlapMs = SFRAME_OVERLAP_MS,
): void {
  if (sharedWorker === null) return;
  sharedWorker.postMessage({
    type: "rekey",
    id,
    seed,
    nowMs: Date.now(),
    overlapMs,
  });
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
