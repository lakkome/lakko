// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The SFrame transform worker's engine, apart from the worker's own event
 * wiring (`./sframe-transform-worker.ts`) so that it can run under a test
 * without a browser and so that BOTH ways a browser hands the worker a stream
 * of encoded frames end in the same code:
 *
 *   * the W3C Encoded Transform API: an `rtctransform` event carrying an
 *     `RTCRtpScriptTransformer` ({@link SFrameWorkerCore.onTransformer});
 *   * the earlier insertable-streams interface: a message carrying the
 *     readable / writable pair of `createEncodedStreams()`, transferred from
 *     the main thread, with the same options
 *     ({@link SFrameWorkerCore.onMessage}, `type: "encoded-streams"`).
 *
 * Both call {@link SFrameWorkerCore.start} with a readable, a writable and
 * the options; nothing after that point knows which one it was. The seal
 * (`sealingStream`), the open (`openFrame`), the codec resolver and its hold,
 * the receive ledger, the rekey with its overlap window and the stream index
 * per sender are therefore one implementation. Which interface is used is
 * decided on the main thread, in `./sframe-transport.ts`.
 *
 * Per frame (ADR-0071 layout: `clear_prefix || ciphertext_and_tag ||
 * sframe_header || trailer`):
 *   * encrypt - `ParticipantKey.sealMediaFrame(frame, codec)`. The codec's
 *     own frame header stays in the clear at the start of the frame (10 bytes
 *     of a VP8 key frame, 3 of any other VP8 frame, none for audio or an
 *     unknown codec); the rest is sealed, and the SFrame header, the clear
 *     bytes and the trailer are the AEAD additional data. Which codec a frame
 *     is of comes from `./sframe-codec.ts`.
 *   * decrypt - read the KID off the SFrame header at the END of the inbound
 *     frame (`parseMediaFrame`), then `CallKeyRing.openMediaFrameWithOverlap`,
 *     which tries the current epoch's per-stream key and, during a mid-call
 *     rekey overlap window, the previous epoch's (ADR-0029). The receiver
 *     needs no codec.
 *
 * Security invariant #4, on either interface: a frame that cannot be sealed
 * or opened is DROPPED, never passed on. A transform that cannot be built
 * (the core did not load, no leaf, a key that does not derive) leaves its
 * readable unconnected to its writable, so nothing flows; there is no
 * pass-through anywhere in this file.
 *
 * Mid-call rekey (G-18, ADR-0029): each transform holds a `CallKeyRing`
 * (current + previous call key) keyed by the transform's `id`. The main
 * thread posts a `rekey` message with the freshly derived seed; the ring
 * rotates (keeping the previous key for the overlap window), the encrypt
 * side's seal key is re-derived, and the overlap window is closed once it
 * elapses. A `rekey` or `codecs` message addresses a transform by `id` only,
 * so it reaches a transform created either way.
 */

import type {
  SFrameOperation,
  SFrameStreamIndex,
  SFrameTransformOptions,
} from "./sframe-transform";
import type { CallKeyRing, ParticipantKey } from "../../wasm/lakko_ffi_wasm";
import { createCodecResolver, type CodecResolver, type SenderCodecEntry } from "./sframe-codec";
import {
  openFrame,
  sealingStream,
  type FrameInfoReader,
  type RedReceive,
  type SealableFrame,
} from "./sframe-seal-stream";
import { ReceiveLedger, receivedFrameInfo, type InfoFrame } from "./sframe-receive-ledger";

/** A single encoded media frame the transform rewrites in place: an
 *  `RTCEncodedAudioFrame` or `RTCEncodedVideoFrame`, cut to what is read. */
export type EncodedFrame = SealableFrame & InfoFrame;

/** A readable / writable pair of encoded frames, however it arrived. */
export interface EncodedStreams {
  readonly readable: ReadableStream<EncodedFrame>;
  readonly writable: WritableStream<EncodedFrame>;
}

/** How a transform's streams reached the worker (for the diagnostics only). */
export type SFrameStreamSource = "script-transform" | "encoded-streams";

