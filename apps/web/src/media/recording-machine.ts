// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 / UX spec §2–§3, §6 — the hold-to-record gesture as a pure state
 * machine.
 *
 * Every rule about WHEN a take starts, is sent, is locked, is cancelled or is
 * thrown away lives here, as `reduce(state, event) → { state, effects }`, so
 * it runs under vitest's plain `node` environment. The browser glue
 * (`recording-controller.ts`) only translates pointer / key / timer / capture
 * callbacks into events and performs the effects this returns.
 *
 * The rules, and where they come from:
 *
 *   * Press and hold starts recording immediately — once permission is
 *     known to be granted (§2 flow 1, §6).
 *   * Release sends (§2.3). Slide left past `CANCEL_SLIDE_PX` cancels
 *     (§2.4). Slide up past `LOCK_SLIDE_PX` locks (§2.5).
 *   * A take shorter than `MIN_TAKE_MS` is discarded with the
 *     `Hold to record` hint (§2.8) — for a TOUCH or PEN press. A MOUSE press
 *     that short is a click, and a click starts a LOCKED recording (§2 web
 *     note), so the same capture simply continues, locked.
 *   * A short touch tap also toggles mic / camera mode (§1: "after a tap
 *     toggles the mode").
 *   * Keyboard activation (no pointer gesture) starts a locked recording —
 *     the accessible single-activation path (§7).
 *   * The cap (`VOICE_CAP_MS` 15 min, `VIDEO_NOTE_CAP_MS` 60 s) stops the
 *     take into preview (§2.7, §3.3).
 *   * The hold that triggered a permission PROMPT does not record (§6). When
 *     the browser cannot say whether it prompted, a grant that took longer
 *     than `PROMPT_INFERENCE_MS` is treated as a prompt — the safe side is a
 *     take that did not start, never a take the user did not mean to make.
 *   * Permission revoked mid-take discards the take and shows the denied
 *     card (§6).
 */

export type CaptureKind = "voice" | "video_note";
export type PointerKind = "mouse" | "touch" | "pen";
/** What the browser reports before any capture is attempted. `unknown` is
 *  a browser with no Permissions API answer for this device. */
export type PermissionHint = "granted" | "prompt" | "denied" | "unknown";

/** §2.7 — the voice cap. */
export const VOICE_CAP_MS = 15 * 60 * 1000;
/** §3.3 — the video-note cap. */
export const VIDEO_NOTE_CAP_MS = 60 * 1000;
/** §2.8 — shorter takes are discarded (touch) or read as a click (mouse). */
export const MIN_TAKE_MS = 500;
/** §2.4 — how far left a held pointer travels before the take is cancelled. */
export const CANCEL_SLIDE_PX = 80;
/** §2.5 — how far up a held pointer travels before the take locks. */
export const LOCK_SLIDE_PX = 60;
/** A permission grant slower than this is assumed to have shown a prompt. */
export const PROMPT_INFERENCE_MS = 1000;

export function capFor(kind: CaptureKind): number {
  return kind === "voice" ? VOICE_CAP_MS : VIDEO_NOTE_CAP_MS;
}

export type RecState =
  | { readonly phase: "idle" }
  | {
      readonly phase: "requesting";
      readonly kind: CaptureKind;
      readonly pointer: PointerKind | "keyboard";
      readonly pressedAt: number;
      /** The pointer is still down. */
      readonly held: boolean;
      /** The browser said `prompt` up front. */
      readonly knownPrompt: boolean;
      readonly originX: number;
      readonly originY: number;
    }
  | {
      readonly phase: "recording";
      readonly kind: CaptureKind;
      readonly pointer: PointerKind;
      readonly startedAt: number;
      readonly originX: number;
      readonly originY: number;
      /** Current horizontal offset from the press point (negative = left),
       *  for the "slide to cancel" feedback. */
      readonly dx: number;
      readonly dy: number;
    }
  | { readonly phase: "locked"; readonly kind: CaptureKind; readonly startedAt: number }
  /** Waiting for the recorder to hand over the finished take. */
  | { readonly phase: "finalizing"; readonly kind: CaptureKind }
  | { readonly phase: "preview"; readonly kind: CaptureKind; readonly durationMs: number };

export type RecEvent =
  | {
      readonly type: "press";
      readonly kind: CaptureKind;
      readonly pointer: PointerKind;
      readonly x: number;
      readonly y: number;
      readonly at: number;
      readonly permission: PermissionHint;
    }
  | { readonly type: "move"; readonly x: number; readonly y: number }
  | { readonly type: "release"; readonly at: number }
  /** Keyboard / assistive-technology activation of the record button. */
  | {
      readonly type: "activate";
      readonly kind: CaptureKind;
      readonly at: number;
      readonly permission: PermissionHint;
    }
  | { readonly type: "permission-result"; readonly granted: boolean; readonly at: number }
  | { readonly type: "tick"; readonly at: number }
  | { readonly type: "stop"; readonly at: number }
  | { readonly type: "delete" }
  /** Esc, or a pointercancel the browser raised mid-gesture. */
  | { readonly type: "cancel" }
  | { readonly type: "send" }
  | { readonly type: "revoked" }
  | { readonly type: "capture-failed" }
  | { readonly type: "preview-ready"; readonly durationMs: number }
  | { readonly type: "preview-failed" };

export type RecHint = "hold-to-record" | "cancelled";

export type RecEffect =
  | { readonly type: "request-permission"; readonly kind: CaptureKind }
  | { readonly type: "start-capture"; readonly kind: CaptureKind }
  /** Release the stream a prompt produced without recording on it. */
  | { readonly type: "release-stream" }
  | { readonly type: "finish"; readonly disposition: "send" | "preview" | "discard" }
  | { readonly type: "hint"; readonly hint: RecHint }
  | { readonly type: "show-denied"; readonly kind: CaptureKind }
  | { readonly type: "toggle-mode" }
  | { readonly type: "send-preview" }
  | { readonly type: "discard-preview" };

export interface RecStep {
  readonly state: RecState;
  readonly effects: readonly RecEffect[];
}

export const IDLE: RecState = { phase: "idle" };

function step(state: RecState, ...effects: RecEffect[]): RecStep {
  return { state, effects };
}

/** Is a capture (MediaRecorder) running in this state? */
export function isCapturing(state: RecState): boolean {
  return state.phase === "recording" || state.phase === "locked";
}

export function reduce(state: RecState, event: RecEvent): RecStep {
  switch (state.phase) {
    case "idle":
      return fromIdle(event);
    case "requesting":
      return fromRequesting(state, event);
    case "recording":
      return fromRecording(state, event);
    case "locked":
      return fromLocked(state, event);
    case "finalizing":
      return fromFinalizing(state, event);
    case "preview":
      return fromPreview(state, event);
  }
}

function fromIdle(event: RecEvent): RecStep {
  if (event.type === "press") {
    if (event.permission === "denied") {
      return step(IDLE, { type: "show-denied", kind: event.kind });
    }
    if (event.permission === "granted") {
      return step(
        {
          phase: "recording",
          kind: event.kind,
          pointer: event.pointer,
          startedAt: event.at,
          originX: event.x,
          originY: event.y,
          dx: 0,
          dy: 0,
        },
        { type: "start-capture", kind: event.kind },
      );
    }
    return step(
      {
        phase: "requesting",
        kind: event.kind,
        pointer: event.pointer,
        pressedAt: event.at,
        held: true,
        knownPrompt: event.permission === "prompt",
        originX: event.x,
        originY: event.y,
      },
      { type: "request-permission", kind: event.kind },
    );
  }
  if (event.type === "activate") {
    if (event.permission === "denied") {
      return step(IDLE, { type: "show-denied", kind: event.kind });
    }
    if (event.permission === "granted") {
      return step(
        { phase: "locked", kind: event.kind, startedAt: event.at },
        { type: "start-capture", kind: event.kind },
      );
    }
    return step(
      {
        phase: "requesting",
        kind: event.kind,
        pointer: "keyboard",
        pressedAt: event.at,
        held: false,
        knownPrompt: event.permission === "prompt",
        originX: 0,
        originY: 0,
      },
      { type: "request-permission", kind: event.kind },
    );
  }
  return step(IDLE);
}

function fromRequesting(
  state: Extract<RecState, { phase: "requesting" }>,
  event: RecEvent,
): RecStep {
  switch (event.type) {
    case "release":
      return step({ ...state, held: false });
    case "cancel":
      // Nothing is recording yet; the grant that may still arrive must not
      // start one. Going idle does that: `permission-result` is ignored there.
      return step(IDLE);
    case "permission-result": {
      if (!event.granted) {
        return step(IDLE, { type: "show-denied", kind: state.kind });
      }
      const prompted =
        state.knownPrompt || event.at - state.pressedAt > PROMPT_INFERENCE_MS;
      if (prompted) {
        // §6: the gesture that raised the prompt does not record.
        return step(IDLE, { type: "release-stream" });
      }
      if (state.pointer === "keyboard") {
        return step(
          { phase: "locked", kind: state.kind, startedAt: event.at },
          { type: "start-capture", kind: state.kind },
        );
      }
      if (state.held) {
        return step(
          {
            phase: "recording",
            kind: state.kind,
            pointer: state.pointer,
            startedAt: event.at,
            originX: state.originX,
            originY: state.originY,
            dx: 0,
            dy: 0,
          },
          { type: "start-capture", kind: state.kind },
        );
      }
      // Released before the (prompt-less) grant arrived: a quick press.
      if (state.pointer === "mouse") {
        return step(
          { phase: "locked", kind: state.kind, startedAt: event.at },
          { type: "start-capture", kind: state.kind },
        );
      }
      return step(
        IDLE,
        { type: "release-stream" },
        { type: "hint", hint: "hold-to-record" },
        { type: "toggle-mode" },
      );
    }
    default:
      return step(state);
  }
}

function fromRecording(
  state: Extract<RecState, { phase: "recording" }>,
  event: RecEvent,
): RecStep {
  switch (event.type) {
    case "move": {
      const dx = event.x - state.originX;
      const dy = event.y - state.originY;
      if (dx <= -CANCEL_SLIDE_PX) {
        return step(
          IDLE,
          { type: "finish", disposition: "discard" },
          { type: "hint", hint: "cancelled" },
        );
      }
      if (dy <= -LOCK_SLIDE_PX) {
        return step({ phase: "locked", kind: state.kind, startedAt: state.startedAt });
      }
      return step({ ...state, dx, dy });
    }
    case "release": {
      if (event.at - state.startedAt < MIN_TAKE_MS) {
        if (state.pointer === "mouse") {
          // A click: the take already running becomes a locked recording.
          return step({ phase: "locked", kind: state.kind, startedAt: state.startedAt });
        }
        return step(
          IDLE,
          { type: "finish", disposition: "discard" },
          { type: "hint", hint: "hold-to-record" },
          { type: "toggle-mode" },
        );
      }
      return step(IDLE, { type: "finish", disposition: "send" });
    }
    case "tick":
      if (event.at - state.startedAt >= capFor(state.kind)) {
        return step(
          { phase: "finalizing", kind: state.kind },
          { type: "finish", disposition: "preview" },
        );
      }
      return step(state);
    case "cancel":
      return step(
        IDLE,
        { type: "finish", disposition: "discard" },
        { type: "hint", hint: "cancelled" },
      );
    case "delete":
      return step(IDLE, { type: "finish", disposition: "discard" });
    case "revoked":
      return step(
        IDLE,
        { type: "finish", disposition: "discard" },
        { type: "show-denied", kind: state.kind },
      );
    case "capture-failed":
      return step(IDLE, { type: "finish", disposition: "discard" });
    default:
      return step(state);
  }
}

function fromLocked(
  state: Extract<RecState, { phase: "locked" }>,
  event: RecEvent,
): RecStep {
  switch (event.type) {
    case "stop":
      if (event.at - state.startedAt < MIN_TAKE_MS) {
        return step(
          IDLE,
          { type: "finish", disposition: "discard" },
          { type: "hint", hint: "hold-to-record" },
        );
      }
      return step(
        { phase: "finalizing", kind: state.kind },
        { type: "finish", disposition: "preview" },
      );
    case "tick":
      if (event.at - state.startedAt >= capFor(state.kind)) {
        return step(
          { phase: "finalizing", kind: state.kind },
          { type: "finish", disposition: "preview" },
        );
      }
      return step(state);
    case "delete":
    case "cancel":
      return step(IDLE, { type: "finish", disposition: "discard" });
    case "revoked":
      return step(
        IDLE,
        { type: "finish", disposition: "discard" },
        { type: "show-denied", kind: state.kind },
      );
    case "capture-failed":
      return step(IDLE, { type: "finish", disposition: "discard" });
    default:
      return step(state);
  }
}

function fromFinalizing(
  state: Extract<RecState, { phase: "finalizing" }>,
  event: RecEvent,
): RecStep {
  switch (event.type) {
    case "preview-ready":
      if (event.durationMs < MIN_TAKE_MS) {
        return step(IDLE, { type: "discard-preview" }, { type: "hint", hint: "hold-to-record" });
      }
      return step({ phase: "preview", kind: state.kind, durationMs: event.durationMs });
    case "preview-failed":
    case "capture-failed":
      return step(IDLE);
    default:
      return step(state);
  }
}

function fromPreview(
  state: Extract<RecState, { phase: "preview" }>,
  event: RecEvent,
): RecStep {
  switch (event.type) {
    case "send":
      return step(IDLE, { type: "send-preview" });
    case "delete":
    case "cancel":
      return step(IDLE, { type: "discard-preview" });
    default:
      return step(state);
  }
}

/** `0:07`, `1:02`, `15:00` — the recording bar and bubble duration format. */
export function formatClock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

/** Spoken duration for labels: "12 seconds", "1 minute 5 seconds". */
export function spokenDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  const s = `${seconds} ${seconds === 1 ? "second" : "seconds"}`;
  if (minutes === 0) return s;
  const m = `${minutes} ${minutes === 1 ? "minute" : "minutes"}`;
  return seconds === 0 ? m : `${m} ${s}`;
}
