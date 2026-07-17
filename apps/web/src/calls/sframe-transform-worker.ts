// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * SFrame transform worker — the per-frame seal / open engine behind
 * the W3C Encoded Transform API. Companion to the main-thread
 * controller in `./sframe-transform.ts` (see that file for the full
 * architecture + the native-shim parallel).
 *
 * This script runs OFF the main thread. The browser fires one
 * `rtctransform` event per `RTCRtpScriptTransform` the main thread
 * constructs; each event carries an `RTCRtpScriptTransformer` whose
 * `readable` / `writable` are a `TransformStream` of encoded media
 * frames plus the `options` the main thread passed.
 *
 * Per frame:
 *   * encrypt — `ParticipantKey.sealFramed(plaintext)` produces an
 *     RFC 9605 wire frame (`SframeHeader(kid, ctr) || ciphertext`).
 *     The header is the AEAD AAD (ADR-0013), so the output decrypts
 *     byte-for-byte on iOS / Android.
 *   * decrypt — read the KID off the inbound frame header
 *     (`unwrapSframeFrame`), then `CallKeyRing.openFramedWithOverlap`,
 *     which tries the current epoch's per-sender key and, during a
 *     mid-call rekey overlap window, falls back to the previous
 *     epoch's key (ADR-0029). A frame that fails to parse or
 *     authenticate under either key is DROPPED, never forwarded as
 *     plaintext (security invariant #4).
 *
 * Mid-call rekey (G-18, ADR-0029): each transform holds a
 * `CallKeyRing` (current + previous call key) keyed by the transform's
 * `id`. When the group's MLS epoch advances during a live call, the
 * main thread posts a `rekey` message with the freshly-derived seed;
 * the worker rotates the ring (retaining the previous key for the
 * overlap window), re-derives the encrypt side's seal key, and closes
 * the overlap window once it elapses.
 */

import { loadWasm } from "../wasm";
import type {
  SFrameMediaType,
  SFrameOperation,
  SFrameTransformOptions,
} from "./sframe-transform";
import type { CallKeyRing, ParticipantKey } from "../../wasm/lakko_ffi_wasm";

/** A single encoded media frame the transform rewrites in place.
 *  Both audio and video frames expose a settable `data: ArrayBuffer`. */
type EncodedFrame = RTCEncodedAudioFrame | RTCEncodedVideoFrame;

/**
 * Worker-side view of the Encoded Transform objects. These are part
 * of `DedicatedWorkerGlobalScope` and are NOT in TypeScript's bundled
 * lib definitions, so we declare the minimal surface we touch.
 */
interface RTCRtpScriptTransformer {
  readonly readable: ReadableStream<EncodedFrame>;
  readonly writable: WritableStream<EncodedFrame>;
  readonly options: unknown;
}

interface RTCTransformEvent extends Event {
  readonly transformer: RTCRtpScriptTransformer;
}

/**
 * Per-transform state, keyed by the transform's `id` so a `rekey`
 * message from the main thread can target the right stream. The stream
 * closures read `ring` (decrypt) / `sealKey` (encrypt) fresh each frame,
 * so a rekey takes effect on the next frame with no re-attach.
 */
interface TransformState {
  readonly operation: SFrameOperation;
  readonly localLeaf: number | undefined;
  /** ADR-0034 media type of this transform's stream (`0 = audio`, `1 = video`),
   *  fixed for the transform's lifetime and folded into every per-(leaf, media)
   *  key derivation on both the seal and open paths. */
  readonly mediaType: SFrameMediaType;
  readonly ring: CallKeyRing;
  /** Encrypt side only: the current epoch's seal key, re-derived on rekey. */
  sealKey: ParticipantKey | null;
}

/** Live transforms keyed by `SFrameTransformOptions.id`. */
const transforms = new Map<string, TransformState>();

/** Copy `bytes` into a fresh, exactly-sized `ArrayBuffer`. wasm-bindgen
 *  hands back a `Uint8Array` whose `.buffer` is typed `ArrayBufferLike`;
 *  the frame's `data` setter requires a concrete `ArrayBuffer`, so we
 *  normalise here rather than fight the type. */
function toArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const out = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(out).set(bytes);
  return out;
}

/** Build the encrypting `TransformStream`: seal every frame under the
 *  current epoch's local participant key (read fresh so a rekey swaps it). */
function encryptStream(
  state: TransformState,
): TransformStream<EncodedFrame, EncodedFrame> {
  return new TransformStream<EncodedFrame, EncodedFrame>({
    transform(frame, controller): void {
      const key = state.sealKey;
      if (key === null) return; // no leaf / not armed — never emit unsealed media
      let wire: Uint8Array;
      try {
        wire = key.sealFramed(new Uint8Array(frame.data));
      } catch {
        // Counter exhausted or AEAD failure — drop the frame rather than
        // emit unsealed media. A re-key (fresh MLS commit) is the recovery
        // path; the caller observes silence until it lands.
        return;
      }
      frame.data = toArrayBuffer(wire);
      controller.enqueue(frame);
    },
  });
}

