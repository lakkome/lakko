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
 *     (`unwrapSframeFrame`), lazily derive that sender's
 *     `ParticipantKey`, then `openFramed(wire)`. A frame that fails
 *     to parse or authenticate is DROPPED, never forwarded as
 *     plaintext (security invariant #4).
 */

import { loadWasm } from "../wasm";
import type { SFrameTransformOptions } from "./sframe-transform";
import type { ParticipantKey } from "../../wasm/lakko_ffi_wasm";

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
 *  local participant key. */
function encryptStream(
  participantKey: ParticipantKey,
): TransformStream<EncodedFrame, EncodedFrame> {
  return new TransformStream<EncodedFrame, EncodedFrame>({
    transform(frame, controller): void {
      let wire: Uint8Array;
      try {
        wire = participantKey.sealFramed(new Uint8Array(frame.data));
      } catch {
        // Counter exhausted or AEAD failure — drop the frame rather
        // than emit unsealed media. A re-key (fresh MLS commit) is the
        // recovery path; the caller observes silence until it lands.
        return;
      }
      frame.data = toArrayBuffer(wire);
      controller.enqueue(frame);
    },
  });
}

/** Build the decrypting `TransformStream`: read each frame's header
 *  KID, lazily derive that sender's key, and open. Maps one
 *  `CallKey` to many senders (the SFU fan-in shape). */
function decryptStream(
  deriveKey: (senderLeaf: number) => ParticipantKey | null,
  unwrapSenderLeaf: (wire: Uint8Array) => number | null,
): TransformStream<EncodedFrame, EncodedFrame> {
  const keys = new Map<number, ParticipantKey>();
  return new TransformStream<EncodedFrame, EncodedFrame>({
    transform(frame, controller): void {
      const wire = new Uint8Array(frame.data);
      const senderLeaf = unwrapSenderLeaf(wire);
      if (senderLeaf === null) return; // malformed header — drop
      let key = keys.get(senderLeaf);
      if (key === undefined) {
        const derived = deriveKey(senderLeaf);
        if (derived === null) return; // derivation failed — drop
        key = derived;
        keys.set(senderLeaf, key);
      }
      let plaintext: Uint8Array;
      try {
        plaintext = key.openFramed(wire);
      } catch {
        // Wrong key / tampered / replayed counter — drop, never
        // forward unverified bytes (invariant #4).
        return;
      }
      frame.data = toArrayBuffer(plaintext);
      controller.enqueue(frame);
    },
  });
}

/** Wire one transformer's readable → transform → writable. */
async function runTransform(
  transformer: RTCRtpScriptTransformer,
  opts: SFrameTransformOptions,
): Promise<void> {
  const wasm = await loadWasm();
  const callKey = new wasm.CallKey(opts.seed);

  let stream: TransformStream<EncodedFrame, EncodedFrame>;
  if (opts.operation === "encrypt") {
    if (opts.localLeaf === undefined) {
      // No leaf → cannot stamp a KID → cannot encrypt. Refuse by
      // leaving the pipe unconnected (no plaintext escapes).
      return;
    }
    stream = encryptStream(callKey.participantKey(opts.localLeaf));
  } else {
    stream = decryptStream(
      (senderLeaf) => {
        try {
          return callKey.participantKey(senderLeaf);
        } catch {
          return null;
        }
      },
      (wire) => {
        try {
          return wasm.unwrapSframeFrame(wire).senderLeaf;
        } catch {
          return null;
        }
      },
    );
  }

  await transformer.readable.pipeThrough(stream).pipeTo(transformer.writable);
}

addEventListener("rtctransform", (event: Event): void => {
  const { transformer } = event as RTCTransformEvent;
  const opts = transformer.options as SFrameTransformOptions;
  void runTransform(transformer, opts);
});
