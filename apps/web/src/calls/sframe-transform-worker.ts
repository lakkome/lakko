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
  SFrameOperation,
  SFrameStreamIndex,
  SFrameTransformOptions,
} from "./sframe-transform";
import type { CallKeyRing, ParticipantKey } from "../../wasm/lakko_ffi_wasm";
import { sealingStream, toArrayBuffer } from "./sframe-seal-stream";

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
  /** ADR-0035 stream index of this transform's stream (`0 = audio`,
   *  `1 = video`), fixed for the transform's lifetime. Encrypt side only: the
   *  worker composes the seal-side KID `(localLeaf << 8) | streamIndex` from it.
   *  Unset on the decrypt side (the receiver reads the whole KID off the wire). */
  readonly streamIndex: SFrameStreamIndex | undefined;
  readonly ring: CallKeyRing;
  /** Encrypt side only: the current epoch's seal key, re-derived on rekey. */
  sealKey: ParticipantKey | null;
  /** Decrypt side only: what became of each frame (`noteDecrypt`). */
  readonly decryptCounts: Partial<Record<"opened" | "badHeader" | "openFailed", number>>;
}

/** Live transforms keyed by `SFrameTransformOptions.id`. */
const transforms = new Map<string, TransformState>();

/** Transforms whose core is still loading: created, not yet in `transforms`. */
const starting = new Set<string>();

/** The newest rekey that arrived for a transform still in `starting`. */
const pendingRekeys = new Map<string, RekeyMessage>();

/** Build the encrypting `TransformStream`: seal every frame under the
 *  current epoch's local participant key (read fresh so a rekey swaps it).
 *  A simulcast sender's layers all come through this one stream and share its
 *  one key and counter; see `./sframe-seal-stream.ts`. */
function encryptStream(
  state: TransformState,
): TransformStream<EncodedFrame, EncodedFrame> {
  return sealingStream<EncodedFrame>(state);
}

/** Relay one diagnostic line to the main thread, which logs it where the
 *  call's own lines are. A worker's console is a separate context nobody is
 *  watching during a call. */
function diag(text: string): void {
  try {
    (self as unknown as { postMessage: (m: unknown) => void }).postMessage({
      kind: "sframe-diag",
      text,
    });
  } catch {
    // A relay that cannot report must not take the media path down with it.
  }
}

/**
 * Count what the receive side does with each frame, and say it - once at the
 * first of each kind, then rarely.
 *
 * The two drops in `decryptStream` were silent, so from outside a call whose
 * frames all failed to open looked exactly like one where no media arrived at
 * all. Those are different defects with different fixes, and the counters are
 * what separate them. Bounded on purpose: the first of each kind, then every
 * 250th, so a call that drops everything costs a handful of lines rather than
 * fifty a second.
 */
function noteDecrypt(
  state: TransformState,
  what: "opened" | "badHeader" | "openFailed",
  kid?: bigint,
): void {
  const seen = (state.decryptCounts[what] ?? 0) + 1;
  state.decryptCounts[what] = seen;
  if (seen !== 1 && seen % 250 !== 0) return;
  const counts = state.decryptCounts;
  diag(
    `sframe receive ${what}${kid === undefined ? "" : ` kid=${kid}`}: ` +
      `opened=${counts.opened ?? 0} badHeader=${counts.badHeader ?? 0} ` +
      `openFailed=${counts.openFailed ?? 0}`,
  );
}

/** Build the decrypting `TransformStream`: read each frame's whole header
 *  KID `(sender_leaf << 8) | stream_index` and open through the ring's overlap
 *  fall-back (current then, during a rekey window, previous). */
function decryptStream(
  state: TransformState,
  unwrapKid: (wire: Uint8Array) => bigint | null,
): TransformStream<EncodedFrame, EncodedFrame> {
  return new TransformStream<EncodedFrame, EncodedFrame>({
    transform(frame, controller): void {
      const wire = new Uint8Array(frame.data);
      const kid = unwrapKid(wire);
      if (kid === null) {
        // Malformed header - drop. Counted and said out loud, because a
        // receiver that silently discards every frame is indistinguishable
        // from one that never received any: both render as a call that
        // connects and then dies of silence.
        noteDecrypt(state, "badHeader");
        return;
      }
      let plaintext: Uint8Array;
      try {
        // The whole KID selects the per-(leaf, stream) key straight off the
        // wire — no separate media discriminator. Each of a participant's
        // streams (audio, video, …) carries a distinct KID, so their keys
        // never collide (ADR-0035).
        plaintext = state.ring.openFramedWithOverlap(kid, wire);
      } catch {
        // Wrong key / tampered / replayed counter, under BOTH the current
        // and the overlap-window previous key — drop, never forward
        // unverified bytes (invariant #4).
        noteDecrypt(state, "openFailed", kid);
        return;
      }
      frame.data = toArrayBuffer(plaintext);
      controller.enqueue(frame);
      noteDecrypt(state, "opened", kid);
    },
  });
}

