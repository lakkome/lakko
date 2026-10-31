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
 *   * A press does not record at once. It opens a `pressing` phase in which
 *     NOTHING is captured, and the microphone is asked for only once the
 *     pointer has been down for `HOLD_TO_RECORD_MS` (§2 flow 1, §6).
 *   * Asking for it and having it are two different moments, and the machine
 *     spends the gap between them in `requesting`. A capturing phase is
 *     entered ONLY on the event that delivers the stream, so `startedAt` is
 *     when capture began rather than when the machine decided to capture.
 *   * A press released before that, without drifting past `TAP_SLOP_PX`, is a
 *     TAP: it switches mic / camera mode and records nothing (§3.1). The
 *     composer is ONE button, so the tap is the only way a pointer reaches the
 *     other mode, and it must not leave a take behind.
 *   * Release sends (§2.3). Slide left past `CANCEL_SLIDE_PX` cancels
 *     (§2.4). Slide up past `LOCK_SLIDE_PX` locks (§2.5).
 *   * A take shorter than `MIN_TAKE_MS` is discarded with the
 *     `Hold to record` hint (§2.8), for every pointer kind alike, and is NOT
 *     read as a mode switch: the press had already opened the microphone, so
 *     it is the accidental-press case and nothing else (§3.1). The tap and the
 *     too-short take are told apart by the gesture — how long the pointer was
 *     down — and never by what the recorder later produced.
 *   * Keyboard activation (no pointer gesture) starts a locked recording —
 *     the accessible single-activation path (§7). Switching mode without a
 *     pointer is a separate control the view carries (§3.1), not an event
 *     here.
 *   * The cap (`VOICE_CAP_MS` 15 min, `VIDEO_NOTE_CAP_MS` 60 s) stops the
 *     take into preview (§2.7, §3.3).
 *   * The hold that triggered a permission PROMPT does not record (§6). When
 *     the browser cannot say whether it prompted, a grant that took longer
 *     than `PROMPT_INFERENCE_MS` is treated as a prompt — the safe side is a
 *     take that did not start, never a take the user did not mean to make.
 *     Both rules apply only once the pointer has come up: a prompt cannot be
 *     answered with the pointer held on the button.
 *   * Permission revoked mid-take discards the take and shows the denied
 *     card (§6).
 */
import { t } from "../l10n";

export type CaptureKind = "voice" | "video_note";
export type PointerKind = "mouse" | "touch" | "pen";
/** What the browser reports before any capture is attempted. `unknown` is
 *  a browser with no Permissions API answer for this device. */
export type PermissionHint = "granted" | "prompt" | "denied" | "unknown";

/**
 * Whether opening the stream could put a permission prompt on the screen.
 *
 * Kept apart from {@link PermissionHint} because the two answer different
 * questions once `granted` no longer short-circuits the acquire: the hint is
 * what the browser said before the gesture, this is what the machine must do
 * with a grant that arrives slowly.
 *
 *   * `none` — the browser reported the permission as already granted, so the
 *     acquire is the device opening and nothing else. However long it takes,
 *     the take records: there was no prompt for a slow grant to have been.
 *   * `certain` — the browser reported `prompt`. §6: the gesture that raises
 *     the prompt does not record.
 *   * `possible` — the browser would not say (Firefox and Safari, for camera
 *     and microphone), so the signal left is how long the grant took
 *     (`PROMPT_INFERENCE_MS`).
 *
 * Neither applies while the pointer is still down: nobody answers a prompt
 * with the pointer held on the record button, so a grant under a held pointer
 * raised none. `prompt` is not proof of a dialog. The macOS desktop webview
 * reports it on every launch while the system grant stands, and the stream
 * arrives in milliseconds: the first take after each launch was lost in
 * silence until this rule.
 */
export type PromptRisk = "none" | "certain" | "possible";

function promptRiskOf(permission: PermissionHint): PromptRisk {
  if (permission === "granted") return "none";
  if (permission === "prompt") return "certain";
  // `denied` never reaches here: both callers answer it with the card first.
  return "possible";
}

