// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * How a call's remote sound reaches the speakers, the same way for every call
 * kind, and what the console is told about it.
 *
 * Seen 2026-11-07 in the desktop app (macOS 27.2, WebKit): in a group call the
 * others heard the person and the person heard nobody, while the remote sound
 * arrived and was decoded (the sound-level analyser on each remote stream read
 * speech, `audio#N pkts=...`). The system's voice processor, which WebKit
 * renders remote call sound through while the microphone is captured with echo
 * cancellation (WebKit registers its stream renderer as the voice processor's
 * "speaker samples producer"), logged its far-end and downlink nodes as
 * digital silence (`rms -120 dB`, `issue_type=Silence`) every ten seconds for
 * the whole call; its uplink carried the microphone. The same silence is in
 * every WebKit call session the Mac's log still held that day. The hidden
 * `<audio>` sinks were never asked to `play()` and nothing said whether they
 * played.
 *
 * So, here:
 *
 *   * Every sink is bound once it is in the document and `play()` is called;
 *     a refusal is kept and retried on the next gesture and on a slow timer.
 *   * Where {@link remoteAudioThroughWebAudio} says so (the desktop app on
 *     WebKit), the sound is played through Web Audio instead: one
 *     `AudioContext`, a `MediaStreamAudioSourceNode` per remote stream, into
 *     the context's destination; the element stays bound and playing but
 *     muted, so nothing plays twice. Web Audio is the path that demonstrably
 *     carried the decoded sound in that call. A per-device switch
 *     ({@link REMOTE_AUDIO_PATH_KEY}) forces either path, for comparing them.
 *   * Each sink writes one line a little after it is bound and again whenever
 *     what it says changes ({@link describeRemoteAudioSink}): the path, the
 *     element's `paused`, `readyState`, whether `currentTime` moves, the last
 *     `play()` refusal, the audio tracks, and on the Web Audio path the
 *     context state and the level the graph sends to the speakers.
 */

/** The browser this decides for, as far as it is read here. */
export interface EngineHint {
  readonly userAgent: string;
}

/** Which way a remote stream's sound is played. */
export type RemoteAudioPath = "element" | "webaudio";

/** `localStorage` key that forces a path on this device: `element` or
 *  `webaudio`. Anything else (or nothing) leaves the default. */
export const REMOTE_AUDIO_PATH_KEY = "lakko.call.remoteAudioPath";

/** A WebKit engine that is not Chrome, Edge or Firefox. */
export function isWebKitEngine(engine: EngineHint): boolean {
  const ua = engine.userAgent;
  return (
    ua.includes("AppleWebKit") &&
    !ua.includes("Chrome/") &&
    !ua.includes("Edg/") &&
    !ua.includes("Firefox/")
  );
}

/**
 * Whether remote call sound plays through Web Audio by default: in the
 * desktop app on a WebKit engine only. Safari is not included: it was not
 * seen failing on its own, and a browser that plays the element correctly
 * gains nothing from the change.
 */
export function remoteAudioThroughWebAudio(desktop: boolean, engine: EngineHint): boolean {
  return desktop && isWebKitEngine(engine);
}

/** The path to use: the device's switch if it names one, else the default. */
export function chooseRemoteAudioPath(
  desktop: boolean,
  engine: EngineHint,
  override: string | null,
): RemoteAudioPath {
  if (override === "element" || override === "webaudio") return override;
  return remoteAudioThroughWebAudio(desktop, engine) ? "webaudio" : "element";
}

/** The part of an `<audio>` element used here. */
export interface SinkElement<S> {
  srcObject: S | null;
  muted: boolean;
  readonly paused: boolean;
  readonly readyState: number;
  readonly currentTime: number;
  readonly isConnected?: boolean;
  play?: () => Promise<void> | undefined;
}

/** The part of a track read here. */
export interface SinkTrack {
  readonly readyState?: string;
  readonly muted?: boolean;
  readonly enabled?: boolean;
}

/** The part of a stream read here. */
export interface SinkStream {
  getAudioTracks(): readonly SinkTrack[];
}

/** A `play()` rejection as `<name>: <message>`, one line, bounded. */
export function playRefusalOf(err: unknown): string {
  const e = err as { name?: unknown; message?: unknown } | null;
  const name = typeof e?.name === "string" ? e.name : "Error";
  const message = typeof e?.message === "string" ? e.message : String(err);
  return `${name}: ${message}`.replace(/\s+/g, " ").slice(0, 160);
}