/** The part of the wasm core the worker uses. */
export interface SFrameWorkerWasm {
  readonly CallKeyRing: new (seed: Uint8Array) => CallKeyRing;
  readonly MediaCodec: { readonly Vp8: number };
  mediaCodecFromMime(mime: string): number;
  vp8IsKeyFrame(firstBytes: Uint8Array): boolean | undefined;
  parseMediaFrame(wire: Uint8Array): { readonly kid: bigint; readonly isRedOpus: boolean };
  redPrimary(container: Uint8Array): Uint8Array | undefined;
}

/**
 * How a receive transform treats a frame its sender marked as an RFC 2198
 * RED container of Opus (the ADR-0071 trailer, amended 2026-10-26).
 *
 * A browser that negotiated `audio/red` decodes the container itself, and
 * the frame is handed on whole. One that did not (Firefox has no RED) is
 * sent the container under its Opus payload type, because the SFU cannot
 * unwrap a sealed frame, and its decoder must be given the primary frame
 * alone. Which of the two a frame goes to is read off the frame's own
 * metadata where the browser names the codec; else from whether this
 * browser decodes RED at all (`browserDecodesRed`, which the main thread
 * reads, as a worker has no `RTCRtpReceiver`).
 */
export function redReceive(
  wasm: Pick<SFrameWorkerWasm, "redPrimary">,
  browserDecodesRed: boolean,
): RedReceive {
  return {
    wantsPrimary(frame): boolean {
      const mime = frameMimeType(frame);
      if (mime !== undefined) return mime.toLowerCase() !== "audio/red";
      return !browserDecodesRed;
    },
    primary: (container) => wasm.redPrimary(container),
  };
}

function frameMimeType(frame: object): string | undefined {
  const getMetadata = (frame as { getMetadata?: () => unknown }).getMetadata;
  if (typeof getMetadata !== "function") return undefined;
  try {
    const meta = getMetadata.call(frame) as { mimeType?: unknown } | null;
    return typeof meta?.mimeType === "string" && meta.mimeType !== "" ? meta.mimeType : undefined;
  } catch {
    return undefined;
  }
}

/** What the engine needs from its host (the worker, or a test). */
export interface SFrameWorkerDeps {
  /** Load the wasm core. Called once per transform; the host caches. */
  readonly loadWasm: () => Promise<SFrameWorkerWasm>;
  /** Post one message to the main thread. */
  readonly post: (message: unknown) => void;
  /** Run `fn` after `ms`. Left out, the global `setTimeout`. */
  readonly setTimeout?: (fn: () => void, ms: number) => unknown;
  /** The wall clock, milliseconds. Left out, `Date.now`. */
  readonly now?: () => number;
}

/** The message that carries a `createEncodedStreams()` pair to the worker.
 *  `readable` and `writable` are in the message's transfer list. */