/** §2.7 — the voice cap. */
export const VOICE_CAP_MS = 15 * 60 * 1000;
/** §3.3 — the video-note cap. */
export const VIDEO_NOTE_CAP_MS = 60 * 1000;
/** §2.8 — shorter takes are discarded with the `Hold to record` hint. */
export const MIN_TAKE_MS = 500;
/**
 * §3.1 — how long a pointer stays down before the microphone opens.
 *
 * Under it the press is a TAP, which switches the mode; at it the press is a
 * HOLD, which records. The two rules of §3.1 — "a tap never records" and "a
 * take under the floor is an accidental press, not a mode switch" — can only
 * both hold if the decision is made BEFORE any capture starts, which is what
 * this threshold is. Kept well under `MIN_TAKE_MS` so the accidental press it
 * describes stays reachable: a hold released between the two is a real take,
 * too short to send.
 */
export const HOLD_TO_RECORD_MS = 200;
/**
 * How far a pointer may drift while still being a tap.
 *
 * A press that travels further is a drag — the beginning of a cancel or a lock
 * — and releasing it switches nothing, because flipping the composer's mode
 * under a finger that was reaching for the cancel target would be the last
 * thing it meant.
 */
export const TAP_SLOP_PX = 10;
/** §2.4 — how far left a held pointer travels before the take is cancelled. */
export const CANCEL_SLIDE_PX = 80;
/** §2.5 — how far up a held pointer travels before the take locks. */
export const LOCK_SLIDE_PX = 60;
/** A permission grant slower than this, with the pointer up, is assumed to
 *  have shown a prompt. */
export const PROMPT_INFERENCE_MS = 1000;

export function capFor(kind: CaptureKind): number {
  return kind === "voice" ? VOICE_CAP_MS : VIDEO_NOTE_CAP_MS;
}

export type RecState =
  | { readonly phase: "idle" }
  /**
   * The pointer is down and NOTHING is captured yet (§3.1).
   *
   * The composer must not change while this lasts: from the person's side a
   * tap that switches the mode and a hold that starts a take begin
   * identically, and swapping the text field for a recording bar on every tap
   * is the flicker §3.1 was written against. `recorderBusy` therefore reads
   * this phase as not busy, and the button does not light up.
   */
  | {
      readonly phase: "pressing";
      readonly kind: CaptureKind;
      readonly pointer: PointerKind;
      readonly pressedAt: number;
      /** What the browser said about permission when the press landed; spent
       *  only if the press becomes a hold, so a tap prompts for nothing. */
      readonly permission: PermissionHint;
      readonly originX: number;
      readonly originY: number;
      readonly dx: number;
      readonly dy: number;
    }
  /**
   * The stream is being OPENED, and nothing is captured yet.
   *
   * EVERY take passes through here, including one on a device whose
   * permission the browser has already reported as `granted`. That report
   * means no prompt will appear; it does not mean the microphone is in hand.
   * `getUserMedia` still has to open an audio device, which takes real time,
   * and the defect this phase now covers was the recording bar running across
   * that gap — clock counting, red dot pulsing — so the first words of a
   * message were spoken into a recorder that did not exist yet.
   *
   * The rule, and the one the machine is tested against: a capturing phase is
   * reached ONLY through `permission-result`, the event that delivers the
   * stream. Nothing else can produce a `recording` or a `locked`.
   */
  | {
      readonly phase: "requesting";
      readonly kind: CaptureKind;
      readonly pointer: PointerKind | "keyboard";
      /** When the request went out, which is what `PROMPT_INFERENCE_MS` is
       *  measured against. */
      readonly pressedAt: number;
      /** The pointer is still down. */
      readonly held: boolean;
      /** Whether a prompt can have been raised by this acquire. */
      readonly promptRisk: PromptRisk;
      readonly originX: number;
      readonly originY: number;
      /** The offset the press carried in, kept up to date while the device
       *  opens: the pointer goes on travelling, and the cancel and lock
       *  targets are answered from it the moment the stream lands. */
      readonly dx: number;
      readonly dy: number;
    }
  | {
      readonly phase: "recording";
      readonly kind: CaptureKind;
      /**
       * No `pointer` here, and its absence is a rule: since §3.1 a mouse and
       * a finger answer a release identically, and a field nothing reads is
       * an invitation to re-branch on the pointer kind - which is exactly the
       * divergence §3.1 was written to end.
       */
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
    case "pressing":
      return fromPressing(state, event);
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
    // No effect at all, deliberately: a press might still turn out to be a
    // tap, and a tap neither records nor asks for permission. Everything the
    // decision needs is carried until `HOLD_TO_RECORD_MS` has passed - the
    // denied card included, which would otherwise appear over a gesture that
    // was only reaching for the other mode.
    return step({
      phase: "pressing",
      kind: event.kind,
      pointer: event.pointer,
      pressedAt: event.at,
      permission: event.permission,
      originX: event.x,
      originY: event.y,
      dx: 0,
      dy: 0,
    });
  }
  if (event.type === "activate") {
    if (event.permission === "denied") {
      return step(IDLE, { type: "show-denied", kind: event.kind });
    }
    // Including `granted`: a keyboard activation has the same device to open
    // as a hold does, and locking the recorder before it is open would run
    // the clock over nothing exactly as the pointer path did.
    return step(
      {
        phase: "requesting",
        kind: event.kind,
        pointer: "keyboard",
        pressedAt: event.at,
        held: false,
        promptRisk: promptRiskOf(event.permission),
        originX: 0,
        originY: 0,
        dx: 0,
        dy: 0,
      },
      { type: "request-permission", kind: event.kind },
    );
  }
  return step(IDLE);
}