/** Build the decrypting `TransformStream`: read each frame's header KID
 *  and open through the ring's overlap fall-back (current then, during a
 *  rekey window, previous). */
function decryptStream(
  state: TransformState,
  unwrapSenderLeaf: (wire: Uint8Array) => number | null,
): TransformStream<EncodedFrame, EncodedFrame> {
  return new TransformStream<EncodedFrame, EncodedFrame>({
    transform(frame, controller): void {
      const wire = new Uint8Array(frame.data);
      const senderLeaf = unwrapSenderLeaf(wire);
      if (senderLeaf === null) return; // malformed header — drop
      let plaintext: Uint8Array;
      try {
        // (senderLeaf, mediaType) together select the per-(leaf, media) key —
        // this transform's fixed media type keeps a leaf's audio and video
        // frames on distinct keys (ADR-0034).
        plaintext = state.ring.openFramedWithOverlap(
          senderLeaf,
          state.mediaType,
          wire,
        );
      } catch {
        // Wrong key / tampered / replayed counter, under BOTH the current
        // and the overlap-window previous key — drop, never forward
        // unverified bytes (invariant #4).
        return;
      }
      frame.data = toArrayBuffer(plaintext);
      controller.enqueue(frame);
    },
  });
}

/** Wire one transformer's readable → transform → writable, tracking its
 *  state under `opts.id` for the lifetime of the stream. */
async function runTransform(
  transformer: RTCRtpScriptTransformer,
  opts: SFrameTransformOptions,
): Promise<void> {
  const wasm = await loadWasm();
  const ring = new wasm.CallKeyRing(opts.seed);
  const state: TransformState = {
    operation: opts.operation,
    localLeaf: opts.localLeaf,
    mediaType: opts.mediaType,
    ring,
    sealKey: null,
  };

  let stream: TransformStream<EncodedFrame, EncodedFrame>;
  if (opts.operation === "encrypt") {
    if (opts.localLeaf === undefined) {
      // No leaf → cannot stamp a KID → cannot encrypt. Refuse by leaving
      // the pipe unconnected (no plaintext escapes).
      return;
    }
    try {
      state.sealKey = ring.participantKey(opts.localLeaf, opts.mediaType);
    } catch {
      return; // derivation failed — refuse rather than emit plaintext
    }
    stream = encryptStream(state);
  } else {
    stream = decryptStream(state, (wire) => {
      try {
        return wasm.unwrapSframeFrame(wire).senderLeaf;
      } catch {
        return null;
      }
    });
  }

  transforms.set(opts.id, state);
  try {
    await transformer.readable.pipeThrough(stream).pipeTo(transformer.writable);
  } finally {
    // Stream ended (track removed / call over) — drop the state so the ring
    // (and its keys) can be reclaimed and a stale id can't be rekeyed.
    transforms.delete(opts.id);
  }
}

/** Control message the main thread posts to rotate a transform's ring on a
 *  mid-call MLS epoch change (G-18, ADR-0029). */
interface RekeyMessage {
  readonly type: "rekey";
  readonly id: string;
  readonly seed: Uint8Array;
  readonly nowMs: number;
  readonly overlapMs: number;
}

function isRekeyMessage(v: unknown): v is RekeyMessage {
  if (typeof v !== "object" || v === null) return false;
  const m = v as Record<string, unknown>;
  return (
    m.type === "rekey" &&
    typeof m.id === "string" &&
    m.seed instanceof Uint8Array &&
    typeof m.nowMs === "number" &&
    typeof m.overlapMs === "number"
  );
}

addEventListener("message", (event: MessageEvent): void => {
  if (!isRekeyMessage(event.data)) return;
  const msg = event.data;
  const state = transforms.get(msg.id);
  if (state === undefined) return; // unknown / already-torn-down transform
  try {
    state.ring.rekeyFromSeedAt(msg.seed, BigInt(msg.nowMs), BigInt(msg.overlapMs));
  } catch {
    return;
  }
  if (state.operation === "encrypt" && state.localLeaf !== undefined) {
    try {
      state.sealKey = state.ring.participantKey(state.localLeaf, state.mediaType);
    } catch {
      state.sealKey = null; // refuse to seal under a bad key rather than plaintext
    }
  }
  // Close the overlap window (drop the previous key → full seed-boundary
  // forward secrecy, C-9 / C-10) once it has elapsed. The worker's own wall
  // clock ticks the prune, per ADR-0029.
  setTimeout(() => {
    try {
      state.ring.pruneExpired(BigInt(Date.now()));
    } catch {
      /* transform torn down before the window elapsed — nothing to prune */
    }
  }, msg.overlapMs + 200);
});

addEventListener("rtctransform", (event: Event): void => {
  const { transformer } = event as RTCTransformEvent;
  const opts = transformer.options as SFrameTransformOptions;
  void runTransform(transformer, opts);
});
