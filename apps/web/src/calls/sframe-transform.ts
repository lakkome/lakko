// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Web SFrame attach machinery — the browser mirror of the native
 * `apps/shared/sframe-bridge` C++ shim.
 *
 * On native (iOS / Android) every outgoing media frame is sealed and
 * every incoming frame opened inside libwebrtc's
 * `FrameEncryptorInterface` / `FrameDecryptorInterface`. On the web
 * the equivalent hook is a `Worker` that sees a stream of
 * `RTCEncodedAudioFrame` / `RTCEncodedVideoFrame` and rewrites each
 * frame's `.data` in place. A browser hands the worker that stream in one
 * of two ways (`./sframe-transport.ts` decides which, from what the page
 * exposes and never from a browser's name):
 *
 *   * the W3C **Encoded Transform** API: `sender.transform = new
 *     RTCRtpScriptTransform(worker, options)`; preferred where it exists;
 *   * the earlier **insertable streams** interface of Chromium:
 *     `sender.createEncodedStreams()` on a peer connection constructed with
 *     `encodedInsertableStreams: true`, the readable / writable pair
 *     transferred to the worker in a message carrying the same options.
 *
 * The worker runs the identical seal / open for both
 * (`./sframe-worker-core.ts`); only the delivery of the streams differs.
 *
 * This module runs on the MAIN thread. It owns the single shared
 * transform `Worker` and exposes two attach helpers that the SFU
 * call path calls right after it adds a track to (or receives a
 * track from) the `RTCPeerConnection` — exactly the
 * "attach-between-addTrack-and-SDP-build" sequencing the native
 * shim follows.
 *
 * The actual seal / open is `sealMediaFrame` /
 * `openMediaFrameWithOverlap` from `lakko-ffi-wasm`, called inside the
 * worker. The frame layout is ADR-0071's: the codec's own frame header
 * stays in the clear at the start of the frame, the SFrame header (KID +
 * CTR) sits at the end before a 3-byte trailer, and the AEAD additional
 * data is the SFrame header, the clear bytes and the trailer — never any
 * host RTP metadata (ADR-0013). The native clients still seal the
 * header-first layout; until they move (ADR-0071 phase 2) a web client and
 * a native client do not open each other's media.
 *
 * Security invariant #4 (no fallback to non-E2EE): if the browser
 * exposes neither interface, the attach helpers THROW. A caller that
 * cannot attach the transform MUST refuse to open the call rather than
 * send plaintext media. There is no third path and no pass-through.
 *
 * On the insertable-streams path three things hold that do not on the
 * other, and this module carries them:
 *
 *   * the peer connection must be built with
 *     `sframePeerConnectionConfig` (`./sframe-transport.ts`), or
 *     `createEncodedStreams()` throws;
 *   * a sender's or receiver's streams can be taken ONCE, so a second
 *     attach of the same sender or receiver returns the first transform's
 *     id (re-keyed if the seed moved) instead of building another;
 *   * a sender or receiver whose streams were never taken passes no media
 *     at all (a stall, not plaintext). `coverEncodedStreams` is the check
 *     that none was missed.
 *
 * Used by the SFU group / link call (`sfu-call-session.ts`) and by the
 * one-to-one call (`call-session.ts`).
 */

import { senderCodecEntries } from "./sframe-codec";
import { sframeTransport, type SFrameTransport } from "./sframe-transport";

/** Operation the worker performs on the attached RTP stream. */
export type SFrameOperation = "encrypt" | "decrypt";

/**
 * ADR-0035 per-stream index — the low 8 bits of the SFrame KID
 * `(sender_leaf << 8) | stream_index`. Unlike ADR-0034's local media-type
 * byte, this rides ON the wire (it is part of the KID) so the receiver reads
 * the whole KID off each frame's header and never needs a separate
 * discriminator. The indices in use ({@link SFRAME_STREAM_AUDIO} and its
 * neighbours):
 *   * `0` = the participant's audio stream,
 *   * `1` = the participant's camera (the main sender),
 *   * `2` = reserved for a screen share, which is not published yet,
 *   * `3` = the camera's light sender (ADR-0070 step 1),
 *   * `4` = the camera's middle sender (ADR-0070 step 5).
 * Each `(sender_leaf, stream_index)` pair derives a distinct per-participant
 * key + nonce-salt space, so two concurrent streams from one leaf can never
 * collide on a `(key, nonce)` pair — the gap ADR-0034's per-media-TYPE tag left
 * open for two same-kind streams. The camera, its light sender and its middle
 * sender are video streams of one leaf, which is exactly that case: no two of
 * them may ever share an index.
 */
export type SFrameStreamIndex = number;

/** The stream index of a participant's microphone. */
export const SFRAME_STREAM_AUDIO: SFrameStreamIndex = 0;
/** The stream index of a participant's camera (the main sender). */
export const SFRAME_STREAM_CAMERA: SFrameStreamIndex = 1;
/** The stream index of a screen share's picture (ADR-0083,
 *  `sframeStreamScreen()` in the core). */
export const SFRAME_STREAM_SCREEN_SHARE: SFrameStreamIndex = 2;
/** The stream index of the camera's light sender (ADR-0070 step 1). */
export const SFRAME_STREAM_CAMERA_LIGHT: SFrameStreamIndex = 3;
/** The stream index of the camera's middle sender (ADR-0070 step 5). */
export const SFRAME_STREAM_CAMERA_MIDDLE: SFrameStreamIndex = 4;
/** The stream index of a screen share's sound (ADR-0083,
 *  `sframeStreamScreenAudio()` in the core). */
export const SFRAME_STREAM_SCREEN_AUDIO: SFrameStreamIndex = 5;

/** Map an `RTCRtpSender` track `kind` to the ADR-0035 stream index of a
 *  participant's microphone or main camera. Anything that is not `"video"`
 *  seals as the audio stream; a WebRTC track kind is only ever `"audio"` or
 *  `"video"`. A further video sender of the same participant (the camera's
 *  light or middle sender) is NOT covered by this: its caller states its
 *  index. */
export function streamIndexOfKind(kind: string): SFrameStreamIndex {
  return kind === "video" ? SFRAME_STREAM_CAMERA : SFRAME_STREAM_AUDIO;
}

/**
 * Options handed to the worker: via the `RTCRtpScriptTransform`
 * constructor, or in the message that carries a `createEncodedStreams()`
 * pair. The same object either way. Structured-cloned across the thread
 * boundary, so every field must be cloneable (a `Uint8Array` is).
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
   * `1 = camera`, `3 = the camera's light sender`, `4 = its middle sender`. Fixed for the transform's
   * whole lifetime (each
   * `RTCRtpScriptTransform` carries exactly one track, hence one kind), so the
   * worker reads it once and composes the seal-side KID
   * `(localLeaf << 8) | streamIndex` from it. Required for `"encrypt"`; unset
   * for `"decrypt"` (the receiver takes the whole KID straight off the wire).
   */
  readonly streamIndex?: SFrameStreamIndex;
  /**
   * The kind of the sender's track. `"encrypt"` only. A video sender's
   * frames keep their codec header in the clear (ADR-0071), which needs the
   * codec; an audio sender's frames keep nothing.
   */
  readonly mediaKind?: "audio" | "video";
}

