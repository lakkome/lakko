// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a call's media is doing, read from the browser's own counters
 * (ADR-0067 step 1).
 *
 * A burst of packet loss shows up on the receiving side as freezes, dropped
 * frames, a growing jitter buffer and concealed audio. Those are the numbers
 * any change to how calls survive loss has to move, so the group call logs
 * them per interval and once more as a total when it ends. Diagnostics only:
 * counts and durations, nothing about who is in the call or what they say.
 *
 * Pure: takes plain stats records, returns plain values and lines. The caller
 * owns `getStats()` and the timer.
 */

/** One remote stream's cumulative receiver counters. */
export interface InboundCounters {
  readonly kind: "audio" | "video";
  readonly packetsReceived: number;
  readonly packetsLost: number;
  readonly nackCount: number;
  readonly pliCount: number;
  readonly freezeCount: number;
  readonly totalFreezesDuration: number;
  readonly framesDecoded: number;
  readonly framesDropped: number;
  readonly jitterBufferDelay: number;
  readonly jitterBufferEmittedCount: number;
  readonly concealedSamples: number;
  readonly totalSamplesReceived: number;
}

/** What this device's own camera sender reports. */
export interface OutboundVideo {
  readonly scalabilityMode: string | null;
  readonly qualityLimitationReason: string | null;
  readonly frameWidth: number | null;
  readonly frameHeight: number | null;
  readonly framesPerSecond: number | null;
  readonly bytesSent: number;
}

/** One `getStats()` reading, reduced to what ADR-0067 measures. */
export interface MediaReading {
  /** Keyed by the stats id of the `inbound-rtp` entry, which stays the same
   *  for a stream across readings. */
  readonly inbound: ReadonlyMap<string, InboundCounters>;
  readonly outboundVideo: OutboundVideo | null;
}

/** The parts of an `RTCStatsReport` entry this module reads. */
type StatsEntry = Record<string, unknown>;

function num(entry: StatsEntry, key: string): number {
  const v = entry[key];
  return typeof v === "number" && Number.isFinite(v) ? v : 0;
}

