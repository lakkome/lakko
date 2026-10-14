// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Keeping a remote person's sound and picture together in a group call: the
 * web side of the shared rule (ADR-0070, note of 2026-10-12).
 *
 * A subscriber gets, per remote person, one audio track and up to three video
 * tracks: main (stream id `<peer>`), middle (`<peer>~mid`) and light
 * (`<peer>~light`). The browser's own lip sync covers the main track only (it
 * shares the audio's stream id), so a tile on the middle or the light picture
 * ran ahead of the sound. The rule that ties them is written once, in Rust
 * (`crates/lakko-ui-rules`, `av_align`), and reached here over wasm; iOS and
 * Android reach the same rule over UniFFI. Nothing of the rule is restated in
 * this file.
 *
 * What is here is what the rule cannot know:
 *
 *   * which receiver is which (person, audio | main | middle | light);
 *   * the receivers' statistics as the rule's inputs ({@link trackInput}):
 *     whether packets arrived in the interval, the interval average of
 *     `jitterBufferDelay / jitterBufferEmittedCount`, and the same over
 *     `jitterBufferMinimumDelay` where the browser reports it;
 *   * which track a tile draws ({@link shownTrack});
 *   * whether this browser can set `RTCRtpReceiver.jitterBufferTarget`,
 *     found out here and never assumed ({@link hasJitterTarget},
 *     {@link probeJitterTarget}), and the write itself;
 *   * the switch ({@link avAlignEnabled}) and the log lines.
 *
 * {@link CallAvAlign} holds one rule per remote person for one call. It has
 * no DOM, no timer and no `getStats()`: the caller reads the statistics, owns
 * the clock and says what is shown. Receivers are a structural type, so the
 * whole of it runs under test over plain objects.
 */

import {
  AvAlign as WasmAvAlign,
  AvTrack,
  avAlignCapability,
  avAlignReadingMs,
  avAudioReasonName,
  avCapabilityName,
  avVideoReasonName,
} from "../../wasm/lakko_ffi_wasm";
import { shownQuality } from "./video-given";
import type { VideoQuality } from "./video-wants";

/** One of a remote person's video tracks. */
export type AvVideoRole = "main" | "middle" | "light";

/** One of a remote person's receivers. */
export type AvRole = "audio" | AvVideoRole;

/** The video roles in the order the rule and the log line name them. */
export const AV_VIDEO_ROLES: readonly AvVideoRole[] = ["main", "middle", "light"];

// ---------------------------------------------------------------------------
// The switch
// ---------------------------------------------------------------------------

/**
 * Alignment is on unless switched off. Off, the rule still runs and the skew
 * is still logged, but nothing is written to a receiver: a call made with it
 * off gives the log to compare against.
 */
export const AV_ALIGN_DEFAULT_ON = true;

/**
 * The `localStorage` key that overrides {@link AV_ALIGN_DEFAULT_ON} on this
 * device: `off` (or `0`, `false`, `no`) switches alignment off, `on` (or `1`,
 * `true`, `yes`) on; anything else leaves the default. Read once per call, at
 * its start. In the browser console:
 * `localStorage.setItem("lakko.call.av-align", "off")`.
 */
export const AV_ALIGN_STORAGE_KEY = "lakko.call.av-align";

/** The part of `Storage` the switch reads. */
export interface SwitchStorage {
  getItem(key: string): string | null;
}

/** The page's `localStorage`, or `null` where there is none or it may not be
 *  touched. */
function pageStorage(): SwitchStorage | null {
  try {
    const storage = (globalThis as { localStorage?: unknown }).localStorage;
    if (typeof storage !== "object" || storage === null) return null;
    return typeof (storage as { getItem?: unknown }).getItem === "function"
      ? (storage as SwitchStorage)
      : null;
  } catch {
    return null;
  }
}

/** Whether alignment is on for a call that starts now. A storage that cannot
 *  be read leaves the default. */
export function avAlignEnabled(storage: SwitchStorage | null = pageStorage()): boolean {
  let raw: string | null = null;
  try {
    raw = storage?.getItem(AV_ALIGN_STORAGE_KEY) ?? null;
  } catch {
    raw = null;
  }
  const word = raw?.trim().toLowerCase();
  if (word === "off" || word === "0" || word === "false" || word === "no") return false;
  if (word === "on" || word === "1" || word === "true" || word === "yes") return true;
  return AV_ALIGN_DEFAULT_ON;
}