/**
 * `true` when the browser can hand encoded frames to the SFrame worker by
 * either interface (`./sframe-transport.ts`). `false` means no call that
 * needs frame encryption is opened, and the attach helpers below refuse.
 */
export function isSFrameTransformSupported(): boolean {
  return sframeTransport() !== null;
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
      // The worker met a video frame whose codec it could not tell and asks
      // for the sender's negotiated codecs (ADR-0071).
      if (
        typeof data === "object" &&
        data !== null &&
        (data as { kind?: unknown }).kind === "sframe-codecs-wanted"
      ) {
        const id = (data as { id?: unknown }).id;
        if (typeof id === "string") postSenderCodecs(id);
      }
    });
  }
  return sharedWorker;
}

/** Senders with an encrypting transform attached, by transform id. */
const attachedSenders = new Map<string, RTCRtpSender>();

/** Post one sender's negotiated codecs (payload type -> MIME type) to the
 *  worker. Nothing is posted while the browser reports none, which is the
 *  case before the first negotiation completes. */
function postSenderCodecs(id: string): void {
  const sender = attachedSenders.get(id);
  if (sender === undefined || sharedWorker === null) return;
  const codecs = senderCodecEntries(sender);
  if (codecs.length === 0) return;
  sharedWorker.postMessage({ type: "codecs", id, codecs });
}