/**
 * Binds one sink element to one stream and keeps it playing.
 *
 * `sync` is called when the element appears, when the stream changes, on each
 * gesture and on a slow timer. An element not in the document is left alone.
 */
export class RemoteAudioSinkBinder<S extends E & SinkStream, E = S> {
  private bound: S | null = null;
  private playCalls = 0;
  private playRefusals = 0;
  private lastRefusal: string | null = null;
  private inFlight = false;
  /** Bumped by each `play()`, so a late answer to an older one is ignored. */
  private generation = 0;

  /** @param muteElement the element is kept silent (the Web Audio path). */
  constructor(private readonly muteElement: boolean) {}

  sync(el: SinkElement<E>, stream: S | null | undefined): void {
    if (el.isConnected === false) return;
    if (el.muted !== this.muteElement) el.muted = this.muteElement;
    const next = stream ?? null;
    let rebound = false;
    if (next !== this.bound) {
      this.bound = next;
      el.srcObject = next;
      rebound = true;
    }
    if (next !== null && (rebound || el.paused)) this.play(el, rebound);
  }

  /** The element is gone. */
  reset(): void {
    this.bound = null;
  }

  counts(): { playCalls: number; playRefusals: number; lastRefusal: string | null } {
    return {
      playCalls: this.playCalls,
      playRefusals: this.playRefusals,
      lastRefusal: this.lastRefusal,
    };
  }

  /** `force`: a new stream was bound, so a `play()` still pending for the old
   *  one does not stand in for this one. */
  private play(el: SinkElement<E>, force: boolean): void {
    if (typeof el.play !== "function" || (this.inFlight && !force)) return;
    this.playCalls += 1;
    this.generation += 1;
    const generation = this.generation;
    try {
      const result = el.play();
      if (result !== undefined && typeof result.then === "function") {
        this.inFlight = true;
        result.then(
          () => {
            if (generation !== this.generation) return;
            this.inFlight = false;
            this.lastRefusal = null;
          },
          (err: unknown) => {
            if (generation !== this.generation) return;
            this.inFlight = false;
            this.playRefusals += 1;
            this.lastRefusal = playRefusalOf(err);
          },
        );
      }
    } catch (err) {
      this.inFlight = false;
      this.playRefusals += 1;
      this.lastRefusal = playRefusalOf(err);
    }
  }
}

/** What one sink line says. */
export interface SinkReport {
  readonly label: string;
  readonly path: RemoteAudioPath;
  readonly paused: boolean;
  readonly readyState: number;
  /** `true`: `currentTime` moved since the last look; `null`: first look. */
  readonly timeMoving: boolean | null;
  readonly currentTime: number;
  readonly playCalls: number;
  readonly playRefusals: number;
  readonly lastRefusal: string | null;
  readonly tracks: readonly SinkTrack[];
  /** Web Audio path only: the context state, `null` elsewhere. */
  readonly contextState: string | null;
  /** Web Audio path only: RMS level the graph sends out, `null` elsewhere or
   *  when it cannot be read. */
  readonly outputLevel: number | null;
}

function trackPart(t: SinkTrack): string {
  return `${t.readyState ?? "-"}/muted=${t.muted === undefined ? "-" : String(t.muted)}/enabled=${
    t.enabled === undefined ? "-" : String(t.enabled)
  }`;
}

/**
 * One console line for a sink. The key of the line (everything but the
 * moving numbers) is what decides when it is written again.
 */
export function describeRemoteAudioSink(r: SinkReport): string {
  return `[call] remote audio ${r.label}: ${sinkSummary(r)} ct=${r.currentTime.toFixed(1)} play_calls=${r.playCalls}${
    r.outputLevel === null ? "" : ` out_level=${r.outputLevel.toFixed(3)}`
  }`;
}

/** The part of the line that is compared between looks. */
export function sinkSummary(r: SinkReport): string {
  const moving = r.timeMoving === null ? "-" : r.timeMoving ? "yes" : "no";
  const tracks = r.tracks.length === 0 ? "none" : r.tracks.map(trackPart).join(",");
  return (
    `path=${r.path} paused=${String(r.paused)} ready=${r.readyState} time_moving=${moving}` +
    ` play_refusals=${r.playRefusals} play_error=${r.lastRefusal ?? "-"} audio=${tracks}` +
    (r.contextState === null ? "" : ` ctx=${r.contextState}`)
  );
}

