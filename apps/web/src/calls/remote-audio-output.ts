// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The browser side of `remote-audio.ts`: one controller per hidden remote
 * sound sink, the path chosen once for the page, the shared Web Audio player,
 * and the gestures that retry a refused `play()`.
 */

import { isDesktop } from "../platform";
import {
  REMOTE_AUDIO_PATH_KEY,
  RemoteAudioSinkBinder,
  WebAudioRemotePlayer,
  chooseRemoteAudioPath,
  describeRemoteAudioSink,
  sinkSummary,
  type PlaybackContext,
  type RemoteAudioPath,
  type SinkReport,
  type WebAudioAttachment,
} from "./remote-audio";

/** How often each sink is looked at, milliseconds. Chosen, not measured. */
export const SINK_CHECK_MS = 2_000;
/** A line is written again at least this often while nothing changes. */
export const SINK_REPEAT_MS = 30_000;

function readOverride(): string | null {
  try {
    return globalThis.localStorage?.getItem(REMOTE_AUDIO_PATH_KEY) ?? null;
  } catch {
    return null;
  }
}

let pagePath: RemoteAudioPath | null = null;

/** The path this page plays remote sound through, decided once. */
export function remoteAudioPath(): RemoteAudioPath {
  if (pagePath === null) {
    pagePath = chooseRemoteAudioPath(
      isDesktop(),
      { userAgent: typeof navigator === "undefined" ? "" : navigator.userAgent },
      readOverride(),
    );
  }
  return pagePath;
}

const say = (line: string): void => {
  // eslint-disable-next-line no-console
  console.info(line);
};

let player: WebAudioRemotePlayer<MediaStream> | null = null;
function sharedPlayer(): WebAudioRemotePlayer<MediaStream> {
  if (player === null) {
    player = new WebAudioRemotePlayer<MediaStream>(
      () => new AudioContext() as unknown as PlaybackContext<MediaStream>,
      say,
    );
  }
  return player;
}

/** Live controllers, for the gesture hook. */
const live = new Set<RemoteAudioSinkController>();
let gestureHooked = false;

function onGesture(): void {
  player?.resume();
  for (const c of live) c.look();
}

function hookGestures(): void {
  if (gestureHooked || typeof document === "undefined") return;
  gestureHooked = true;
  document.addEventListener("pointerdown", onGesture, true);
  document.addEventListener("keydown", onGesture, true);
}

function unhookGesturesIfIdle(): void {
  if (!gestureHooked || live.size > 0 || typeof document === "undefined") return;
  gestureHooked = false;
  document.removeEventListener("pointerdown", onGesture, true);
  document.removeEventListener("keydown", onGesture, true);
}

let nextLabel = 1;

/**
 * Keeps one `<audio>` element playing one remote stream, plays it through
 * Web Audio where the page's path says so, and writes the sink's line.
 */
export class RemoteAudioSinkController {
  private readonly path = remoteAudioPath();
  private readonly binder = new RemoteAudioSinkBinder<MediaStream, MediaProvider>(this.path === "webaudio");
  private readonly label: string;
  private stream: MediaStream | null = null;
  private attachment: WebAudioAttachment | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;
  private lastTime: number | null = null;
  private lastSummary: string | null = null;
  private lastSaidAt = 0;
  private readonly onTracksChanged = (): void => {
    this.reattach();
    this.look();
  };

  constructor(
    private readonly el: HTMLAudioElement,
    kind: string,
  ) {
    this.label = `${kind}#${nextLabel}`;
    nextLabel += 1;
    live.add(this);
    hookGestures();
    this.timer = setInterval(() => {
      this.look();
      this.report(false);
    }, SINK_CHECK_MS);
  }

  /** The stream to play (or none). */
  setStream(stream: MediaStream | null): void {
    if (stream === this.stream) {
      this.look();
      return;
    }
    this.stream?.removeEventListener("addtrack", this.onTracksChanged);
    this.stream?.removeEventListener("removetrack", this.onTracksChanged);
    this.stream = stream;
    stream?.addEventListener("addtrack", this.onTracksChanged);
    stream?.addEventListener("removetrack", this.onTracksChanged);
    this.lastTime = null;
    this.reattach();
    this.look();
  }

  /** Bind and play if needed. */
  look(): void {
    this.binder.sync(this.el, this.stream);
    if (this.path === "webaudio" && this.attachment === null && this.stream !== null) {
      this.reattach();
    }
  }

  dispose(): void {
    if (this.timer !== null) clearInterval(this.timer);
    this.timer = null;
    this.stream?.removeEventListener("addtrack", this.onTracksChanged);
    this.stream?.removeEventListener("removetrack", this.onTracksChanged);
    this.attachment?.detach();
    this.attachment = null;
    this.binder.reset();
    this.el.srcObject = null;
    live.delete(this);
    unhookGesturesIfIdle();
  }

  private reattach(): void {
    if (this.path !== "webaudio") return;
    this.attachment?.detach();
    this.attachment = this.stream === null ? null : sharedPlayer().attach(this.stream);
  }

  private report(force: boolean): void {
    if (this.stream === null) return;
    const ct = this.el.currentTime;
    const moving = this.lastTime === null ? null : ct !== this.lastTime;
    this.lastTime = ct;
    const counts = this.binder.counts();
    const r: SinkReport = {
      label: this.label,
      path: this.path,
      paused: this.el.paused,
      readyState: this.el.readyState,
      timeMoving: moving,
      currentTime: ct,
      playCalls: counts.playCalls,
      playRefusals: counts.playRefusals,
      lastRefusal: counts.lastRefusal,
      tracks: this.stream.getAudioTracks(),
      contextState: this.attachment === null ? (this.path === "webaudio" ? "none" : null) : this.attachment.contextState(),
      outputLevel: this.attachment === null ? null : this.attachment.level(),
    };
    const summary = sinkSummary(r);
    const now = Date.now();
    if (!force && summary === this.lastSummary && now - this.lastSaidAt < SINK_REPEAT_MS) return;
    this.lastSummary = summary;
    this.lastSaidAt = now;
    say(describeRemoteAudioSink(r));
  }
}