/**
 * Tell the worker which codecs every attached sender now sends. Call after a
 * negotiation completes (`setRemoteDescription`): a sealed video frame keeps
 * its codec header in the clear (ADR-0071), and where the browser's frame
 * metadata names no codec the worker finds it by payload type in this table.
 * Safe to call at any time; the worker also asks by itself when it meets a
 * frame it cannot place.
 */
export function syncSenderCodecs(): void {
  for (const id of attachedSenders.keys()) postSenderCodecs(id);
}

/** The transport to attach with, or a refusal (invariant #4: never silently
 *  fall back to plaintext media). */
function transportOrRefuse(): SFrameTransport {
  const transport = sframeTransport();
  if (transport === null) {
    throw new Error(
      "This browser has neither RTCRtpScriptTransform nor createEncodedStreams: " +
        "refusing to open the call without SFrame end-to-end media encryption " +
        "(security invariant #4: no fallback to non-E2EE)",
    );
  }
  return transport;
}

/** A sender or receiver as the insertable-streams interface extends it.
 *  Not in TypeScript's bundled lib definitions. */
interface EncodedStreamsEndpoint {
  createEncodedStreams(): { readable: ReadableStream; writable: WritableStream };
}

/** What is known on this thread about a transform built over transferred
 *  streams. */
interface EncodedStreamsAttachment {
  readonly id: string;
  readonly operation: SFrameOperation;
  streamIndex: SFrameStreamIndex | undefined;
  localLeaf: number | undefined;
}

/**
 * Senders and receivers whose encoded streams were taken, with the
 * transform built over them. Insertable-streams path only: the streams of a
 * sender or receiver can be taken once in its lifetime, so this is what
 * makes an attach repeatable (a second `track` event for one receiver, a
 * sender that `addTrack` handed out again). Weak, so a closed connection's
 * senders and receivers are not kept alive.
 */
let encodedStreamsAttached = new WeakMap<object, EncodedStreamsAttachment>();

/** The newest seed posted to each transform, by id. Both paths write it
 *  (`rekeySFrame`); the insertable-streams path reads it to tell whether a
 *  repeated attach brings a new seed. */
