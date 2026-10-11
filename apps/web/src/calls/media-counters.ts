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

import { describeSenderActivity, type SenderActivityNote } from "./sender-activity";
import type { LayerDisplayCounters, TileFrameCounts } from "./layer-display";

/**
 * Which of a remote camera's tracks a video receiver carries: the `main` one,
 * the `middle` one or the `light` companion (`light-stream.ts`). `-` when the
 * browser's report does not say which track the receiver belongs to.
 */
export type ReceiverLayer = "main" | "middle" | "light" | "-";

/** One remote stream's cumulative receiver counters. */
export interface InboundCounters {
  readonly kind: "audio" | "video";
  /** A video receiver's layer; `null` for audio. */
  readonly layer: ReceiverLayer | null;
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

/**
 * Which of this device's camera senders an outbound stream belongs to
 * (ADR-0070 steps 1 and 5): the `main` one, the `middle` one or the `light`
 * one.
 */
export type CameraSenderName = "main" | "middle" | "light";

/** What one of this device's own camera senders reports. */
export interface OutboundVideo {
  /** Which sender: the main camera, its middle copy or its light copy. */
  readonly sender: CameraSenderName;
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
  /** The main camera sender, or `null` with no camera. */
  readonly outboundVideo: OutboundVideo | null;
  /** Every camera sender that reports: the main one first, then the middle
   *  one, then the light one. */
  readonly outboundVideoSenders: readonly OutboundVideo[];
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

/**
 * Reduce a stats report (as its entries) to a reading. `lightTrackIds` holds
 * the ids of the remote tracks that are light companions; a video receiver
 * whose `trackIdentifier` is among them is `light`, any other is `main`.
 * `lightOutboundIds` holds the stats ids of the `outbound-rtp` entries of
 * this device's light camera sender (read from that sender's own
 * `getStats()`, which every engine scopes to the sender); a video
 * `outbound-rtp` entry whose id is among them is the `light` sender, any
 * other the `main` one. `middleTrackIds` and `middleOutboundIds` are the same
 * two sets for the middle track and the middle sender (ADR-0070 step 5); an
 * id in both a light and a middle set is read as light.
 */
export function readMedia(
  entries: Iterable<StatsEntry>,
  lightTrackIds: ReadonlySet<string> = new Set(),
  lightOutboundIds: ReadonlySet<string> = new Set(),
  middleTrackIds: ReadonlySet<string> = new Set(),
  middleOutboundIds: ReadonlySet<string> = new Set(),
): MediaReading {
  const inbound = new Map<string, InboundCounters>();
  const bySender: Record<CameraSenderName, OutboundVideo[]> = { main: [], middle: [], light: [] };
  for (const e of entries) {
    const type = e["type"];
    const kind = e["kind"];
    if (kind !== "audio" && kind !== "video") continue;
    if (type === "inbound-rtp") {
      const id = e["id"];
      if (typeof id !== "string") continue;
      const trackId = strOrNull(e, "trackIdentifier");
      inbound.set(id, {
        kind,
        layer:
          kind === "audio"
            ? null
            : trackId === null
              ? "-"
              : lightTrackIds.has(trackId)
                ? "light"
                : middleTrackIds.has(trackId)
                  ? "middle"
                  : "main",
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
      // Three camera senders at most, one entry each.
      const id = e["id"];
      const sender: CameraSenderName =
        typeof id !== "string"
          ? "main"
          : lightOutboundIds.has(id)
            ? "light"
            : middleOutboundIds.has(id)
              ? "middle"
              : "main";
      bySender[sender].push({
        sender,
        scalabilityMode: strOrNull(e, "scalabilityMode"),
        qualityLimitationReason: strOrNull(e, "qualityLimitationReason"),
        frameWidth: numOrNull(e, "frameWidth"),
        frameHeight: numOrNull(e, "frameHeight"),
        framesPerSecond: numOrNull(e, "framesPerSecond"),
        bytesSent: num(e, "bytesSent"),
      });
    }
  }
  return {
    inbound,
    outboundVideo: bySender.main[0] ?? null,
    outboundVideoSenders: [...bySender.main, ...bySender.middle, ...bySender.light],
  };
}

/** How one stream changed between two readings. */
export interface StreamDelta {
  readonly kind: "audio" | "video";
  /** A video receiver's layer; `null` for audio. */
  readonly layer: ReceiverLayer | null;
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
    const d = (key: keyof Omit<InboundCounters, "kind" | "layer">): number =>
      Math.max(0, cur[key] - (base === null ? 0 : base[key]));
    const emitted = d("jitterBufferEmittedCount");
    const samples = d("totalSamplesReceived");
    out.set(id, {
      kind: cur.kind,
      layer: cur.layer,
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

/** One plain-text line per stream (log format: ASCII, no names). A video
 *  line ends with `layer=`: `main`, `middle`, `light`, or `-` when unknown. */
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
            ` decoded=${d.framesDecoded} dropped=${d.framesDropped} layer=${d.layer ?? "-"}`
        : `${common} concealed=${fixed(d.concealedShare === null ? null : d.concealedShare * 100, 1)}%`,
    );
  }
  return lines;
}

/** The picture of a peer this device asks the SFU for: the name it has in
 *  the `video-wants` frame (`video-wants.ts`). */
export type ShownWant = "main" | "middle" | "light";

/**
 * What one remote peer's tile showed, in one line: the picture this device
 * asks for of them now (`want`), the one the SFU says it gives (`given`,
 * ADR-0070 step 6; `-` when it has not said, as with downlink control off),
 * how often the tile switched to the light
 * track, how long the light track was the shown one, and the longest and the
 * total displayed stall (`layer-display.ts`). Whole milliseconds. `peerId` is
 * the SFU's per-call peer id, the same one the stream ids carry.
 *
 * With `frames` (what the tile observed on its two elements), the line also
 * says what the rule's own counters cannot: `now` (what is shown), `heavy`
 * (`yes`: the heavy picture has shown a frame; `never`: it has shown none
 * since it was put in the tile; `none`: the tile has no heavy picture),
 * `heavy_frames` and `light_frames` (frames observed on each element in the
 * interval) and `heavy_gap_max_ms` (the longest time without a heavy frame).
 * Read with the receivers' `decoded=` of the same interval: a tile on the
 * light copy with `heavy=never` and nothing decoded on the heavy track is
 * waiting for a first key frame; `heavy=never` (or `heavy_frames=0`) while
 * the heavy track decodes means the element's frames do not reach the rule;
 * `heavy=yes` with `heavy_frames` near the decoded count means the rule
 * keeps the light copy, and `heavy_gap_max_ms` says against which threshold.
 */
export function describeShown(
  label: string,
  peerId: string,
  shown: LayerDisplayCounters,
  want: ShownWant,
  given: ShownWant | null = null,
  frames?: TileFrameCounts,
): string {
  return (
    `${label} shown peer=${peerId} want=${want} given=${given ?? "-"}` +
    ` light_switches=${shown.lightSwitches}` +
    ` light_ms=${Math.round(shown.lightMs)} stall_max_ms=${Math.round(shown.stallMaxMs)}` +
    ` stall_total_ms=${Math.round(shown.stallTotalMs)}` +
    (frames === undefined
      ? ""
      : ` now=${frames.shownNow} heavy=${frames.heavy} heavy_frames=${frames.heavyFrames}` +
        ` heavy_gap_max_ms=${Math.round(frames.heavyGapMaxMs)} light_frames=${frames.lightFrames}`)
  );
}

/** {@link describeShown} for every peer, in the map's order. `wantOf` says
 *  which picture is asked for of each, `givenOf` which one the SFU gives
 *  (`null` when it has not said). */
export function describeShownPeers(
  label: string,
  byPeer: ReadonlyMap<string, LayerDisplayCounters>,
  wantOf: (peerId: string) => ShownWant,
  givenOf: (peerId: string) => ShownWant | null = () => null,
  framesOf: (peerId: string) => TileFrameCounts | undefined = () => undefined,
): string[] {
  return [...byPeer].map(([peerId, shown]) =>
    describeShown(label, peerId, shown, wantOf(peerId), givenOf(peerId), framesOf(peerId)),
  );
}

/** One person whose shown picture a `video-given` frame changed
 *  (`video-given.ts`). */
export interface GivenChangeLine {
  readonly peer: string;
  readonly wanted: ShownWant;
  readonly given: ShownWant | null;
}

/**
 * One line for one `video-given` frame that changed the picture a tile shows:
 * every person it changed, each as `peer= want= given=` (`given=-` when the
 * frame no longer names them). `null` when it changed nobody's.
 */
export function describeGivenChanges(label: string, changes: readonly GivenChangeLine[]): string | null {
  if (changes.length === 0) return null;
  return (
    `${label} ` +
    changes.map((c) => `peer=${c.peer} want=${c.wanted} given=${c.given ?? "-"}`).join(" | ")
  );
}

/** What the diagnostics say of one sender's activity
 *  (`sender-activity.ts`): the SFU's last word for it (`null`: none) and
 *  whether its encoding is asked to be active. */
export type OutboundActivity = SenderActivityNote;

function activityTail(activity: OutboundActivity | undefined): string {
  return activity === undefined ? "" : describeSenderActivity(activity);
}

/** One camera sender in one line, or `null` when there is none. The line
 *  names which sender it is (`sender=main`, `sender=middle` or
 *  `sender=light`) and, when `activity` is given, ends with the SFU's last
 *  word for it and whether its encoding is active: `needed=yes|no|-
 *  active=yes|no`. */
export function describeOutbound(
  label: string,
  out: OutboundVideo | null,
  activity?: OutboundActivity,
): string | null {
  if (out === null) return null;
  const size = out.frameWidth !== null && out.frameHeight !== null ? `${out.frameWidth}x${out.frameHeight}` : "-";
  return (
    `${label} send video mode=${out.scalabilityMode ?? "-"} size=${size}` +
    ` fps=${fixed(out.framesPerSecond, 0)} limited_by=${out.qualityLimitationReason ?? "-"}` +
    ` sender=${out.sender}` +
    activityTail(activity)
  );
}

/** The camera, one line for each of its senders (main, middle, light). A sender
 *  the browser is sending nothing on reports no size and no frame rate
 *  (`-`). `activityOf` adds each line's `needed=` and `active=`. A sender
 *  named in `existing` that the browser reports no outbound stream for (some
 *  engines drop the entry of an inactive encoding) still has its line, all
 *  dashes, so a sender that was switched off does not vanish from the log. */
export function describeOutboundSenders(
  label: string,
  senders: readonly OutboundVideo[],
  activityOf?: (sender: CameraSenderName) => OutboundActivity | undefined,
  existing: readonly CameraSenderName[] = [],
): string[] {
  const lines: string[] = [];
  const reported = new Set<CameraSenderName>();
  for (const sender of senders) {
    reported.add(sender.sender);
    const line = describeOutbound(label, sender, activityOf?.(sender.sender));
    if (line !== null) lines.push(line);
  }
  for (const name of existing) {
    if (reported.has(name)) continue;
    const line = describeOutbound(
      label,
      {
        sender: name,
        scalabilityMode: null,
        qualityLimitationReason: null,
        frameWidth: null,
        frameHeight: null,
        framesPerSecond: null,
        bytesSent: 0,
      },
      activityOf?.(name),
    );
    if (line !== null) lines.push(line);
  }
  return lines;
}

/** What the sending side of the call reports, cumulative where it counts. */
export interface SenderReading {
  /** Camera `outbound-rtp`: frames and packets this device produced, summed
   *  over every camera sender. */
  readonly framesEncoded: number;
  readonly packetsSent: number;
  readonly retransmittedPacketsSent: number;
  readonly nackCount: number;
  readonly pliCount: number;
  readonly firCount: number;
  readonly hugeFramesSent: number;
  /** Bits a second the encoders are currently asked for, summed over the
   *  senders; `null` if unknown. */
  readonly targetBitrate: number | null;
  /** What the far end (the SFU) says it lost of the camera, 0..1; the worse
   *  of the senders' figures. */
  readonly remoteFractionLost: number | null;
  /** Round trip the far end's reports give, in milliseconds; the largest
   *  across the senders. */
  readonly remoteRttMs: number | null;
  /** The browser's own estimate of what it may send, bits a second. */
  readonly availableOutgoingBitrate: number | null;
  /** The formats on the wire, e.g. `audio/red` or `audio/opus`, `video/VP8`;
   *  `null` when the browser does not say. */
  readonly audioCodec: string | null;
  readonly videoCodec: string | null;
}

/**
 * Reduce a stats report to the sender reading, or `null` with no camera.
 * Every camera sender is summed. `lightOutboundIds` and `middleOutboundIds`
 * are as in {@link readMedia}; they only decide whose codec is named (the
 * main sender's, when it reports).
 */
export function readSender(
  entries: Iterable<StatsEntry>,
  lightOutboundIds: ReadonlySet<string> = new Set(),
  middleOutboundIds: ReadonlySet<string> = new Set(),
): SenderReading | null {
  const outs: StatsEntry[] = [];
  const remotes: StatsEntry[] = [];
  let pair: StatsEntry | null = null;
  let audioOut: StatsEntry | null = null;
  const codecs = new Map<string, string>();
  for (const e of entries) {
    if (e["type"] === "codec" && typeof e["id"] === "string" && typeof e["mimeType"] === "string") {
      codecs.set(e["id"], e["mimeType"]);
    } else if (e["type"] === "outbound-rtp" && e["kind"] === "audio" && audioOut === null) {
      audioOut = e;
    }
    if (e["type"] === "outbound-rtp" && e["kind"] === "video") outs.push(e);
    else if (e["type"] === "remote-inbound-rtp" && e["kind"] === "video") remotes.push(e);
    else if (e["type"] === "candidate-pair" && e["availableOutgoingBitrate"] !== undefined) {
      if (pair === null || e["nominated"] === true) pair = e;
    }
  }
  const isCopy = (e: StatsEntry): boolean =>
    typeof e["id"] === "string" && (lightOutboundIds.has(e["id"]) || middleOutboundIds.has(e["id"]));
  const first = outs.find((e) => !isCopy(e)) ?? outs[0];
  if (first === undefined) return null;
  const sum = (key: string): number => outs.reduce((acc, e) => acc + num(e, key), 0);
  /** The largest value any entry reports, or `null` when none reports one. */
  const largest = (from: readonly StatsEntry[], key: string): number | null =>
    from.reduce<number | null>((acc, e) => {
      const v = numOrNull(e, key);
      return v === null ? acc : acc === null ? v : Math.max(acc, v);
    }, null);
  const targets = outs.map((e) => numOrNull(e, "targetBitrate")).filter((v): v is number => v !== null);
  const rtt = largest(remotes, "roundTripTime");
  const codecOf = (entry: StatsEntry | null): string | null => {
    const id = entry === null ? null : entry["codecId"];
    return typeof id === "string" ? (codecs.get(id) ?? null) : null;
  };
  return {
    framesEncoded: sum("framesEncoded"),
    packetsSent: sum("packetsSent"),
    retransmittedPacketsSent: sum("retransmittedPacketsSent"),
    nackCount: sum("nackCount"),
    pliCount: sum("pliCount"),
    firCount: sum("firCount"),
    hugeFramesSent: sum("hugeFramesSent"),
    targetBitrate: targets.length === 0 ? null : targets.reduce((a, b) => a + b, 0),
    remoteFractionLost: largest(remotes, "fractionLost"),
    remoteRttMs: rtt === null ? null : rtt * 1000,
    availableOutgoingBitrate: pair === null ? null : numOrNull(pair, "availableOutgoingBitrate"),
    audioCodec: codecOf(audioOut),
    videoCodec: codecOf(first),
  };
}

/** One plain-text line on the sender for the interval since `before`. */
export function describeSender(label: string, before: SenderReading | null, now: SenderReading): string {
  const d = (key: "framesEncoded" | "packetsSent" | "retransmittedPacketsSent" | "nackCount" | "pliCount" | "firCount" | "hugeFramesSent"): number =>
    Math.max(0, now[key] - (before === null || before[key] > now[key] ? 0 : before[key]));
  const kbps = (bps: number | null): string => (bps === null ? "-" : (bps / 1000).toFixed(0));
  return (
    `${label} sender encoded=${d("framesEncoded")} pkts=${d("packetsSent")} rtx=${d("retransmittedPacketsSent")}` +
    ` nack_in=${d("nackCount")} pli_in=${d("pliCount")} fir_in=${d("firCount")} huge=${d("hugeFramesSent")}` +
    ` target_kbps=${kbps(now.targetBitrate)} estimate_kbps=${kbps(now.availableOutgoingBitrate)}` +
    ` remote_lost=${fixed(now.remoteFractionLost === null ? null : now.remoteFractionLost * 100, 0)}%` +
    ` remote_rtt_ms=${fixed(now.remoteRttMs, 0)}` +
    ` audio=${now.audioCodec ?? "-"} video=${now.videoCodec ?? "-"}`
  );
}