/** RMS of a block of samples. */
export function rmsOf(buffer: ArrayLike<number>): number {
  if (buffer.length === 0) return 0;
  let sum = 0;
  for (let i = 0; i < buffer.length; i += 1) {
    const v = buffer[i] ?? 0;
    sum += v * v;
  }
  return Math.sqrt(sum / buffer.length);
}

// ---------------------------------------------------------------------------
// The Web Audio path.

/** The part of an `AudioNode` used here. */
export interface GraphNode {
  connect(destination: GraphNode): unknown;
  disconnect(): void;
}

/** The part of an `AnalyserNode` used here. */
export interface LevelNode extends GraphNode {
  fftSize: number;
  getFloatTimeDomainData(array: Float32Array<ArrayBuffer>): void;
}

/** The part of an `AudioContext` used here. */
export interface PlaybackContext<S> {
  readonly state: string;
  readonly destination: GraphNode;
  createMediaStreamSource(stream: S): GraphNode;
  createAnalyser(): LevelNode;
  resume(): Promise<void>;
  close(): Promise<void>;
}

/** One stream played through the shared context. */
export interface WebAudioAttachment {
  /** What the context sends out now (all attached streams), or `null`. */
  level(): number | null;
  contextState(): string;
  detach(): void;
}

/** Window of samples the output level is read over. */
const LEVEL_WINDOW = 2048;

/**
 * Plays remote streams through one `AudioContext`. Made lazily on the first
 * stream; closed when the last one is detached.
 */
export class WebAudioRemotePlayer<S extends SinkStream> {
  private ctx: PlaybackContext<S> | null = null;
  private meter: LevelNode | null = null;
  private attached = 0;
  private readonly buffer = new Float32Array(new ArrayBuffer(LEVEL_WINDOW * 4));

  constructor(
    private readonly makeContext: () => PlaybackContext<S>,
    private readonly say: (line: string) => void,
  ) {}

  /**
   * Play `stream`. Returns `null` when the stream has no audio track yet (a
   * `MediaStreamAudioSourceNode` needs one) or the context could not be made;
   * the caller tries again when the stream gains a track.
   */
  attach(stream: S): WebAudioAttachment | null {
    if (stream.getAudioTracks().length === 0) return null;
    const ctx = this.ensure();
    if (ctx === null || this.meter === null) return null;
    let source: GraphNode;
    try {
      source = ctx.createMediaStreamSource(stream);
      source.connect(ctx.destination);
      source.connect(this.meter);
    } catch (err) {
      this.say(`[call] remote audio: could not play through web audio: ${playRefusalOf(err)}`);
      return null;
    }
    this.attached += 1;
    this.resume();
    let detached = false;
    return {
      level: (): number | null => this.level(),
      contextState: (): string => ctx.state,
      detach: (): void => {
        if (detached) return;
        detached = true;
        try {
          source.disconnect();
        } catch {
          // Already gone with its stream.
        }
        this.attached -= 1;
        if (this.attached === 0) this.close();
      },
    };
  }

  /** Ask a suspended context to run; called on every gesture. */
  resume(): void {
    const ctx = this.ctx;
    if (ctx === null || ctx.state === "running" || ctx.state === "closed") return;
    void ctx.resume().catch(() => undefined);
  }

  private level(): number | null {
    const meter = this.meter;
    const ctx = this.ctx;
    if (meter === null || ctx === null || ctx.state !== "running") return null;
    try {
      meter.getFloatTimeDomainData(this.buffer);
      return rmsOf(this.buffer);
    } catch {
      return null;
    }
  }

  private ensure(): PlaybackContext<S> | null {
    if (this.ctx !== null) return this.ctx;
    try {
      const ctx = this.makeContext();
      const meter = ctx.createAnalyser();
      meter.fftSize = LEVEL_WINDOW;
      this.ctx = ctx;
      this.meter = meter;
      this.say(`[call] remote audio: web audio context ${ctx.state}`);
      return ctx;
    } catch (err) {
      this.say(`[call] remote audio: no web audio context: ${playRefusalOf(err)}`);
      return null;
    }
  }

  private close(): void {
    const ctx = this.ctx;
    this.ctx = null;
    this.meter = null;
    if (ctx !== null) void ctx.close().catch(() => undefined);
  }
}