// ---------------------------------------------------------------------------
// Whether a receiver's target delay can be set
// ---------------------------------------------------------------------------

/** The part of `RTCRtpReceiver` the rule's answers are written to. */
export interface JitterTargetReceiver {
  jitterBufferTarget?: number | null;
}

/**
 * Whether `target` (a receiver, or `RTCRtpReceiver.prototype`) has the
 * `jitterBufferTarget` property at all. Presence is asked with `in`, never by
 * reading: a plain object takes any property that is written to it, so a
 * write alone would read back in a browser that has no such setter.
 */
export function hasJitterTarget(target: unknown): boolean {
  return typeof target === "object" && target !== null && "jitterBufferTarget" in target;
}

/** Whether this browser's receivers have the property, asked of the
 *  prototype, before any receiver exists. The call's first answer; a real
 *  receiver of each kind then confirms or corrects it. */
export function browserHasJitterTarget(): boolean {
  const ctor = (globalThis as { RTCRtpReceiver?: { prototype?: unknown } }).RTCRtpReceiver;
  return hasJitterTarget(ctor?.prototype);
}

/**
 * Whether `receiver`'s target delay can really be set: the property is
 * there, a write does not throw, and the written value reads back. The value
 * that was there before is put back. A receiver that already holds a target
 * someone set is taken to work and is not written.
 */
