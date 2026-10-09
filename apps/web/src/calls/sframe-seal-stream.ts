// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The per-frame seal and open of the SFrame transform worker, apart from the
 * worker's event wiring so they can run under a test without a browser.
 *
 * Frame layout (ADR-0071): `clear_prefix || ciphertext_and_tag ||
 * sframe_header || trailer`. The codec's own frame header stays in the clear
 * at the start, the SFrame header (KID, counter) sits at the end before a
 * 3-byte trailer, and every clear byte is authenticated. The layout, the
 * clear-prefix rule per codec and the AEAD are the Rust core's; this file
 * only hands it frames.
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

import { MEDIA_CODEC_OTHER, type CodecFrame } from "./sframe-codec";

/** The part of an encoded audio or video frame the transform rewrites. */
export interface SealableFrame extends CodecFrame {
  data: ArrayBuffer;
}

/** The part of the participant key the transform uses. Each call consumes one
 *  counter value. `codec` is the core's `MediaCodec` value; it decides how
 *  many leading bytes of the frame stay in the clear. */
export interface FrameSealer {
  sealMediaFrame(frame: Uint8Array, codec: number): Uint8Array;
}

/** Where the transform reads its current key. Read fresh for every frame, so
 *  a mid-call rekey takes effect on the next one. */
export interface SealState {
  sealKey: FrameSealer | null;
  /** The codec of a frame (`./sframe-codec.ts`). Absent, every frame is
   *  sealed as `MediaCodec.Other`: nothing in the clear. */
  codecOf?: (frame: CodecFrame, bytes: Uint8Array) => number;
  /**
   * Whether `frame` has to wait before it is sealed: `null` when it can be
   * sealed now, else a promise that settles when it can (the sender's codec
   * table arrived, or the wait ran out). See `CodecResolver.ready` in
   * `./sframe-codec.ts`. Absent, no frame waits.
   */
  ready?: (frame: CodecFrame) => Promise<void> | null;
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
    const bytes = new Uint8Array(frame.data);
    const codec = state.codecOf === undefined ? MEDIA_CODEC_OTHER : state.codecOf(frame, bytes);
    wire = key.sealMediaFrame(bytes, codec);
  } catch {
    return false;
  }
  frame.data = toArrayBuffer(wire);
  return true;
}

/**
 * The encrypting `TransformStream`: every frame sealed under the current
 * key, or dropped.
 *
 * A frame the state says is not ready (`SealState.ready`: a video frame
 * whose codec is not known yet) is HELD until it is, and the frames behind
 * it wait their turn: a `TransformStream` runs one `transform` at a time, so
 * order is kept and nothing is sealed out of sequence. Whatever the wait's
 * outcome the frame is then sealed or dropped as any other; it is never
 * passed on unsealed (security invariant #4).
 */
export function sealingStream<F extends SealableFrame>(state: SealState): TransformStream<F, F> {
  const seal = (frame: F, controller: TransformStreamDefaultController<F>): void => {
    // A re-key (fresh MLS commit) is the recovery path for a failed seal;
    // the far end observes silence until it lands.
    if (sealFrame(state, frame)) controller.enqueue(frame);
  };
  return new TransformStream<F, F>({
    transform(frame, controller): void | Promise<void> {
      const wait = state.ready === undefined ? null : state.ready(frame);
      if (wait === null) {
        seal(frame, controller);
        return;
      }
      // Sealed when the wait settles, however it settles.
      return wait.then(
        () => seal(frame, controller),
        () => seal(frame, controller),
      );
    },
  });
}

/** The part of the key ring the receive side uses: split the frame by its
 *  trailer, select the key by the KID in its SFrame header, open under the
 *  current key or, inside a rekey overlap window, the previous one. */
export interface FrameOpener {
  openMediaFrameWithOverlap(wire: Uint8Array): Uint8Array;
}

/** Reads the KID off a sealed frame without a key; throws when the bytes are
 *  not a well-formed sealed media frame. */
export type FrameKidReader = (wire: Uint8Array) => bigint;

/** What became of one received frame. */
export type OpenOutcome =
  | { readonly what: "opened"; readonly kid: bigint }
  | { readonly what: "badHeader"; readonly reason: string }
  | { readonly what: "openFailed"; readonly kid: bigint; readonly reason: string };

/** The message of whatever was thrown, for the diagnostics only. */
function reasonOf(err: unknown): string {
  if (err instanceof Error) return err.message;
  return typeof err === "string" ? err : "unknown";
}

/**
 * Open one received frame in place. On `"opened"` the frame's `data` is the
 * encoded frame as the sender's encoder produced it (the clear prefix followed
 * by the decrypted rest). On anything else `data` is untouched and the caller
 * must DROP the frame: a frame that does not parse or authenticate is never
 * passed on (security invariant #4).
 */
export function openFrame(
  ring: FrameOpener,
  readKid: FrameKidReader,
  frame: { data: ArrayBuffer },
): OpenOutcome {
  const wire = new Uint8Array(frame.data);
  let kid: bigint;
  try {
    kid = readKid(wire);
  } catch (err) {
    return { what: "badHeader", reason: reasonOf(err) };
  }
  let opened: Uint8Array;
  try {
    opened = ring.openMediaFrameWithOverlap(wire);
  } catch (err) {
    return { what: "openFailed", kid, reason: reasonOf(err) };
  }
  frame.data = toArrayBuffer(opened);
  return { what: "opened", kid };
}
