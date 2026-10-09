// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the receive side of the SFrame transform saw, counted so that the
 * next call's log says WHAT the frames that did not open were.
 *
 * Why this exists. On live calls about half of the frames reaching the
 * receive transform of a person's LIGHT track did not open, on every
 * receiver, while their main and middle tracks opened cleanly. With the
 * header-first layout they were `openFailed`; with the ADR-0071 layout
 * (SFrame header and trailer at the END of the frame) the same share became
 * `badHeader`. Read together: those frames begin like a sealed frame and do
 * not end like one. The light track is the only one whose packets the SFU
 * sends twice (30 ms apart, as retransmissions).
 *
 * What the code establishes without a browser:
 *
 *   * Opening is stateless (`lakko_calls`: no replay window, pinned by the
 *     core's own test). A byte-identical second copy of a frame OPENS; it is
 *     counted here as `opened_again`, and it cannot be what fails.
 *   * So the frames that fail are not copies of good frames. They are other
 *     bytes that the browser's receiver assembled into a "frame" on that
 *     track.
 *
 * What only a call can say, and what these counters are for. For every frame
 * that is dropped, the ledger looks for a frame it OPENED with the same RTP
 * timestamp (the same picture) and records how the dropped bytes relate to
 * that frame's wire bytes:
 *
 *   * `cutN`: the dropped frame is the opened one with its last `N` bytes
 *     missing. A second copy that lost its tail: small `N` (1..3, the
 *     padding of a retransmission stripped twice) or a large one (one packet
 *     of a frame of several).
 *   * `extraN`: the opened one with `N` more bytes at its end.
 *   * `shift+N` / `shift-N`: the same bytes moved by `N` (a header byte of
 *     another packet format left in, or eaten).
 *   * `diff0`, `diff<16`, `diff>=16`: different from the first byte, within
 *     the first sixteen, or later.
 *
 * and, when no opened frame has that timestamp, counts it apart
 * (`dropped_new_ts`: something that is not a copy of a picture seen, such as
 * a forward-error-correction or padding packet delivered as media), with the
 * payload types and the reasons the parser gave.
 *
 * Nothing here decides anything. A frame that does not open is dropped by
 * the caller whatever this module counts (security invariant #4), and no
 * frame bytes leave it: the lines carry counts, lengths and payload types.
 *
 * Pure: no worker, no wasm, no timers.
 */

/** What became of one received frame. */
export type ReceiveOutcome = "opened" | "badHeader" | "openFailed";

/** What the browser says about one received frame, as far as it says it. */
export interface ReceivedFrameInfo {
  /** The frame's RTP timestamp; `undefined` when the browser gives none. */
  readonly rtpTimestamp: number | undefined;
  /** The RTP payload type; `undefined` when the browser gives none. */
  readonly payloadType: number | undefined;
}

/** The part of an encoded frame {@link receivedFrameInfo} reads. */
export interface InfoFrame {
  readonly timestamp?: unknown;
  getMetadata?: () => unknown;
}

/**
 * Read a received frame's RTP timestamp and payload type, defensively: the
 * metadata dictionary's `rtpTimestamp` where the browser has it, else the
 * frame's own `timestamp` (which older engines set to the RTP timestamp);
 * `payloadType` from the metadata. Either may be absent. Never throws.
 */
export function receivedFrameInfo(frame: InfoFrame): ReceivedFrameInfo {
  let meta: { rtpTimestamp?: unknown; payloadType?: unknown } = {};
  if (typeof frame.getMetadata === "function") {
    try {
      const m = frame.getMetadata();
      if (typeof m === "object" && m !== null) meta = m;
    } catch {
      meta = {};
    }
  }
  let own: unknown;
  try {
    own = frame.timestamp;
  } catch {
    own = undefined;
  }
  const ts = typeof meta.rtpTimestamp === "number" ? meta.rtpTimestamp : own;
  return {
    rtpTimestamp: typeof ts === "number" && Number.isFinite(ts) ? ts : undefined,
    payloadType: typeof meta.payloadType === "number" ? meta.payloadType : undefined,
  };
}

/** How many opened frames are remembered to compare dropped ones with. */
export const REMEMBERED_FRAMES = 32;

/** How many leading bytes of an opened frame's wire are kept. A light
 *  track's frame fits in it whole; of a larger frame only this much of the
 *  start is compared. */
export const REMEMBERED_BYTES = 1500;

/** The most distinct keys one of the small tallies keeps; further ones are
 *  counted together under `more`. */
export const MAX_TALLY_KEYS = 10;

/** The largest shift looked for, bytes. */
const MAX_SHIFT = 4;

/** Bytes compared to recognise a shift. */
const SHIFT_PROBE = 16;

/** A log line is produced for the first frame of each outcome and then for
 *  every this-many of that outcome. */
export const RECEIVE_LINE_EVERY = 250;

interface Remembered {
  readonly rtpTimestamp: number | undefined;
  readonly length: number;
  /** The first {@link REMEMBERED_BYTES} bytes of the wire, a copy. */
  readonly head: Uint8Array;
}

/** A small tally by key with a bound on the number of keys. */
class Tally {
  private readonly counts = new Map<string, number>();

  add(key: string): void {
    const k = this.counts.has(key) || this.counts.size < MAX_TALLY_KEYS ? key : "more";
    this.counts.set(k, (this.counts.get(k) ?? 0) + 1);
  }

  /** `[a:3 b:1]`, largest first; `[]` when empty. */
  text(): string {
    const entries = [...this.counts].sort((x, y) => y[1] - x[1]);
    return `[${entries.map(([k, n]) => `${k}:${n}`).join(" ")}]`;
  }
}

function startsWith(hay: Uint8Array, hayFrom: number, needle: Uint8Array, needleFrom: number, n: number): boolean {
  if (hayFrom + n > hay.length || needleFrom + n > needle.length) return false;
  for (let i = 0; i < n; i += 1) {
    if (hay[hayFrom + i] !== needle[needleFrom + i]) return false;
  }
  return true;
}

/**
 * How `dropped` relates to the wire of an opened frame of which `head` is the
 * first bytes and `openedLength` the whole length. See the module comment for
 * the names.
 */
export function relationToOpened(
  dropped: Uint8Array,
  head: Uint8Array,
  openedLength: number,
): string {
  const comparable = Math.min(dropped.length, head.length);
  let common = 0;
  while (common < comparable && dropped[common] === head[common]) common += 1;
  if (common === comparable && comparable > 0) {
    // The same start, as far as it can be compared.
    const delta = dropped.length - openedLength;
    if (delta < 0) return `cut${-delta}`;
    if (delta > 0) return `extra${delta}`;
    return head.length < openedLength ? "same-start" : "identical";
  }
  if (common < SHIFT_PROBE) {
    for (let k = 1; k <= MAX_SHIFT; k += 1) {
      if (startsWith(dropped, k, head, 0, SHIFT_PROBE)) return `shift+${k}`;
      if (startsWith(head, k, dropped, 0, SHIFT_PROBE)) return `shift-${k}`;
    }
  }
  if (common === 0) return "diff0";
  return common < 16 ? "diff<16" : "diff>=16";
}

/** A parser's or the AEAD's reason with its numbers removed, so that
 *  "needed 20, had 17" and "needed 20, had 9" are one key. */
export function reasonKey(reason: string | undefined): string {
  if (reason === undefined || reason === "") return "unknown";
  return reason
    .replace(/^call crypto error: /, "")
    .replace(/[0-9]+/g, "#")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 48);
}

/** One transform's receive counters. */
export class ReceiveLedger {
  private readonly outcomes: Record<ReceiveOutcome, number> = {
    opened: 0,
    badHeader: 0,
    openFailed: 0,
  };
  private openedAgain = 0;
  private droppedSameTs = 0;
  private droppedNewTs = 0;
  private droppedNoTs = 0;
  private droppedLenMin = Number.POSITIVE_INFINITY;
  private droppedLenMax = 0;
  private readonly relations = new Tally();
  private readonly reasons = new Tally();
  private readonly openedPayloadTypes = new Tally();
  private readonly droppedPayloadTypes = new Tally();
  private readonly remembered: Remembered[] = [];

  /**
   * Count one frame. `wire` is the frame's bytes as received, before any
   * open replaced them; it is read during the call and a bounded copy of an
   * opened frame's start is kept. Returns a log line when one is due (the
   * first frame of each outcome, then every {@link RECEIVE_LINE_EVERY}th),
   * else `null`.
   */
  note(
    outcome: ReceiveOutcome,
    wire: Uint8Array,
    info: ReceivedFrameInfo,
    kid?: bigint,
    reason?: string,
  ): string | null {
    const seen = (this.outcomes[outcome] += 1);
    const pt = info.payloadType === undefined ? "?" : String(info.payloadType);
    if (outcome === "opened") {
      this.openedPayloadTypes.add(pt);
      const twin = this.find(info.rtpTimestamp);
      if (
        twin !== undefined &&
        relationToOpened(wire, twin.head, twin.length) === (twin.head.length < twin.length ? "same-start" : "identical")
      ) {
        this.openedAgain += 1;
      } else {
        this.remember(wire, info.rtpTimestamp);
      }
    } else {
      this.droppedPayloadTypes.add(pt);
      this.reasons.add(reasonKey(reason));
      this.droppedLenMin = Math.min(this.droppedLenMin, wire.length);
      this.droppedLenMax = Math.max(this.droppedLenMax, wire.length);
      if (info.rtpTimestamp === undefined) {
        this.droppedNoTs += 1;
      } else {
        const twin = this.find(info.rtpTimestamp);
        if (twin === undefined) {
          this.droppedNewTs += 1;
        } else {
          this.droppedSameTs += 1;
          this.relations.add(relationToOpened(wire, twin.head, twin.length));
        }
      }
    }
    if (seen !== 1 && seen % RECEIVE_LINE_EVERY !== 0) return null;
    return this.line(outcome, kid);
  }

  /** The counters as one line. */
  line(what: ReceiveOutcome, kid?: bigint): string {
    const o = this.outcomes;
    const dropped = o.badHeader + o.openFailed;
    return (
      `sframe receive ${what}${kid === undefined ? "" : ` kid=${kid}`}: ` +
      `opened=${o.opened} badHeader=${o.badHeader} openFailed=${o.openFailed}` +
      ` frames=${o.opened + dropped} opened_again=${this.openedAgain}` +
      ` pt_opened=${this.openedPayloadTypes.text()}` +
      (dropped === 0
        ? ""
        : ` dropped_same_ts=${this.droppedSameTs} dropped_new_ts=${this.droppedNewTs}` +
          ` dropped_no_ts=${this.droppedNoTs} rel=${this.relations.text()}` +
          ` dropped_len=${this.droppedLenMin}..${this.droppedLenMax}` +
          ` pt_dropped=${this.droppedPayloadTypes.text()} reasons=${this.reasons.text()}`)
    );
  }

  /** The newest remembered opened frame with this RTP timestamp. */
  private find(rtpTimestamp: number | undefined): Remembered | undefined {
    if (rtpTimestamp === undefined) return undefined;
    for (let i = this.remembered.length - 1; i >= 0; i -= 1) {
      const r = this.remembered[i];
      if (r !== undefined && r.rtpTimestamp === rtpTimestamp) return r;
    }
    return undefined;
  }

  private remember(wire: Uint8Array, rtpTimestamp: number | undefined): void {
    if (rtpTimestamp === undefined) return;
    if (this.remembered.length >= REMEMBERED_FRAMES) this.remembered.shift();
    this.remembered.push({
      rtpTimestamp,
      length: wire.length,
      head: wire.slice(0, REMEMBERED_BYTES),
    });
  }
}
