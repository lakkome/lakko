// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which codec an outgoing encoded frame is of (ADR-0071).
 *
 * A sealed media frame leaves the codec's own frame header in the clear at
 * the start of the frame, so that a receiver whose RTP stack has no generic
 * frame descriptor can still find the key frames. How many bytes that is
 * depends on the codec, and only the SENDER has to know it: 10 bytes of a VP8
 * key frame, 3 of any other VP8 frame, none for Opus or anything else. The
 * rule itself is the Rust core's (`lakko_calls::media_frame`); this module
 * only answers "which codec is this frame of", from what the browser says:
 *
 *   1. the frame's own metadata `mimeType`, where the browser gives one;
 *   2. else the frame's metadata `payloadType`, looked up in the payload
 *      type -> MIME type table of the sender's negotiated codecs
 *      (`RTCRtpSender.getParameters().codecs`), which the main thread posts
 *      to the worker;
 *   3. else unknown.
 *
 * An unknown codec is NOT guessed to be VP8. The frame is sealed as
 * `MediaCodec.Other`: nothing is left in the clear, so it is exactly as
 * secret as before ADR-0071, and a receiver without the frame descriptor
 * will not decode it. The resolver says so once and asks the main thread for
 * the sender's codec table, so the next frames resolve.
 *
 * That last case must not happen to a frame whose codec is about to be
 * known. The table reaches the worker from the main thread after the SFU's
 * answer is applied, and the encoder can hand the worker its first frame, a
 * key frame, before that message arrives. Sealed with nothing in the clear,
 * that key frame is useless to a receiver without the frame descriptor (and
 * to an SFU that finds key frames the same way): the receiver then waits for
 * the next key frame, which it has to ask for. So a video frame the resolver
 * cannot place is HELD ({@link CodecResolver.ready}): the main thread is
 * asked for the table, and the frame, with the frames behind it, waits until
 * a table arrives or {@link CODEC_WAIT_MS} has passed. The cost is that
 * delay, once, on the first frames of a sender, and only when the table was
 * late; a sender whose frames name their codec themselves (`mimeType`) never
 * waits. After a wait that did not help (no table came, or the table does
 * not have the frame's payload type) frames are no longer held, until a new
 * table arrives, so a browser that never names the codec costs one wait and
 * not one per frame.
 *
 * None of the three browsers was run for this. What each gives, as far as
 * the specifications and this code show: `payloadType` is in the W3C encoded
 * frame metadata dictionary for both audio and video; `mimeType` was added
 * to the dictionary later and a browser may not have it. Both are read
 * defensively and either may be absent.
 */

/** The frozen value of `lakko_calls::MediaCodec::Other` (and of the wasm
 *  `MediaCodec.Other`): no clear prefix. */
export const MEDIA_CODEC_OTHER = 0;

/** One negotiated codec of a sender: `RTCRtpCodecParameters` cut to what the
 *  resolver reads. Structured-cloneable. */
export interface SenderCodecEntry {
  readonly payloadType: number;
  readonly mimeType: string;
}

/** The part of an encoded frame the resolver reads. */
export interface CodecFrame {
  /** `"key" | "delta" | "empty"` on a video frame; absent on audio. */
  readonly type?: string;
  getMetadata?: () => unknown;
}

/** What the resolver needs from the core and from its host. */
export interface CodecResolverDeps {
  /** `"audio"` or `"video"`: the kind of the sender's track. */
  readonly mediaKind: "audio" | "video";
  /** MIME type -> the core's `MediaCodec` value (`mediaCodecFromMime`). */
  readonly codecFromMime: (mime: string) => number;
  /** The core's `MediaCodec.Vp8` value. */
  readonly vp8: number;
  /** The core's reading of a VP8 frame tag (`vp8IsKeyFrame`). */
  readonly vp8IsKeyFrame: (firstBytes: Uint8Array) => boolean | undefined;
  /** One diagnostic line. */
  readonly diag: (text: string) => void;
  /** Ask the main thread for this sender's codec table. */
  readonly askForCodecs: () => void;
  /** Run `fn` after `ms`. Left out, the worker's own `setTimeout`. */
  readonly setTimeout?: (fn: () => void, ms: number) => unknown;
}

/** Resolves the codec of each outgoing frame of one sender. */
export interface CodecResolver {
  /** Replace the payload type -> MIME type table. */
  setCodecs(entries: readonly SenderCodecEntry[]): void;
  /** The core `MediaCodec` value to seal `frame` under. */
  codecOf(frame: CodecFrame, bytes: Uint8Array): number;
  /**
   * Whether `frame` must wait before it is sealed. `null`: seal it now (an
   * audio frame, a frame whose codec is known, or waiting has already been
   * tried and did not help). Otherwise a promise that resolves when a codec
   * table arrives or {@link CODEC_WAIT_MS} has passed, whichever is first;
   * it never rejects. Every frame that asks while a wait is running gets the
   * same promise.
   */
  ready(frame: CodecFrame): Promise<void> | null;
}

/**
 * How long a video frame of unknown codec is held for the sender's codec
 * table, milliseconds. Chosen, not measured: the table is one `postMessage`
 * behind the applied answer, so it is normally a few milliseconds away; this
 * bounds the case where the main thread is busy, and is short enough that a
 * sender whose codec can never be told starts less than half a second late.
 */
export const CODEC_WAIT_MS = 400;

/** How often the resolver repeats a line or a request: the first time, then
 *  every this-many frames. */
const REPEAT_EVERY = 250;

function metadataOf(frame: CodecFrame): { mimeType?: unknown; payloadType?: unknown } {
  if (typeof frame.getMetadata !== "function") return {};
  try {
    const meta = frame.getMetadata();
    return typeof meta === "object" && meta !== null ? meta : {};
  } catch {
    return {};
  }
}

export function createCodecResolver(deps: CodecResolverDeps): CodecResolver {
  const mimeByPayloadType = new Map<number, string>();
  const codecByMime = new Map<string, number>();
  let unknownSeen = 0;
  let mismatchSeen = 0;
  /** The wait in progress, and how to end it. */
  let waiting: Promise<void> | null = null;
  let endWait: (() => void) | null = null;
  /** Waiting was tried and the codec is still unknown: no more holds until
   *  a new table arrives. */
  let waitingDoesNotHelp = false;
  let holdsSaid = 0;
  const later = deps.setTimeout ?? ((fn: () => void, ms: number): unknown => setTimeout(fn, ms));

  const codecOfMime = (mime: string): number => {
    const key = mime.toLowerCase();
    let codec = codecByMime.get(key);
    if (codec === undefined) {
      try {
        codec = deps.codecFromMime(key);
      } catch {
        codec = MEDIA_CODEC_OTHER;
      }
      codecByMime.set(key, codec);
    }
    return codec;
  };

  const mimeOf = (frame: CodecFrame): string | undefined => {
    const meta = metadataOf(frame);
    if (typeof meta.mimeType === "string" && meta.mimeType !== "") return meta.mimeType;
    if (typeof meta.payloadType === "number") return mimeByPayloadType.get(meta.payloadType);
    return undefined;
  };

  return {
    setCodecs(entries): void {
      mimeByPayloadType.clear();
      for (const entry of entries) {
        if (Number.isInteger(entry.payloadType) && typeof entry.mimeType === "string") {
          mimeByPayloadType.set(entry.payloadType, entry.mimeType);
        }
      }
      // A new table: a held frame can be looked up now, and a frame that
      // still cannot be placed may be held once more.
      waitingDoesNotHelp = false;
      endWait?.();
    },

    ready(frame): Promise<void> | null {
      if (deps.mediaKind !== "video" || waitingDoesNotHelp) return null;
      if (waiting !== null) return waiting;
      if (mimeOf(frame) !== undefined) return null;
      holdsSaid += 1;
      if (holdsSaid === 1 || holdsSaid % REPEAT_EVERY === 0) {
        deps.diag(
          "sframe seal: a video frame arrived before its codec was known; held for the " +
            `sender's codec table, at most ${CODEC_WAIT_MS} ms (${holdsSaid} such holds so far)`,
        );
      }
      deps.askForCodecs();
      waiting = new Promise<void>((resolve) => {
        let ended = false;
        const end = (timedOut: boolean): void => {
          if (ended) return;
          ended = true;
          waiting = null;
          endWait = null;
          if (timedOut) {
            waitingDoesNotHelp = true;
            deps.diag(
              `sframe seal: no codec table came within ${CODEC_WAIT_MS} ms; ` +
                "video frames are not held again until one does",
            );
          }
          resolve();
        };
        endWait = (): void => end(false);
        later(() => end(true), CODEC_WAIT_MS);
      });
      return waiting;
    },

    codecOf(frame, bytes): number {
      const mime = mimeOf(frame);
      if (mime === undefined) {
        // Holding a frame for a table that does not name its payload type
        // would hold every frame: stop until a new table arrives.
        if (deps.mediaKind === "video") waitingDoesNotHelp = true;
        // Audio is sealed with nothing in the clear whatever its codec, so an
        // unknown audio codec changes nothing and is not worth a line.
        if (deps.mediaKind === "video") {
          unknownSeen += 1;
          if (unknownSeen === 1 || unknownSeen % REPEAT_EVERY === 0) {
            deps.diag(
              `sframe seal: codec of a video frame unknown (${unknownSeen} so far); ` +
                "sealed with no clear codec header, so a receiver without the " +
                "frame descriptor cannot decode it",
            );
            deps.askForCodecs();
          }
        }
        return MEDIA_CODEC_OTHER;
      }
      const codec = codecOfMime(mime);
      if (codec === deps.vp8 && (frame.type === "key" || frame.type === "delta")) {
        // The frame's own tag decides how much stays clear (the core's rule:
        // it is what a receiver reads). The browser's word is only checked
        // against it, and a disagreement is said out loud.
        let tagSaysKey: boolean | undefined;
        try {
          tagSaysKey = deps.vp8IsKeyFrame(bytes.subarray(0, 1));
        } catch {
          tagSaysKey = undefined;
        }
        if (tagSaysKey !== undefined && tagSaysKey !== (frame.type === "key")) {
          mismatchSeen += 1;
          if (mismatchSeen === 1 || mismatchSeen % REPEAT_EVERY === 0) {
            deps.diag(
              `sframe seal: browser calls a VP8 frame "${frame.type}" but its frame tag says ` +
                `${tagSaysKey ? "key" : "delta"} (${mismatchSeen} so far); the tag decides`,
            );
          }
        }
      }
      return codec;
    },
  };
}

/** Read a sender's negotiated codecs as the table the worker wants. Empty
 *  before negotiation, or when the browser gives none. */
export function senderCodecEntries(sender: {
  getParameters?: () => { codecs?: readonly { payloadType?: unknown; mimeType?: unknown }[] };
}): SenderCodecEntry[] {
  if (typeof sender.getParameters !== "function") return [];
  let codecs: readonly { payloadType?: unknown; mimeType?: unknown }[] | undefined;
  try {
    codecs = sender.getParameters().codecs;
  } catch {
    return [];
  }
  const out: SenderCodecEntry[] = [];
  for (const codec of codecs ?? []) {
    if (typeof codec.payloadType === "number" && typeof codec.mimeType === "string") {
      out.push({ payloadType: codec.payloadType, mimeType: codec.mimeType });
    }
  }
  return out;
}