/** Did the pointer stay close enough to the press point to still be a tap? */
function withinTapSlop(state: Extract<RecState, { phase: "pressing" }>): boolean {
  return Math.abs(state.dx) <= TAP_SLOP_PX && Math.abs(state.dy) <= TAP_SLOP_PX;
}

/**
 * The window in which a press has not yet decided what it is (§3.1).
 *
 * `tick` is the hold threshold arriving, and it is the only door out of here
 * that opens the microphone.
 */
function fromPressing(
  state: Extract<RecState, { phase: "pressing" }>,
  event: RecEvent,
): RecStep {
  switch (event.type) {
    case "move":
      return step({ ...state, dx: event.x - state.originX, dy: event.y - state.originY });
    case "release": {
      if (!withinTapSlop(state)) return step(IDLE);
      if (event.at - state.pressedAt < HOLD_TO_RECORD_MS) {
        // §3.1 — a tap, and the only gesture that switches the mode. No hint:
        // nothing went wrong and nothing was thrown away.
        return step(IDLE, { type: "toggle-mode" });
      }
      // Past the threshold with no capture to show for it - the timer was
      // late (a throttled tab), so the press was a hold that never got going.
      // The honest answer is the hint, never a mode switch the person did not
      // ask for.
      return step(IDLE, { type: "hint", hint: "hold-to-record" });
    }
    case "tick": {
      if (event.at - state.pressedAt < HOLD_TO_RECORD_MS) return step(state);
      if (state.dx <= -CANCEL_SLIDE_PX) {
        // Already sitting in the cancel target. Opening the microphone here
        // would only be to shut it on the next move event.
        return step(IDLE);
      }
      if (state.permission === "denied") {
        return step(IDLE, { type: "show-denied", kind: state.kind });
      }
      // `granted` included, and that is the whole of the fix: the threshold
      // ASKS for the microphone, it does not have it. A grant already on
      // record only means the ask is silent.
      return step(
        {
          phase: "requesting",
          kind: state.kind,
          pointer: state.pointer,
          // The moment the request goes out, not the moment of the press:
          // `PROMPT_INFERENCE_MS` measures how long the GRANT took, and
          // charging the hold threshold against that budget would read a
          // prompt-less grant as a prompt.
          pressedAt: event.at,
          held: true,
          promptRisk: promptRiskOf(state.permission),
          originX: state.originX,
          originY: state.originY,
          dx: state.dx,
          dy: state.dy,
        },
        { type: "request-permission", kind: state.kind },
      );
    }
    case "cancel":
      return step(IDLE);
    default:
      return step(state);
  }
}

