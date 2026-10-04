// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The sealing half of the SFrame transform worker, apart from the worker's
 * event wiring so it can run under a test without a browser.
 *
 * One RTP sender has one transform, and so one stream of this kind, one
 * participant key and one frame counter. That holds when the sender sends
 * simulcast: every encoded frame of every layer comes through this one
 * stream, in the order the encoders produced them, and each is sealed with
 * the next counter value of the one key. Two layers therefore never share a
 * counter (which under one key would be a repeated AEAD nonce), and each
 * layer's frames carry rising counters with gaps where the other layer's
 * frames took a value. The receive side opens statelessly, so gaps are fine.
 *
 * The frame object itself is passed on with only its `data` replaced: its
 * metadata, which is how the browser knows which layer's RTP stream a frame
 * belongs to, is not touched.
 */

/** The part of an encoded audio or video frame the transform rewrites. */
export interface SealableFrame {
  data: ArrayBuffer;
}

/** The part of the participant key the transform uses. Each call consumes one
 *  counter value. */
export interface FrameSealer {
  sealFramed(plaintext: Uint8Array): Uint8Array;
}

/** Where the transform reads its current key. Read fresh for every frame, so
 *  a mid-call rekey takes effect on the next one. */
export interface SealState {
  sealKey: FrameSealer | null;
}

/** Copy `bytes` into a fresh, exactly-sized `ArrayBuffer`. wasm-bindgen
 *  hands back a `Uint8Array` whose `.buffer` is typed `ArrayBufferLike`;
 *  the frame's `data` setter requires a concrete `ArrayBuffer`. */
export function toArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  const out = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(out).set(bytes);
  return out;
}

/**
 * Seal one frame in place. Returns `false` when the frame must be dropped:
 * no key, or the seal failed (counter exhausted, AEAD failure). A dropped
 * frame is never passed on unsealed (security invariant #4).
 */
export function sealFrame(state: SealState, frame: SealableFrame): boolean {
  const key = state.sealKey;
  if (key === null) return false;
  let wire: Uint8Array;
  try {
    wire = key.sealFramed(new Uint8Array(frame.data));
  } catch {
    return false;
  }
  frame.data = toArrayBuffer(wire);
  return true;
}

/** The encrypting `TransformStream`: every frame sealed under the current
 *  key, or dropped. */
export function sealingStream<F extends SealableFrame>(state: SealState): TransformStream<F, F> {
  return new TransformStream<F, F>({
    transform(frame, controller): void {
      // A re-key (fresh MLS commit) is the recovery path for a failed seal;
      // the far end observes silence until it lands.
      if (sealFrame(state, frame)) controller.enqueue(frame);
    },
  });
}
