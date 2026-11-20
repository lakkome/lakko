// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The main page's half of the desktop call window (`docs/ux/calls.md`
 * section 4.5.4a; the rules are in `desktop-call-window-model.ts`).
 *
 * In the desktop app `CallWindow.tsx` still builds the collapsed call's
 * picture in the main page, out of sight, so its `<video>` elements play the
 * same streams with the same layer choice the in-app window would make. This
 * module draws the shown one into a canvas at the OS window's size and posts
 * it to the host, and posts the window's view whenever it changes. Nothing
 * here leaves the device: the host keeps the latest image in memory and hands
 * it to the second window.
 */

import { postCallWindowFrame, postCallWindowView, type CallWindowSlot } from "../platform/host";
import {
  FRAME_INTERVAL_MS,
  FRAME_JPEG_QUALITY,
  FrameSchedule,
  INSET_ASPECT,
  INSET_WIDTH_FRACTION,
  OS_CALL_WINDOW_HEIGHT,
  OS_CALL_WINDOW_WIDTH,
  drawPlan,
  frameSize,
  pickShownVideo,
  sameView,
  type CallWindowView,
} from "./desktop-call-window-model";

/** The call surface colour behind a letterboxed screen share
 *  (`styles.css` `.lk-call-window`). */
const SURFACE_BG = "#0f1220";

/** How often the feed rate is logged. */
const RATE_LOG_MS = 10_000;

export interface CallWindowRelayOptions {
  /** The hidden picture the frames are drawn from. */
  readonly root: HTMLElement;
  /** The window's view, or `null` while there is nothing to show yet. */
  readonly view: () => CallWindowView | null;
  /** The picture is a shared screen: drawn whole, letterboxed. */
  readonly pictureIsScreen: () => boolean;
  /** The picture is your own camera: mirrored, as the call screen draws it. */
  readonly pictureMirrored: () => boolean;
}

interface Slot {
  readonly name: CallWindowSlot;
  readonly schedule: FrameSchedule;
  /** Something was sent that the window may still be showing. */
  sent: boolean;
  canvas: HTMLCanvasElement | null;
}

/**
 * Start relaying. Returns the function that stops it and hides the window.
 */