export function probeJitterTarget(receiver: JitterTargetReceiver): boolean {
  if (!hasJitterTarget(receiver)) return false;
  try {
    const before = receiver.jitterBufferTarget ?? null;
    if (before !== null) return true;
    receiver.jitterBufferTarget = 0;
    const took = receiver.jitterBufferTarget === 0;
    receiver.jitterBufferTarget = null;
    return took;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// Statistics as the rule's inputs
// ---------------------------------------------------------------------------

/** The cumulative counters of one `inbound-rtp` entry the rule's inputs are
 *  made of. */
export interface InboundSample {
  readonly packetsReceived: number;
  /** Seconds, summed over everything that left the jitter buffer. */
  readonly jitterBufferDelay: number;
  readonly jitterBufferEmittedCount: number;
  /** Seconds, the same sum for the delay the network alone would need; zero
   *  where the browser does not report it. */
  readonly jitterBufferMinimumDelay: number;
}

type StatsEntry = Record<string, unknown>;

function num(entry: StatsEntry, key: string): number {
  const v = entry[key];
  return typeof v === "number" && Number.isFinite(v) ? v : 0;
}

function sampleOfEntry(entry: StatsEntry): InboundSample {
  return {
    packetsReceived: num(entry, "packetsReceived"),
    jitterBufferDelay: num(entry, "jitterBufferDelay"),
    jitterBufferEmittedCount: num(entry, "jitterBufferEmittedCount"),
    jitterBufferMinimumDelay: num(entry, "jitterBufferMinimumDelay"),
  };
}

/** The `inbound-rtp` entries of a connection's report, by the id of the
 *  track each belongs to. */
export interface InboundSamples {
  readonly byTrack: ReadonlyMap<string, InboundSample>;
  /** Entries that do not say which track they belong to (a browser without
   *  `trackIdentifier`): their receivers have to be asked one by one. */
  readonly unidentified: number;
}

/** Reduce a connection's statistics report (as its entries) to the samples
 *  the rule needs. Nothing else of the report is read. */
export function inboundSamples(entries: Iterable<StatsEntry>): InboundSamples {
  const byTrack = new Map<string, InboundSample>();
  let unidentified = 0;
  for (const e of entries) {
    if (e["type"] !== "inbound-rtp") continue;
    const trackId = e["trackIdentifier"];
    if (typeof trackId !== "string" || trackId.length === 0) {
      unidentified += 1;
      continue;
    }
    byTrack.set(trackId, sampleOfEntry(e));
  }
  return { byTrack, unidentified };
}

/** The sample of one receiver's own report (`receiver.getStats()`, which
 *  every engine scopes to the receiver); `null` when it has no `inbound-rtp`
 *  entry. */
export function receiverSample(entries: Iterable<StatsEntry>): InboundSample | null {
  for (const e of entries) {
    if (e["type"] === "inbound-rtp") return sampleOfEntry(e);
  }
  return null;
}

/** What one receiver reports for one interval, as the rule takes it. */
export interface TrackInput {
  /** Packets arrived in the interval. */
  readonly flowing: boolean;
  /** Average time in the jitter buffer over the interval, whole
   *  milliseconds; `undefined` when nothing left the buffer. */
  readonly delayMs: number | undefined;
  /** The delay the network alone would need, whole milliseconds; `undefined`
   *  when not measured. */
  readonly networkMs: number | undefined;
}

/** A receiver with no reading: not flowing, nothing measured. */
export const NO_TRACK_INPUT: TrackInput = { flowing: false, delayMs: undefined, networkMs: undefined };

/**
 * One interval of one receiver: `cur` against `prev`. With no `prev`, or with
 * counters that went backwards (a stream replaced under the same track), the
 * interval is measured from zero. With no `cur` the receiver reported
 * nothing.
 */
export function trackInput(prev: InboundSample | null, cur: InboundSample | null): TrackInput {
  if (cur === null) return NO_TRACK_INPUT;
  const base =
    prev !== null &&
    prev.packetsReceived <= cur.packetsReceived &&
    prev.jitterBufferEmittedCount <= cur.jitterBufferEmittedCount
      ? prev
      : null;
  const d = (key: keyof InboundSample): number => Math.max(0, cur[key] - (base === null ? 0 : base[key]));
  const emitted = d("jitterBufferEmittedCount");
  const perEmittedMs = (seconds: number): number => Math.max(0, Math.round((seconds / emitted) * 1000));
  const network = d("jitterBufferMinimumDelay");
  return {
    flowing: d("packetsReceived") > 0,
    delayMs: emitted > 0 ? perEmittedMs(d("jitterBufferDelay")) : undefined,
    networkMs: emitted > 0 && network > 0 ? perEmittedMs(network) : undefined,
  };
}

// ---------------------------------------------------------------------------
// Which track a tile draws
// ---------------------------------------------------------------------------

/** What is known of how a person is drawn now. */
export interface ShownFacts {
  /** A tile draws a picture of the person at all. */
  readonly hasTile: boolean;
  /** The picture this device asks the SFU for of them (`video-wants.ts`). */
  readonly wanted: VideoQuality;
  /** What the SFU says it gives (`video-given.ts`); `null` when it has not
   *  said. */
  readonly given: VideoQuality | null;
  /** The person has a middle track / a light companion. */
  readonly hasMiddle: boolean;
  readonly hasLight: boolean;
  /** The tile's light element is the one on top (`layer-display.ts`). */
  readonly lightOnTop: boolean;
}

/**
 * The one track a tile draws of a person now, or `null` when none does. The
 * tile's own arithmetic, asked the same way the tile asks it: the heavy
 * picture is the lower of what is wanted and what is given
 * (`shownQuality`), the middle picture is the main track while the person
 * has no middle one, and the light copy is drawn when it is on top.
 */
export function shownTrack(facts: ShownFacts): AvVideoRole | null {
  if (!facts.hasTile) return null;
  const quality = shownQuality(facts.wanted, facts.given, facts.hasLight);
  if (quality === "light") return facts.hasLight ? "light" : null;
  if (facts.lightOnTop && facts.hasLight) return "light";
  return quality === "middle" && facts.hasMiddle ? "middle" : "main";
}

// ---------------------------------------------------------------------------
// The shared rule, as this module reaches it
// ---------------------------------------------------------------------------

/** The part of the shared `AvAlign` this module calls. Tracks are 0 main,
 *  1 middle, 2 light, as the shared `AvTrack`. */
export interface AvAlignRule {
  setAudio(flowing: boolean, delayMs?: number | null, networkMs?: number | null): void;
  setVideo(
    track: number,
    flowing: boolean,
    shown: boolean,
    engineSynced: boolean,
    delayMs?: number | null,
    networkMs?: number | null,
  ): void;
  read(nowMs: number): void;
  audioSetMs(): number | undefined;
  audioAskedMs(): number;
  audioReason(): number;
  videoSetMs(track: number): number | undefined;
  videoAskedMs(track: number): number;
  videoSkewMs(track: number): number | undefined;
  videoReason(track: number): number;
  receiversReplaced(): void;
  free(): void;
}

/** The shared rule and the words it has for its answers. */
export interface AvAlignRules {
  /** A person's rule; throws while the wasm module is not ready. */
  create(audioSettable: boolean, videoSettable: boolean): AvAlignRule;
  /** How often the rule expects to be read, milliseconds. */
  readingMs(): number;
  capabilityName(audioSettable: boolean, videoSettable: boolean): string;
  audioReasonName(reason: number): string;
  videoReasonName(reason: number): string;
}

/** The rule every client shares, over wasm. */
export const sharedAvAlignRules: AvAlignRules = {
  create: (audioSettable, videoSettable) => new WasmAvAlign(audioSettable, videoSettable),
  readingMs: () => avAlignReadingMs(),
  capabilityName: (audioSettable, videoSettable) =>
    avCapabilityName(avAlignCapability(audioSettable, videoSettable)),
  audioReasonName: (reason) => avAudioReasonName(reason),
  videoReasonName: (reason) => avVideoReasonName(reason),
};

const TRACK_OF: Record<AvVideoRole, number> = {
  main: AvTrack.Main,
  middle: AvTrack.Middle,
  light: AvTrack.Light,
};

/** The reading interval used when the rule cannot be asked (the wasm module
 *  is not loaded) or answers something a timer cannot run at, milliseconds. */
export const AV_READING_FALLBACK_MS = 1000;

/** The rule's reading interval as a timer period: what the rule says, or
 *  {@link AV_READING_FALLBACK_MS} when it cannot say or says something
 *  outside a quarter of a second to two seconds (the longest interval the
 *  rule counts in full). */
export function avReadingIntervalMs(rules: AvAlignRules = sharedAvAlignRules): number {
  try {
    const ms = rules.readingMs();
    return Number.isFinite(ms) && ms >= 250 && ms <= 2000 ? Math.round(ms) : AV_READING_FALLBACK_MS;
  } catch {
    return AV_READING_FALLBACK_MS;
  }
}

// ---------------------------------------------------------------------------
// The log lines
// ---------------------------------------------------------------------------

/** What the last reading said of a person's audio. */
export interface AudioSyncState {
  readonly delayMs: number | undefined;
  readonly askedMs: number;
  readonly reason: string;
}

/** What the last reading said of one of a person's video tracks. */
export interface VideoSyncState {
  readonly role: AvVideoRole;
  readonly delayMs: number | undefined;
  readonly askedMs: number;
  /** The track's delay minus the audio's; `undefined` when either was not
   *  measured. */
  readonly skewMs: number | undefined;
  readonly reason: string;
}

/** What the last reading said of one person. */
export interface PeerSyncState {
  readonly audio: AudioSyncState;
  /** The video tracks the person has, main first. */
  readonly video: readonly VideoSyncState[];
}

function ms(value: number | undefined): string {
  return value === undefined ? "-" : String(Math.round(value));
}

/**
 * One person in one line (log format: ASCII, no names; `peerId` is the SFU's
 * per-call peer id). The audio as `delay/ask=asked/reason`, each video track
 * the person has as `delay/ask=asked/skew=skew/reason`; a figure that was
 * not measured is `-`, `ask=0` is nothing asked. The reasons are the shared
 * rule's own words. For example:
 *
 * `call media interval sync peer=p7 audio=420/ask=0/free main=431/ask=0/skew=11/engine-synced light=418/ask=420/skew=-2/aligned`
 */
export function describeSync(label: string, peerId: string, state: PeerSyncState): string {
  const audio = `audio=${ms(state.audio.delayMs)}/ask=${state.audio.askedMs}/${state.audio.reason}`;
  const video = state.video.map(
    (v) => `${v.role}=${ms(v.delayMs)}/ask=${v.askedMs}/skew=${ms(v.skewMs)}/${v.reason}`,
  );
  return [`${label} sync peer=${peerId}`, audio, ...video].join(" ");
}

function yesNo(value: boolean): string {
  return value ? "yes" : "no";
}

/** Where a capability answer came from: the receiver prototype (before any
 *  receiver exists) or real receivers. */
export type CapabilitySource = "prototype" | "receiver";

/**
 * What the rule can achieve in this browser, in one line. `capability` is
 * the shared rule's word for the two setters this browser has; `enabled=no`
 * means the switch is off and nothing is written whatever the browser can
 * do. For example:
 *
 * `call media sync capability=full audio_setter=yes video_setter=yes enabled=yes probe=prototype interval_ms=1000`
 */
export function describeCapability(
  label: string,
  capability: string,
  audioSettable: boolean,
  videoSettable: boolean,
  enabled: boolean,
  source: CapabilitySource,
  intervalMs: number,
): string {
  return (
    `${label} sync capability=${capability} audio_setter=${yesNo(audioSettable)}` +
    ` video_setter=${yesNo(videoSettable)} enabled=${yesNo(enabled)} probe=${source}` +
    ` interval_ms=${intervalMs}`
  );
}

// ---------------------------------------------------------------------------
// One call
// ---------------------------------------------------------------------------

interface Slot<R> {
  readonly receiver: R;
  readonly trackId: string;
  prev: InboundSample | null;
}

interface Person<R> {
  rule: AvAlignRule | null;
  /** Kinds whose write threw on one of this person's receivers. */
  audioWriteFailed: boolean;
  videoWriteFailed: boolean;
  readonly slots: Map<AvRole, Slot<R>>;
  last: PeerSyncState | null;
}

/** What a {@link CallAvAlign} is built with. */
export interface CallAvAlignOptions {
  /** The switch ({@link avAlignEnabled}). Off: observe and log only. */
  readonly enabled: boolean;
  /** The browser's receivers have `jitterBufferTarget`
   *  ({@link browserHasJitterTarget}). */
  readonly prototypeSettable: boolean;
  /** The shared rule; the real one by default. */
  readonly rules?: AvAlignRules;
  /** Where a warning goes (a write that threw). */
  readonly warn?: (message: string, err?: unknown) => void;
  /** Where the capability line goes, at the start and whenever a receiver
   *  corrects it. */
  readonly info?: (line: string) => void;
  /** The label of the capability line. */
  readonly label?: string;
}

/** One existing receiver of a call, for a caller that has to ask it for its
 *  own statistics. */
export interface TrackedReceiver<R> {
  readonly peerId: string;
  readonly role: AvRole;
  readonly trackId: string;
  readonly receiver: R;
}

/**
 * The alignment of every remote person of one call.
 *
 * The caller registers each receiver as its track arrives
 * ({@link setReceiver}) and takes it away when the track ends or the person
 * leaves; once per reading interval it hands over the receivers' samples and
 * says what is shown ({@link read}), and the rule's answers are written to
 * the receivers here. A person's rule is built at their first reading and
 * freed when they leave or the call ends ({@link dispose}); nothing else of
 * the wasm module is held.
 *
 * Capability is found out, not assumed: the prototype's answer at the start,
 * then a write that reads back on the first audio and the first video
 * receiver. A write that later throws is warned of once per receiver, and
 * that person's rule is built again as one that cannot set that kind, so it
 * goes on measuring and stops asking.
 */
export class CallAvAlign<R extends JitterTargetReceiver = JitterTargetReceiver> {
  private readonly rules: AvAlignRules;
  private readonly enabled: boolean;
  private readonly warn: (message: string, err?: unknown) => void;
  private readonly info: (line: string) => void;
  private readonly label: string;
  private readonly people = new Map<string, Person<R>>();
  private readonly warned = new WeakSet<object>();
  private readonly settable: Record<"audio" | "video", boolean>;
  private readonly probed: Record<"audio" | "video", boolean> = { audio: false, video: false };
  private disposed = false;

  constructor(options: CallAvAlignOptions) {
    this.rules = options.rules ?? sharedAvAlignRules;
    this.enabled = options.enabled;
    this.warn = options.warn ?? ((): void => undefined);
    this.info = options.info ?? ((): void => undefined);
    this.label = options.label ?? "call media";
    this.settable = { audio: options.prototypeSettable, video: options.prototypeSettable };
    this.sayCapability("prototype");
  }

  /** How often {@link read} is to be called, milliseconds. */
  readingMs(): number {
    return avReadingIntervalMs(this.rules);
  }

  /** Whether this browser can set the target of an audio / a video
   *  receiver, as far as is known now. */
  browserSettable(kind: "audio" | "video"): boolean {
    return this.settable[kind];
  }

  /** `peerId` has a receiver for `role`. */
  has(peerId: string, role: AvRole): boolean {
    return this.people.get(peerId)?.slots.has(role) ?? false;
  }

  /** Every receiver registered now. */
  receivers(): TrackedReceiver<R>[] {
    const out: TrackedReceiver<R>[] = [];
    for (const [peerId, person] of this.people) {
      for (const [role, slot] of person.slots) {
        out.push({ peerId, role, trackId: slot.trackId, receiver: slot.receiver });
      }
    }
    return out;
  }

  /**
   * `peerId`'s `role` track arrived on `receiver`. The first audio and the
   * first video receiver of the call are probed. A different receiver under
   * a role the person already had means their receivers were replaced: the
   * rule is told, and tells every receiver its target again.
   */
  setReceiver(peerId: string, role: AvRole, receiver: R, trackId: string): void {
    if (this.disposed) return;
    this.probe(role === "audio" ? "audio" : "video", receiver);
    const person = this.person(peerId);
    const before = person.slots.get(role);
    if (before !== undefined && before.receiver !== receiver) person.rule?.receiversReplaced();
    if (before === undefined || before.receiver !== receiver || before.trackId !== trackId) {
      person.slots.set(role, { receiver, trackId, prev: null });
    }
  }

  /** `peerId`'s `role` track ended. With `receiver`, only if it is still the
   *  one registered (a later track under the same role stays). */
  removeReceiver(peerId: string, role: AvRole, receiver?: R): void {
    const person = this.people.get(peerId);
    const slot = person?.slots.get(role);
    if (person === undefined || slot === undefined) return;
    if (receiver !== undefined && slot.receiver !== receiver) return;
    person.slots.delete(role);
  }

  /** `peerId` left the call: their rule is freed. */
  removePeer(peerId: string): void {
    const person = this.people.get(peerId);
    if (person === undefined) return;
    this.freeRule(person);
    this.people.delete(peerId);
  }

  /** Every receiver of the call was recreated (a renegotiation that made new
   *  receiver objects): nothing has been told to them. */
  receiversReplaced(): void {
    for (const person of this.people.values()) {
      person.rule?.receiversReplaced();
      for (const slot of person.slots.values()) slot.prev = null;
    }
  }

  /**
   * One reading interval, taken at `nowMs` (one clock, whole milliseconds).
   * `byTrack` holds the sample of every receiver that reported, by track id;
   * `shownOf` says which one track a tile draws of a person now. The rule's
   * answers are written to the receivers before this returns.
   */
  read(
    nowMs: number,
    byTrack: ReadonlyMap<string, InboundSample>,
    shownOf: (peerId: string) => AvVideoRole | null,
  ): void {
    if (this.disposed) return;
    for (const [peerId, person] of this.people) {
      if (person.slots.size === 0) {
        person.last = null;
        continue;
      }
      const rule = this.ruleOf(person);
      // The samples move on whether or not the rule could be asked, so the
      // first interval it is asked about is one interval long.
      const inputs = new Map<AvRole, TrackInput>();
      for (const [role, slot] of person.slots) {
        const cur = byTrack.get(slot.trackId) ?? null;
        inputs.set(role, trackInput(slot.prev, cur));
        if (cur !== null) slot.prev = cur;
      }
      if (rule === null) {
        person.last = null;
        continue;
      }
      const shown = shownOf(peerId);
      const audio = inputs.get("audio");
      if (audio !== undefined) rule.setAudio(audio.flowing, audio.delayMs, audio.networkMs);
      for (const role of AV_VIDEO_ROLES) {
        const input = inputs.get(role);
        if (input === undefined) continue;
        rule.setVideo(
          TRACK_OF[role],
          input.flowing,
          shown === role,
          // The browser synchronises the main track itself: it shares the
          // audio's stream id.
          role === "main",
          input.delayMs,
          input.networkMs,
        );
      }
      rule.read(Math.max(0, Math.round(nowMs)));
      person.last = {
        audio: {
          delayMs: audio?.delayMs,
          askedMs: rule.audioAskedMs(),
          reason: this.rules.audioReasonName(rule.audioReason()),
        },
        video: AV_VIDEO_ROLES.filter((role) => person.slots.has(role)).map((role) => ({
          role,
          delayMs: inputs.get(role)?.delayMs,
          askedMs: rule.videoAskedMs(TRACK_OF[role]),
          skewMs: rule.videoSkewMs(TRACK_OF[role]),
          reason: this.rules.videoReasonName(rule.videoReason(TRACK_OF[role])),
        })),
      };
      // The answers are read before any is written: a write that throws
      // builds the person's rule again, and the old one is gone then.
      const sets: [AvRole, number][] = [];
      const audioSet = rule.audioSetMs();
      if (audioSet !== undefined) sets.push(["audio", audioSet]);
      for (const role of AV_VIDEO_ROLES) {
        const set = rule.videoSetMs(TRACK_OF[role]);
        if (set !== undefined) sets.push([role, set]);
      }
      for (const [role, set] of sets) this.write(peerId, person, role, set);
    }
  }

  /** One line per person the last reading covered, in the order they
   *  joined. */
  lines(label: string): string[] {
    const out: string[] = [];
    for (const [peerId, person] of this.people) {
      if (person.last !== null) out.push(describeSync(label, peerId, person.last));
    }
    return out;
  }

  /** The call is over: every rule is freed. Nothing is read or written
   *  afterwards. */
  dispose(): void {
    for (const person of this.people.values()) this.freeRule(person);
    this.people.clear();
    this.disposed = true;
  }

  private person(peerId: string): Person<R> {
    let person = this.people.get(peerId);
    if (person === undefined) {
      person = {
        rule: null,
        audioWriteFailed: false,
        videoWriteFailed: false,
        slots: new Map(),
        last: null,
      };
      this.people.set(peerId, person);
    }
    return person;
  }

  /** What `person`'s rule is told it can set: nothing with the switch off,
   *  else what the browser can, less what failed on their receivers. */
  private flags(person: Person<R>): [boolean, boolean] {
    return [
      this.enabled && this.settable.audio && !person.audioWriteFailed,
      this.enabled && this.settable.video && !person.videoWriteFailed,
    ];
  }

  /** The person's rule, built on first use; `null` while the wasm module is
   *  not ready. */
  private ruleOf(person: Person<R>): AvAlignRule | null {
    if (person.rule !== null) return person.rule;
    try {
      person.rule = this.rules.create(...this.flags(person));
    } catch {
      person.rule = null;
    }
    return person.rule;
  }

  private freeRule(person: Person<R>): void {
    const rule = person.rule;
    person.rule = null;
    person.last = null;
    try {
      rule?.free();
    } catch {
      // Already freed with the module; nothing is held either way.
    }
  }

  /** The rule takes what it can set when it is built, so a change of that
   *  builds it again; it starts as for a person who has just joined. */
  private rebuild(person: Person<R>): void {
    this.freeRule(person);
    for (const slot of person.slots.values()) slot.prev = null;
  }

  /** The first receiver of a kind says whether its target can really be
   *  set; an answer that differs from the prototype's builds every rule
   *  again and is logged. Not asked with the switch off: nothing is written
   *  then, the probe included. */
  private probe(kind: "audio" | "video", receiver: R): void {
    if (!this.enabled || this.probed[kind]) return;
    this.probed[kind] = true;
    const settable = probeJitterTarget(receiver);
    if (settable === this.settable[kind]) return;
    this.settable[kind] = settable;
    for (const person of this.people.values()) this.rebuild(person);
    this.sayCapability("receiver");
  }

  private sayCapability(source: CapabilitySource): void {
    let capability = "-";
    try {
      capability = this.rules.capabilityName(this.settable.audio, this.settable.video);
    } catch {
      // The wasm module is not ready: the line still says what was found.
    }
    this.info(
      describeCapability(
        this.label,
        capability,
        this.settable.audio,
        this.settable.video,
        this.enabled,
        source,
        this.readingMs(),
      ),
    );
  }

  /** Put the rule's answer on a receiver: `0` gives the target back. */
  private write(peerId: string, person: Person<R>, role: AvRole, setMs: number): void {
    const slot = person.slots.get(role);
    if (slot === undefined) return;
    try {
      slot.receiver.jitterBufferTarget = setMs === 0 ? null : setMs;
    } catch (err) {
      const kind = role === "audio" ? "audio" : "video";
      if (!this.warned.has(slot.receiver)) {
        this.warned.add(slot.receiver);
        this.warn(
          `${this.label} sync peer=${peerId} ${role}: jitterBufferTarget could not be set; ` +
            `${kind} targets of this person are no longer asked`,
          err,
        );
      }
      const failed = kind === "audio" ? person.audioWriteFailed : person.videoWriteFailed;
      if (failed) return;
      if (kind === "audio") person.audioWriteFailed = true;
      else person.videoWriteFailed = true;
      this.rebuild(person);
    }
  }
}