const seedById = new Map<string, Uint8Array>();

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.byteLength !== b.byteLength) return false;
  for (let i = 0; i < a.byteLength; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

/**
 * Hand one sender's or receiver's encoded frames to the worker, by whichever
 * interface the browser has. Returns the transform's id: `options.id`, or on
 * the insertable-streams path the id of the transform that already holds
 * this endpoint's streams.
 */
function attachTransform(
  endpoint: RTCRtpSender | RTCRtpReceiver,
  options: SFrameTransformOptions,
): string {
  const transport = transportOrRefuse();
  if (transport === "script-transform") {
    endpoint.transform = new RTCRtpScriptTransform(transformWorker(), options);
    seedById.set(options.id, options.seed);
    return options.id;
  }
  const existing = encodedStreamsAttached.get(endpoint);
  if (existing !== undefined) {
    // The streams are with the worker already and cannot be taken again, so
    // the transform that holds them is brought to what this attach asks for.
    // On the script-transform path the same call builds a new transform with
    // these options; the result is the same: frames sealed under this
    // `(leaf, stream)` and this seed from here on.
    if (
      options.operation === "encrypt" &&
      options.localLeaf !== undefined &&
      options.streamIndex !== undefined &&
      (existing.streamIndex !== options.streamIndex || existing.localLeaf !== options.localLeaf)
    ) {
      // A sender the browser handed out again for another role (`addTrack`
      // may reuse a sender that never sent).
      transformWorker().postMessage({
        type: "retarget",
        id: existing.id,
        localLeaf: options.localLeaf,
        streamIndex: options.streamIndex,
      });
      existing.streamIndex = options.streamIndex;
      existing.localLeaf = options.localLeaf;
    }
    const known = seedById.get(existing.id);
    if (known === undefined || !sameBytes(known, options.seed)) {
      rekeySFrame(existing.id, options.seed);
    }
    return existing.id;
  }
  // Throws when the peer connection was not built with
  // `sframePeerConnectionConfig`; the caller refuses the call.
  const streams = (endpoint as unknown as EncodedStreamsEndpoint).createEncodedStreams();
  transformWorker().postMessage(
    { type: "encoded-streams", options, readable: streams.readable, writable: streams.writable },
    [streams.readable, streams.writable] as unknown as Transferable[],
  );
  encodedStreamsAttached.set(endpoint, {
    id: options.id,
    operation: options.operation,
    streamIndex: options.streamIndex,
    localLeaf: options.localLeaf,
  });
  seedById.set(options.id, options.seed);
  return options.id;
}

/**
 * Attach the encrypting SFrame transform to an outgoing RTP sender.
 * Every frame the sender emits is sealed before it reaches the SFU:
 * `clear_prefix || ciphertext || SframeHeader(kid=(localLeaf << 8) |
 * streamIndex, ctr) || trailer` (ADR-0035, ADR-0071).
 *
 * `streamIndex` is stated by the caller for any sender that is not the
 * participant's one microphone or one main camera: the camera's light sender
 * passes {@link SFRAME_STREAM_CAMERA_LIGHT}, its middle sender
 * {@link SFRAME_STREAM_CAMERA_MIDDLE}. Left out, the index follows the
 * track kind (audio 0, camera 1). Two senders of one participant must never
 * be attached under the same index: they would share a key.
 *
 * Throws if the browser has no interface to the worker (invariant #4), if
 * `streamIndex` is not a whole number in 0..=255, or, on the
 * insertable-streams path, if the sender's peer connection was not built
 * with `sframePeerConnectionConfig`. On that path a second attach of one
 * sender returns the first transform's id, moved to the index and seed
 * asked for.
 */
export function attachSenderSFrame(
  sender: RTCRtpSender,
  opts: {
    readonly seed: Uint8Array;
    readonly localLeaf: number;
    readonly streamIndex?: SFrameStreamIndex;
    /** The media kind, when the sender has no track yet (a sender sealed as
     *  soon as its transceiver appears, before the camera is put on it). */
    readonly kind?: "audio" | "video";
  },
): string {
  transportOrRefuse();
  const kind = opts.kind ?? (sender.track?.kind === "video" ? "video" : "audio");
  // The sender's track is set by the caller's `addTrack` before this attach
  // (the native-shim sequencing), so without a stated index its kind is the
  // authoritative stream for every frame this transform seals: audio -> 0,
  // camera -> 1 (ADR-0035).
  const streamIndex = opts.streamIndex ?? streamIndexOfKind(kind);
  if (!Number.isInteger(streamIndex) || streamIndex < 0 || streamIndex > 255) {
    throw new Error(`SFrame stream index ${String(streamIndex)} is outside 0..=255`);
  }
  const id = crypto.randomUUID();
  const options: SFrameTransformOptions = {
    operation: "encrypt",
    id,
    seed: opts.seed,
    localLeaf: opts.localLeaf,
    streamIndex,
    mediaKind: kind,
  };
  const attachedId = attachTransform(sender, options);
  attachedSenders.set(attachedId, sender);
  // Mid-call attaches already have negotiated codecs; the first attach of a
  // call has none yet and is covered by `syncSenderCodecs`.
  postSenderCodecs(attachedId);
  return attachedId;
}

/**
 * Attach the decrypting SFrame transform to an incoming RTP receiver.
 * Each inbound frame's SFrame header (at the end of the frame) KID `(sender_leaf << 8) | stream_index`
 * selects (and lazily derives) the per-stream key; frames that fail the
 * AEAD open are dropped, never forwarded as plaintext.
 *
 * Throws if the browser has no interface to the worker (invariant #4). On
 * the insertable-streams path a second attach of one receiver returns the
 * first transform's id.
 */
export function attachReceiverSFrame(
  receiver: RTCRtpReceiver,
  opts: { readonly seed: Uint8Array },
): string {
  transportOrRefuse();
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
  return attachTransform(receiver, options);
}

/** What {@link coverEncodedStreams} found and did. */
export interface EncodedStreamsCoverage {
  /** Transform ids of receivers it attached because nothing had. */
  readonly attachedReceiverIds: readonly string[];
  /** Senders that carry a track and have no transform. Their media does not
   *  flow (their streams were never taken); nothing of theirs is sent
   *  unsealed. The caller says so and removes or re-attaches them. */
  readonly unsealedSenders: readonly RTCRtpSender[];
}

const NOTHING_TO_COVER: EncodedStreamsCoverage = { attachedReceiverIds: [], unsealedSenders: [] };

/**
 * Insertable-streams path only (a no-op returning nothing on the other):
 * make sure every sender and receiver of `pc` has had its encoded streams
 * taken. On that path one that has not passes no media, so a miss shows as a
 * person who is silent or frozen with no error anywhere.
 *
 *   * Every RECEIVER without a transform gets one, under `seed`. A receive
 *     transform needs nothing but the seed (each frame's own KID selects the
 *     key), so this is always possible; it covers a receiver the `track`
 *     event did not announce.
 *   * A SENDER with a track and no transform cannot be attached here: its
 *     stream index is its caller's to state. It is reported.
 *
 * Call after every negotiation step that can create a transceiver
 * (`setRemoteDescription`, `setLocalDescription`) and after adding senders.
 * Never throws.
 */
export function coverEncodedStreams(
  pc: Pick<RTCPeerConnection, "getSenders" | "getReceivers">,
  seed: Uint8Array,
): EncodedStreamsCoverage {
  if (sframeTransport() !== "encoded-streams") return NOTHING_TO_COVER;
  const attachedReceiverIds: string[] = [];
  const unsealedSenders: RTCRtpSender[] = [];
  let receivers: RTCRtpReceiver[] = [];
  let senders: RTCRtpSender[] = [];
  try {
    receivers = pc.getReceivers();
    senders = pc.getSenders();
  } catch {
    return NOTHING_TO_COVER; // a closed connection has nothing to cover
  }
  for (const receiver of receivers) {
    if (encodedStreamsAttached.has(receiver)) continue;
    try {
      attachedReceiverIds.push(attachReceiverSFrame(receiver, { seed }));
    } catch (err) {
      // Its streams stay untaken: nothing of it is played.
      // eslint-disable-next-line no-console
      console.warn("sframe: a receiver's encoded streams could not be taken", err);
    }
  }
  for (const sender of senders) {
    if (sender.track !== null && !encodedStreamsAttached.has(sender)) unsealedSenders.push(sender);
  }
  return { attachedReceiverIds, unsealedSenders };
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
  seedById.set(id, seed);
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
  attachedSenders.clear();
  seedById.clear();
  // The transforms died with the worker. A sender or receiver whose streams
  // went to it cannot be attached again (its streams can be taken once), so
  // forgetting it here makes a later attach fail loudly in
  // `createEncodedStreams` instead of returning the id of a dead transform.
  encodedStreamsAttached = new WeakMap();
}