export function startCallWindowRelay(options: CallWindowRelayOptions): () => void {
  let stopped = false;
  let lastView: CallWindowView | null = null;
  // Counts only, logged every 10 s: how fast the window is fed, so the first
  // run on an OS whose hidden-page throttling is not measured shows it.
  const rate = { ticks: 0, frames: 0, since: performance.now() };
  const slots: Slot[] = [
    { name: "picture", schedule: new FrameSchedule(), sent: false, canvas: null },
    { name: "inset", schedule: new FrameSchedule(), sent: false, canvas: null },
  ];

  const postView = (): void => {
    const view = options.view();
    if (view === null || sameView(view, lastView)) return;
    lastView = view;
    void postCallWindowView(view);
  };

  const clear = (slot: Slot): void => {
    slot.schedule.reset();
    if (!slot.sent) return;
    slot.sent = false;
    void postCallWindowFrame(slot.name, new Uint8Array(0));
  };

  const send = (slot: Slot, draw: (canvas: HTMLCanvasElement) => boolean, still: string | null): void => {
    const now = performance.now();
    slot.schedule.started(now, still);
    const canvas = slot.canvas ?? document.createElement("canvas");
    slot.canvas = canvas;
    if (!draw(canvas)) {
      slot.schedule.finished();
      return;
    }
    canvas.toBlob(
      (blob) => {
        if (blob === null || stopped) {
          slot.schedule.finished();
          return;
        }
        void blob
          .arrayBuffer()
          .then(async (buffer) => {
            if (stopped) return;
            const ok = await postCallWindowFrame(slot.name, new Uint8Array(buffer));
            if (ok) {
              slot.sent = true;
              rate.frames += 1;
            }
            // Not taken (it raced the window being shown or hidden): a photo
            // is sent again rather than counted as delivered.
            else slot.schedule.reset();
          })
          .catch(() => {
            // The next tick tries again.
          })
          .finally(() => slot.schedule.finished());
      },
      "image/jpeg",
      FRAME_JPEG_QUALITY,
    );
  };

  /** Draw `source` (its own size `sw` x `sh`) into `canvas` at `w` x `h`. */
  const drawInto = (
    canvas: HTMLCanvasElement,
    source: CanvasImageSource,
    sw: number,
    sh: number,
    w: number,
    h: number,
    fit: "cover" | "contain",
    mirrored: boolean,
  ): boolean => {
    const plan = drawPlan(sw, sh, w, h, fit);
    if (plan === null) return false;
    if (canvas.width !== w) canvas.width = w;
    if (canvas.height !== h) canvas.height = h;
    const ctx = canvas.getContext("2d");
    if (ctx === null) return false;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = SURFACE_BG;
    ctx.fillRect(0, 0, w, h);
    if (mirrored) ctx.setTransform(-1, 0, 0, 1, w, 0);
    try {
      ctx.drawImage(source, plan.sx, plan.sy, plan.sw, plan.sh, plan.dx, plan.dy, plan.dw, plan.dh);
    } catch {
      return false;
    }
    return true;
  };

  const ratio = (): number => globalThis.devicePixelRatio ?? 1;

  const tickPicture = (slot: Slot): void => {
    const view = options.view();
    if (view === null || view.picture.kind !== "frame") {
      clear(slot);
      return;
    }
    const { width, height } = frameSize(OS_CALL_WINDOW_WIDTH, OS_CALL_WINDOW_HEIGHT, ratio());
    const box = options.root.querySelector(".lk-call-window-picture");
    const video = pickShownVideo(Array.from(box?.querySelectorAll("video") ?? []));
    const now = performance.now();
    if (video !== null) {
      if (!slot.schedule.dueForVideo(now)) return;
      const fit = options.pictureIsScreen() ? "contain" : "cover";
      const mirrored = options.pictureMirrored();
      send(
        slot,
        (c) => drawInto(c, video, video.videoWidth, video.videoHeight, width, height, fit, mirrored),
        null,
      );
      return;
    }
    // A photo: sent once, and again only when it changes.
    const img = box?.querySelector("img") ?? null;
    if (img !== null && img.complete && img.naturalWidth > 0) {
      const key = img.currentSrc || img.src;
      if (!slot.schedule.dueForStill(key)) return;
      send(
        slot,
        (c) => drawInto(c, img, img.naturalWidth, img.naturalHeight, width, height, "cover", false),
        key,
      );
      return;
    }
    clear(slot);
  };

  const tickInset = (slot: Slot): void => {
    const view = options.view();
    const video =
      view?.inset === true
        ? pickShownVideo(Array.from(options.root.querySelectorAll<HTMLVideoElement>(".lk-call-window-inset video")))
        : null;
    if (video === null) {
      clear(slot);
      return;
    }
    if (!slot.schedule.dueForVideo(performance.now())) return;
    const insetWidth = OS_CALL_WINDOW_WIDTH * INSET_WIDTH_FRACTION;
    const { width, height } = frameSize(insetWidth, insetWidth / INSET_ASPECT, ratio());
    send(
      slot,
      (c) => drawInto(c, video, video.videoWidth, video.videoHeight, width, height, "cover", true),
      null,
    );
  };

  const tick = (): void => {
    if (stopped) return;
    rate.ticks += 1;
    const now = performance.now();
    if (now - rate.since >= RATE_LOG_MS) {
      const secs = (now - rate.since) / 1000;
      console.info(
        `[call-window] ${(rate.ticks / secs).toFixed(1)} ticks/s ${(rate.frames / secs).toFixed(1)} frames/s hidden=${document.hidden}`,
      );
      rate.ticks = 0;
      rate.frames = 0;
      rate.since = now;
    }
    postView();
    const [picture, inset] = slots;
    if (picture !== undefined) tickPicture(picture);
    if (inset !== undefined) tickInset(inset);
  };

  // Ticks at a third of the frame interval so a frame is never late by a
  // whole one; the schedule keeps the rate.
  const stopClock = startClock(tick, Math.round(FRAME_INTERVAL_MS / 3));
  const releaseLock = holdLock();
  tick();

  return () => {
    stopped = true;
    stopClock();
    releaseLock();
    void postCallWindowView(null);
  };
}

/**
 * Call `onTick` every `everyMs`, from a worker's clock
 * (`desktop-call-window-ticker.ts`): the page's own timers fire once a second
 * while Lakko is minimised, and the collapsed call's window is there to be
 * watched while it is. Without a worker, the page's own timer.
 */
function startClock(onTick: () => void, everyMs: number): () => void {
  let worker: Worker | null = null;
  try {
    worker = new Worker(new URL("./desktop-call-window-ticker.ts", import.meta.url), { type: "module" });
  } catch {
    worker = null;
  }
  if (worker === null) {
    const timer = globalThis.setInterval(onTick, everyMs);
    return () => globalThis.clearInterval(timer);
  }
  const w = worker;
  let fallback: ReturnType<typeof setInterval> | null = null;
  w.onmessage = () => onTick();
  // A worker that fails to load is replaced by the page's own timer.
  w.onerror = () => {
    w.terminate();
    if (fallback === null) fallback = globalThis.setInterval(onTick, everyMs);
  };
  w.postMessage({ every: everyMs });
  return () => {
    w.terminate();
    if (fallback !== null) globalThis.clearInterval(fallback);
  };
}

/**
 * Hold a Web Lock while the window is relayed: the workaround Tauri names for
 * a hidden page being suspended on Windows and Linux (a page holding a lock
 * is not frozen in Chromium). On macOS 27.2 it changed no timer rate in the
 * probe; Windows and Linux are not measured.
 */
function holdLock(): () => void {
  const locks = typeof navigator === "undefined" ? undefined : navigator.locks;
  if (locks === undefined) return () => {};
  let release: (() => void) | null = null;
  let released = false;
  locks
    .request("lakko-call-window", () => new Promise<void>((resolve) => {
      if (released) resolve();
      else release = resolve;
    }))
    .catch(() => {
      // No lock: the relay runs without one.
    });
  return () => {
    released = true;
    release?.();
  };
}