export interface EncodedStreamsMessage extends EncodedStreams {
  readonly type: "encoded-streams";
  readonly options: SFrameTransformOptions;
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

/** The main thread's answer to "which codecs does this sender send": the
 *  payload type -> MIME type table of `RTCRtpSender.getParameters().codecs`
 *  (ADR-0071; see `./sframe-codec.ts`). */
interface CodecsMessage {
  readonly type: "codecs";
  readonly id: string;
  readonly codecs: readonly SenderCodecEntry[];
}

/**
 * Per-transform state, keyed by the transform's `id` so a `rekey` message
 * from the main thread can target the right stream. The stream closures read
 * `ring` (decrypt) / `sealKey` (encrypt) fresh each frame, so a rekey takes
 * effect on the next frame with no re-attach.
 */
interface TransformState {
  readonly operation: SFrameOperation;
  localLeaf: number | undefined;
  /** ADR-0035 stream index of this transform's stream (`0 = audio`,
   *  `1 = camera`, `3 = the camera's light sender`, `4 = its middle sender`).
   *  Encrypt side only: the seal-side KID is `(localLeaf << 8) |
   *  streamIndex`. Unset on the decrypt side (the receiver reads the whole
   *  KID off the wire). It changes only by a `retarget` message, which also
   *  replaces the seal key. */
  streamIndex: SFrameStreamIndex | undefined;
  readonly ring: CallKeyRing;
  /** Encrypt side only: the current epoch's seal key, re-derived on rekey. */
  sealKey: ParticipantKey | null;
  /** Encrypt side only: which codec each outgoing frame is of (ADR-0071). */
  codecs: CodecResolver | null;
  /** Encrypt side only: `codecs.codecOf`, read by the sealing stream. */
  codecOf?: CodecResolver["codecOf"];
  /** Encrypt side only: `codecs.ready`, read by the sealing stream: a video
   *  frame whose codec is not known yet is held, not sealed blind. */
  ready?: CodecResolver["ready"];
  /** Decrypt side only: what became of each frame, and what the frames that
   *  did not open were (`./sframe-receive-ledger.ts`). */
  readonly received: ReceiveLedger;
}

/**
 * The main thread re-attached a sender whose streams this worker already
 * holds, under another stream index (insertable-streams path: a sender's
 * streams can be taken once, so a sender the browser hands out again for a
 * different role keeps its transform and the transform takes the new role).
 * The seal key is replaced by the one of the new `(leaf, stream)`; no frame
 * is ever sealed under a mix of the two.
 */
interface RetargetMessage {
  readonly type: "retarget";
  readonly id: string;
  readonly localLeaf: number;
  readonly streamIndex: number;
}

function isRetargetMessage(v: unknown): v is RetargetMessage {
  if (!isRecord(v)) return false;
  return (
    v.type === "retarget" &&
    typeof v.id === "string" &&
    typeof v.localLeaf === "number" &&
    typeof v.streamIndex === "number"
  );
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null;
}

function isRekeyMessage(v: unknown): v is RekeyMessage {
  if (!isRecord(v)) return false;
  return (
    v.type === "rekey" &&
    typeof v.id === "string" &&
    v.seed instanceof Uint8Array &&
    typeof v.nowMs === "number" &&
    typeof v.overlapMs === "number"
  );
}

function isCodecsMessage(v: unknown): v is CodecsMessage {
  if (!isRecord(v)) return false;
  return v.type === "codecs" && typeof v.id === "string" && Array.isArray(v.codecs);
}

/** Whether `v` is a usable set of transform options. Anything else is
 *  refused: its streams are left unconnected, so nothing flows. */
export function isSFrameTransformOptions(v: unknown): v is SFrameTransformOptions {
  if (!isRecord(v)) return false;
  return (
    (v.operation === "encrypt" || v.operation === "decrypt") &&
    typeof v.id === "string" &&
    v.id !== "" &&
    v.seed instanceof Uint8Array
  );
}

function isStreamPair(v: Record<string, unknown>): boolean {
  const r = v.readable;
  const w = v.writable;
  return (
    isRecord(r) &&
    typeof (r as { pipeThrough?: unknown }).pipeThrough === "function" &&
    isRecord(w) &&
    typeof (w as { getWriter?: unknown }).getWriter === "function"
  );
}

function isEncodedStreamsMessage(v: unknown): v is EncodedStreamsMessage {
  if (!isRecord(v)) return false;
  return v.type === "encoded-streams" && isSFrameTransformOptions(v.options) && isStreamPair(v);
}

/** The engine one worker runs. */
export interface SFrameWorkerCore {
  /** A message from the main thread: `rekey`, `codecs`, `retarget`, or
   *  `encoded-streams` (a new transform over transferred streams). Anything
   *  else is ignored. */
  onMessage(data: unknown): void;
  /** An `rtctransform` event's transformer: a new transform over its
   *  streams, with the options the main thread constructed it with. */
  onTransformer(transformer: EncodedStreams & { readonly options: unknown }): void;
  /**
   * Run one transform: readable -> seal or open -> writable. Resolves when
   * the stream ends; rejects when the core cannot be loaded or the pipe
   * breaks. `onMessage` and `onTransformer` call this and report a rejection
   * as a diagnostic line.
   */
  start(
    streams: EncodedStreams,
    opts: SFrameTransformOptions,
    source: SFrameStreamSource,
  ): Promise<void>;
  /** How many transforms are live (built and piping). For tests. */
  liveCount(): number;
}

export function createSFrameWorkerCore(deps: SFrameWorkerDeps): SFrameWorkerCore {
  const later = deps.setTimeout ?? ((fn: () => void, ms: number): unknown => setTimeout(fn, ms));
  const now = deps.now ?? ((): number => Date.now());

  /** Live transforms keyed by `SFrameTransformOptions.id`. */
  const transforms = new Map<string, TransformState>();
  /** Transforms whose core is still loading: created, not yet in `transforms`. */
  const starting = new Set<string>();
  /** The newest rekey that arrived for a transform still in `starting`. */
  const pendingRekeys = new Map<string, RekeyMessage>();
  /** The newest codec table that arrived for a transform still in `starting`. */
  const pendingCodecs = new Map<string, readonly SenderCodecEntry[]>();
  /** The newest retarget that arrived for a transform still in `starting`. */
  const pendingRetargets = new Map<string, RetargetMessage>();

  /** Relay one diagnostic line to the main thread, which logs it where the
   *  call's own lines are. A worker's console is a separate context nobody is
   *  watching during a call. */
  function diag(text: string): void {
    try {
      deps.post({ kind: "sframe-diag", text });
    } catch {
      // A relay that cannot report must not take the media path down with it.
    }
  }

  /** Ask the main thread for a sender's codec table. */
  function wantCodecs(id: string): void {
    try {
      deps.post({ kind: "sframe-codecs-wanted", id });
    } catch {
      // Nothing to do: the frames stay sealed with no clear prefix.
    }
  }

  /** Build the decrypting `TransformStream`: split each frame by its trailer,
   *  read the whole KID `(sender_leaf << 8) | stream_index` from the SFrame
   *  header at its end and open through the ring's overlap fall-back (current
   *  then, during a rekey window, previous).
   *
   *  What becomes of each frame is counted and said - at the first of each
   *  outcome, then every 250th - because a receiver that silently discards
   *  frames is indistinguishable from one that never received any, and
   *  because the frames that do not open have to be told apart by what they
   *  are (`./sframe-receive-ledger.ts`). */
  function decryptStream(
    state: TransformState,
    readInfo: FrameInfoReader,
    red: RedReceive,
  ): TransformStream<EncodedFrame, EncodedFrame> {
    return new TransformStream<EncodedFrame, EncodedFrame>({
      transform(frame, controller): void {
        // The bytes as received: `openFrame` gives the frame a new buffer
        // when it opens, and leaves this one alone.
        const wire = new Uint8Array(frame.data);
        const info = receivedFrameInfo(frame);
        // Anything but "opened" is a drop: a malformed frame, or one that
        // authenticates under neither the current nor the overlap-window
        // previous key, is never forwarded (invariant #4).
        const outcome = openFrame(state.ring, readInfo, frame, red);
        let line: string | null;
        if (outcome.what === "opened") {
          controller.enqueue(frame);
          line = state.received.note("opened", wire, info, outcome.kid);
        } else if (outcome.what === "openFailed") {
          line = state.received.note("openFailed", wire, info, outcome.kid, outcome.reason);
        } else {
          line = state.received.note("badHeader", wire, info, undefined, outcome.reason);
        }
        if (line !== null) diag(line);
      },
    });
  }

  /** Rotate one transform's ring to `msg.seed` and re-derive its seal key. */
  function applyRekey(state: TransformState, msg: RekeyMessage): void {
    try {
      state.ring.rekeyFromSeedAt(msg.seed, BigInt(msg.nowMs), BigInt(msg.overlapMs));
    } catch {
      return;
    }
    if (state.operation === "encrypt" && state.localLeaf !== undefined) {
      try {
        state.sealKey = state.ring.participantKey(state.localLeaf, state.streamIndex ?? 0);
      } catch {
        state.sealKey = null; // refuse to seal under a bad key rather than plaintext
      }
    }
    // Close the overlap window (drop the previous key -> full seed-boundary
    // forward secrecy, C-9 / C-10) once it has elapsed. The worker's own wall
    // clock ticks the prune, per ADR-0029.
    later(() => {
      try {
        state.ring.pruneExpired(BigInt(now()));
      } catch {
        /* transform torn down before the window elapsed - nothing to prune */
      }
    }, msg.overlapMs + 200);
  }

  /** Move an encrypting transform to another `(leaf, stream)`. A key that
   *  does not derive leaves the transform with none: its frames are dropped. */
  function applyRetarget(state: TransformState, msg: RetargetMessage): void {
    if (state.operation !== "encrypt") return;
    state.localLeaf = msg.localLeaf;
    state.streamIndex = msg.streamIndex;
    try {
      state.sealKey = state.ring.participantKey(msg.localLeaf, msg.streamIndex);
    } catch {
      state.sealKey = null;
    }
  }

  function forget(id: string): void {
    starting.delete(id);
    pendingRekeys.delete(id);
    pendingCodecs.delete(id);
    pendingRetargets.delete(id);
  }

  async function start(
    streams: EncodedStreams,
    opts: SFrameTransformOptions,
    source: SFrameStreamSource,
  ): Promise<void> {
    if (transforms.has(opts.id) || starting.has(opts.id)) {
      // One id, one transform. A second set of streams under an id in use is
      // left unconnected (nothing flows through it) and said.
      diag(`sframe transform refused: id=${opts.id} is already in use (via=${source})`);
      return;
    }
    starting.add(opts.id);
    let wasm: SFrameWorkerWasm;
    try {
      wasm = await deps.loadWasm();
    } catch (err) {
      forget(opts.id);
      throw err;
    }
    // Said once per transform, so the absence of the per-frame lines can be
    // read: with this line present and no `sframe receive` lines after it,
    // no frame ever reached the transform, which is a different defect from
    // frames arriving and failing to open.
    diag(
      `sframe transform armed: op=${opts.operation} id=${opts.id} ` +
        `leaf=${opts.localLeaf ?? "-"} stream=${opts.streamIndex ?? "-"} via=${source}`,
    );
    let ring: CallKeyRing;
    try {
      ring = new wasm.CallKeyRing(opts.seed);
    } catch {
      // A seed the core refuses: no key, so nothing is connected.
      forget(opts.id);
      diag(`sframe transform refused: id=${opts.id} has a seed the core does not accept`);
      return;
    }
    const state: TransformState = {
      operation: opts.operation,
      localLeaf: opts.localLeaf,
      streamIndex: opts.streamIndex,
      ring,
      sealKey: null,
      codecs: null,
      received: new ReceiveLedger(),
    };

    let stream: TransformStream<EncodedFrame, EncodedFrame>;
    if (opts.operation === "encrypt") {
      if (opts.localLeaf === undefined) {
        // No leaf -> cannot stamp a KID -> cannot encrypt. Refuse by leaving
        // the pipe unconnected (no plaintext escapes).
        forget(opts.id);
        return;
      }
      try {
        // Compose the seal-side KID `(localLeaf << 8) | streamIndex` (audio
        // -> 0, camera -> 1, the camera's light sender -> 3, its middle
        // sender -> 4). The ring's `participantKey` does the composition; a
        // stream index > 255 fails closed FFI-side (ADR-0035).
        state.sealKey = ring.participantKey(opts.localLeaf, opts.streamIndex ?? 0);
      } catch {
        forget(opts.id);
        return; // derivation failed - refuse rather than emit plaintext
      }
      const codecs = createCodecResolver({
        mediaKind: opts.mediaKind ?? (opts.streamIndex === 0 ? "audio" : "video"),
        codecFromMime: (mime) => wasm.mediaCodecFromMime(mime),
        vp8: wasm.MediaCodec.Vp8,
        vp8IsKeyFrame: (first) => wasm.vp8IsKeyFrame(first),
        diag,
        askForCodecs: () => wantCodecs(opts.id),
        setTimeout: later,
      });
      state.codecs = codecs;
      state.codecOf = (frame, bytes) => codecs.codecOf(frame, bytes);
      state.ready = (frame) => codecs.ready(frame);
      // One sender, one stream, one key and one counter; the camera's light
      // and middle senders are senders of their own and so have their own of
      // each. See `./sframe-seal-stream.ts`.
      stream = sealingStream<EncodedFrame>(state);
    } else {
      // The KID is the whole composite `(sender_leaf << 8) | stream_index`
      // (ADR-0035), a bigint; it sits in the SFrame header at the end of the
      // frame. `parseMediaFrame` throws on anything that is not a well-formed
      // sealed media frame.
      stream = decryptStream(
        state,
        (wire) => {
          const info = wasm.parseMediaFrame(wire);
          return { kid: info.kid, isRedOpus: info.isRedOpus };
        },
        redReceive(wasm, opts.redDecodes === true),
      );
    }

    transforms.set(opts.id, state);
    starting.delete(opts.id);
    // Before a pending rekey: the rekey then derives the key of the new role.
    const retarget = pendingRetargets.get(opts.id);
    pendingRetargets.delete(opts.id);
    if (retarget !== undefined) applyRetarget(state, retarget);
    const pending = pendingRekeys.get(opts.id);
    pendingRekeys.delete(opts.id);
    if (pending !== undefined) applyRekey(state, pending);
    const codecTable = pendingCodecs.get(opts.id);
    pendingCodecs.delete(opts.id);
    if (codecTable !== undefined) state.codecs?.setCodecs(codecTable);
    try {
      await streams.readable.pipeThrough(stream).pipeTo(streams.writable);
    } finally {
      // Stream ended (track removed / call over) - drop the state so the
      // ring (and its keys) can be reclaimed and a stale id can't be rekeyed.
      transforms.delete(opts.id);
    }
  }

  /** Start a transform and say so if it could not run. */
  function launch(
    streams: EncodedStreams,
    opts: SFrameTransformOptions,
    source: SFrameStreamSource,
  ): void {
    start(streams, opts, source).catch((err: unknown) => {
      const reason = err instanceof Error ? err.message : "unknown";
      diag(`sframe transform stopped: id=${opts.id} via=${source}: ${reason}`);
    });
  }

  return {
    start,

    liveCount: (): number => transforms.size,

    onTransformer(transformer): void {
      if (!isSFrameTransformOptions(transformer.options)) {
        diag("sframe transform refused: an rtctransform event carried no usable options");
        return;
      }
      launch(transformer, transformer.options, "script-transform");
    },

    onMessage(data): void {
      if (isEncodedStreamsMessage(data)) {
        launch(
          { readable: data.readable, writable: data.writable },
          data.options,
          "encoded-streams",
        );
        return;
      }
      if (isCodecsMessage(data)) {
        const state = transforms.get(data.id);
        if (state !== undefined) state.codecs?.setCodecs(data.codecs);
        else if (starting.has(data.id)) pendingCodecs.set(data.id, data.codecs);
        return;
      }
      if (isRetargetMessage(data)) {
        const state = transforms.get(data.id);
        if (state !== undefined) applyRetarget(state, data);
        else if (starting.has(data.id)) pendingRetargets.set(data.id, data);
        return;
      }
      if (!isRekeyMessage(data)) return;
      const state = transforms.get(data.id);
      if (state === undefined) {
        // A transform that is still loading the core keeps the newest rekey
        // and applies it once its state exists; without this the rekey is
        // lost and the transform seals or opens under the seed it was
        // created with.
        if (starting.has(data.id)) pendingRekeys.set(data.id, data);
        return; // unknown / already-torn-down transform
      }
      applyRekey(state, data);
    },
  };
}