/** Wire one transformer's readable → transform → writable, tracking its
 *  state under `opts.id` for the lifetime of the stream. */
async function runTransform(
  transformer: RTCRtpScriptTransformer,
  opts: SFrameTransformOptions,
): Promise<void> {
  starting.add(opts.id);
  let wasm: Awaited<ReturnType<typeof loadWasm>>;
  try {
    wasm = await loadWasm();
  } catch (err) {
    starting.delete(opts.id);
    pendingRekeys.delete(opts.id);
    throw err;
  }
  // Said once per transform, so the absence of the per-frame lines below can
  // be read: with this line present and no `sframe receive` lines after it,
  // no frame ever reached the transform, which is a different defect from
  // frames arriving and failing to open.
  diag(
    `sframe transform armed: op=${opts.operation} id=${opts.id} ` +
      `leaf=${opts.localLeaf ?? "-"} stream=${opts.streamIndex ?? "-"}`,
  );
  const ring = new wasm.CallKeyRing(opts.seed);
  const state: TransformState = {
    operation: opts.operation,
    localLeaf: opts.localLeaf,
    streamIndex: opts.streamIndex,
    ring,
    sealKey: null,
    decryptCounts: {},
  };

  let stream: TransformStream<EncodedFrame, EncodedFrame>;
  if (opts.operation === "encrypt") {
    if (opts.localLeaf === undefined) {
      // No leaf → cannot stamp a KID → cannot encrypt. Refuse by leaving
      // the pipe unconnected (no plaintext escapes).
      starting.delete(opts.id);
      pendingRekeys.delete(opts.id);
      return;
    }
    try {
      // Compose the seal-side KID `(localLeaf << 8) | streamIndex` (audio -> 0,
      // video -> 1). The ring's `participantKey` does the composition; a stream
      // index > 255 fails closed FFI-side (ADR-0035).
      state.sealKey = ring.participantKey(opts.localLeaf, opts.streamIndex ?? 0);
    } catch {
      starting.delete(opts.id);
      pendingRekeys.delete(opts.id);
      return; // derivation failed — refuse rather than emit plaintext
    }
    stream = encryptStream(state);
  } else {
    stream = decryptStream(state, (wire) => {
      try {
        // The FFI `unwrapSframeFrame` reads the whole SFrame header KID; its
        // `senderLeaf` getter now carries the full composite KID
        // `(sender_leaf << 8) | stream_index` (ADR-0035), which the ring uses to
        // derive the per-stream key. Cross to a BigInt for the u64 FFI param.
        return BigInt(wasm.unwrapSframeFrame(wire).senderLeaf);
      } catch {
        return null;
      }
    });
  }

  transforms.set(opts.id, state);
  starting.delete(opts.id);
  const pending = pendingRekeys.get(opts.id);
  pendingRekeys.delete(opts.id);
  if (pending !== undefined) applyRekey(state, pending);
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
  if (state === undefined) {
    // A transform that is still loading the core keeps the newest rekey and
    // applies it once its state exists; without this the rekey is lost and
    // the transform seals or opens under the seed it was created with.
    if (starting.has(msg.id)) pendingRekeys.set(msg.id, msg);
    return; // unknown / already-torn-down transform
  }
  applyRekey(state, msg);
});

/** Rotate one transform's ring to `msg.seed` and re-derive its seal key. */
function applyRekey(state: TransformState, msg: RekeyMessage): void {
  try {
    state.ring.rekeyFromSeedAt(msg.seed, BigInt(msg.nowMs), BigInt(msg.overlapMs));
  } catch {
    return;
  }
  if (state.operation === "encrypt" && state.localLeaf !== undefined) {
    try {
      state.sealKey = state.ring.participantKey(
        state.localLeaf,
        state.streamIndex ?? 0,
      );
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
}

addEventListener("rtctransform", (event: Event): void => {
  const { transformer } = event as RTCTransformEvent;
  const opts = transformer.options as SFrameTransformOptions;
  void runTransform(transformer, opts);
});