function fromRequesting(
  state: Extract<RecState, { phase: "requesting" }>,
  event: RecEvent,
): RecStep {
  switch (event.type) {
    case "move":
      // The device takes time to open and the pointer does not wait for it.
      // Tracked so the gesture is answered from where the finger actually is
      // when the stream lands, not from where it was when the request left.
      return step({ ...state, dx: event.x - state.originX, dy: event.y - state.originY });
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
        !state.held &&
        (state.promptRisk === "certain" ||
          (state.promptRisk === "possible" && event.at - state.pressedAt > PROMPT_INFERENCE_MS));
      if (prompted) {
        // §6: the gesture that raised the prompt does not record.
        return step(IDLE, { type: "release-stream" });
      }
      if (state.dx <= -CANCEL_SLIDE_PX) {
        // Already sitting in the cancel target, exactly as `fromPressing`
        // reads it: a take that exists only to be discarded on the next event
        // is not worth starting. Silent, because nothing was ever recorded to
        // report the loss of.
        return step(IDLE, { type: "release-stream" });
      }
      // The lock is answered before the release is, and for both pointer and
      // keyboard: a pointer that has climbed onto the target has locked, and
      // lifting it is what locking is FOR.
      if (state.pointer === "keyboard" || state.dy <= -LOCK_SLIDE_PX) {
        return step(
          { phase: "locked", kind: state.kind, startedAt: event.at },
          { type: "start-capture", kind: state.kind },
        );
      }
      if (!state.held) {
        // Released before the (prompt-less) grant arrived. The hold threshold
        // is already behind it - nothing reaches this state without crossing
        // it - so this was a hold that ended before anything could record, not
        // a tap, and it does not switch the mode.
        return step(
          IDLE,
          { type: "release-stream" },
          { type: "hint", hint: "hold-to-record" },
        );
      }
      return step(
        {
          phase: "recording",
          kind: state.kind,
          // The stream is in hand on THIS event, so this is the instant
          // capture begins - the clock, the elapsed time and the take's own
          // duration all measure from the same moment.
          startedAt: event.at,
          originX: state.originX,
          originY: state.originY,
          dx: state.dx,
          dy: state.dy,
        },
        { type: "start-capture", kind: state.kind },
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
        // §3.1 - the accidental press, and nothing else. The microphone is
        // already open by the time a take exists, so this is never read back
        // as a tap on the mode switch; every pointer kind is treated alike,
        // because a mouse reaches the mode through the same tap a finger does.
        return step(
          IDLE,
          { type: "finish", disposition: "discard" },
          { type: "hint", hint: "hold-to-record" },
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

/**
 * The TOTAL duration of a recording or received media message: rounds UP to
 * the next whole second, so any non-zero duration below one second reads
 * `0:01` instead of `0:00`. Elapsed / playback-position counters keep
 * `formatClock` (rounded down). Display only — never the wire value.
 */
export function formatTotalClock(ms: number): string {
  if (!(ms > 0)) return formatClock(0);
  return formatClock(Math.ceil(ms / 1000) * 1000);
}

/** Spoken duration for labels: "12 seconds", "1 minute 5 seconds". */
export function spokenDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  const s = t("common.duration.seconds", { count: seconds });
  if (minutes === 0) return s;
  const m = t("common.duration.minutes", { count: minutes });
  return seconds === 0 ? m : t("common.duration.minutes_seconds", { minutes: m, seconds: s });
}