function numOrNull(entry: StatsEntry, key: string): number | null {
  const v = entry[key];
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

function strOrNull(entry: StatsEntry, key: string): string | null {
  const v = entry[key];
  return typeof v === "string" && v.length > 0 ? v : null;
}

/** Reduce a stats report (as its entries) to a reading. */
export function readMedia(entries: Iterable<StatsEntry>): MediaReading {
  const inbound = new Map<string, InboundCounters>();
  let outboundVideo: OutboundVideo | null = null;
  for (const e of entries) {
    const type = e["type"];
    const kind = e["kind"];
    if (kind !== "audio" && kind !== "video") continue;
    if (type === "inbound-rtp") {
      const id = e["id"];
      if (typeof id !== "string") continue;
      inbound.set(id, {
        kind,
        packetsReceived: num(e, "packetsReceived"),
        packetsLost: num(e, "packetsLost"),
        nackCount: num(e, "nackCount"),
        pliCount: num(e, "pliCount"),
        freezeCount: num(e, "freezeCount"),
        totalFreezesDuration: num(e, "totalFreezesDuration"),
        framesDecoded: num(e, "framesDecoded"),
        framesDropped: num(e, "framesDropped"),
        jitterBufferDelay: num(e, "jitterBufferDelay"),
        jitterBufferEmittedCount: num(e, "jitterBufferEmittedCount"),
        concealedSamples: num(e, "concealedSamples"),
        totalSamplesReceived: num(e, "totalSamplesReceived"),
      });
    } else if (type === "outbound-rtp" && kind === "video") {
      // One camera sender; with several layers the first one stands for it.
      if (outboundVideo !== null) continue;
      outboundVideo = {
        scalabilityMode: strOrNull(e, "scalabilityMode"),
        qualityLimitationReason: strOrNull(e, "qualityLimitationReason"),
        frameWidth: numOrNull(e, "frameWidth"),
        frameHeight: numOrNull(e, "frameHeight"),
        framesPerSecond: numOrNull(e, "framesPerSecond"),
        bytesSent: num(e, "bytesSent"),
      };
    }
  }
  return { inbound, outboundVideo };
}

/** How one stream changed between two readings. */
export interface StreamDelta {
  readonly kind: "audio" | "video";
  readonly packetsReceived: number;
  readonly packetsLost: number;
  readonly nacks: number;
  readonly keyframeRequests: number;
  readonly freezes: number;
  readonly freezeSeconds: number;
  readonly framesDecoded: number;
  readonly framesDropped: number;
  /** Average time a frame or sample spent in the jitter buffer over the
   *  interval, in milliseconds; `null` when nothing left it. */
  readonly jitterBufferMs: number | null;
  /** Share of received audio samples that were concealed, 0..1; `null` for
   *  video or when no samples arrived. */
  readonly concealedShare: number | null;
}

/**
 * The change of every stream present in `now` since `before`. A stream that
 * is new in `now` is measured from zero; counters that went backwards (a
 * stream replaced under the same id) are measured from zero as well.
 */
export function deltaOf(before: MediaReading | null, now: MediaReading): Map<string, StreamDelta> {
  const out = new Map<string, StreamDelta>();
  for (const [id, cur] of now.inbound) {
    const prev = before?.inbound.get(id);
    const base = prev !== undefined && prev.packetsReceived <= cur.packetsReceived ? prev : null;
    const d = (key: keyof Omit<InboundCounters, "kind">): number =>
      Math.max(0, cur[key] - (base === null ? 0 : base[key]));
    const emitted = d("jitterBufferEmittedCount");
    const samples = d("totalSamplesReceived");
    out.set(id, {
      kind: cur.kind,
      packetsReceived: d("packetsReceived"),
      packetsLost: d("packetsLost"),
      nacks: d("nackCount"),
      keyframeRequests: d("pliCount"),
      freezes: d("freezeCount"),
      freezeSeconds: d("totalFreezesDuration"),
      framesDecoded: d("framesDecoded"),
      framesDropped: d("framesDropped"),
      jitterBufferMs: emitted > 0 ? (d("jitterBufferDelay") / emitted) * 1000 : null,
      concealedShare: cur.kind === "audio" && samples > 0 ? d("concealedSamples") / samples : null,
    });
  }
  return out;
}

function fixed(n: number | null, digits: number): string {
  return n === null ? "-" : n.toFixed(digits);
}

/** One plain-text line per stream (log format: ASCII, no names). */
export function describeDeltas(label: string, deltas: ReadonlyMap<string, StreamDelta>): string[] {
  const lines: string[] = [];
  let n = 0;
  for (const d of deltas.values()) {
    n += 1;
    const common =
      `${label} ${d.kind}#${n} pkts=${d.packetsReceived} lost=${d.packetsLost} nack=${d.nacks}` +
      ` jb_ms=${fixed(d.jitterBufferMs, 0)}`;
    lines.push(
      d.kind === "video"
        ? `${common} pli=${d.keyframeRequests} freezes=${d.freezes} freeze_s=${fixed(d.freezeSeconds, 2)}` +
            ` decoded=${d.framesDecoded} dropped=${d.framesDropped}`
        : `${common} concealed=${fixed(d.concealedShare === null ? null : d.concealedShare * 100, 1)}%`,
    );
  }
  return lines;
}

/** The camera sender in one line, or `null` when there is none. */
export function describeOutbound(label: string, out: OutboundVideo | null): string | null {
  if (out === null) return null;
  const size = out.frameWidth !== null && out.frameHeight !== null ? `${out.frameWidth}x${out.frameHeight}` : "-";
  return (
    `${label} send video mode=${out.scalabilityMode ?? "-"} size=${size}` +
    ` fps=${fixed(out.framesPerSecond, 0)} limited_by=${out.qualityLimitationReason ?? "-"}`
  );
}
