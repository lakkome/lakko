// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 / UX spec §2–§3, §6 — the hold-to-record gesture.
 *
 * Each case drives the pure reducer the way the controller does and asserts
 * both the resulting phase and the effects, because the effects are the
 * observable behaviour: `finish: send` is a message reaching the other side,
 * `finish: discard` is a take thrown away.
 *
 * Since §3.1 the composer is ONE button, so a press is ambiguous until it has
 * lasted `HOLD_TO_RECORD_MS`: under that it is a tap that switches mic /
 * camera mode, at it a hold that records. Almost every case here therefore
 * starts from {@link hold} rather than a bare press — a bare press is only a
 * press, and nothing is captured on it.
 */
import { describe, expect, it } from "vitest";

import {
  CANCEL_SLIDE_PX,
  HOLD_TO_RECORD_MS,
  IDLE,
  LOCK_SLIDE_PX,
  MIN_TAKE_MS,
  PROMPT_INFERENCE_MS,
  TAP_SLOP_PX,
  VIDEO_NOTE_CAP_MS,
  VOICE_CAP_MS,
  formatClock,
  formatTotalClock,
  reduce,
  spokenDuration,
  type RecEffect,
  type RecEvent,
  type RecState,
} from "./recording-machine";

function run(events: RecEvent[], start: RecState = IDLE): { state: RecState; effects: RecEffect[] } {
  let state = start;
  const effects: RecEffect[] = [];
  for (const e of events) {
    const next = reduce(state, e);
    state = next.state;
    effects.push(...next.effects);
  }
  return { state, effects };
}

type PressEvent = Extract<RecEvent, { type: "press" }>;

/** When the press lands. */
const PRESS_AT = 1000;
/** When the microphone opens: the press plus the hold threshold. */
const HELD_AT = PRESS_AT + HOLD_TO_RECORD_MS;

const press = (over: Partial<PressEvent> = {}): RecEvent => ({
  type: "press",
  kind: "voice",
  pointer: "touch",
  x: 100,
  y: 100,
  at: PRESS_AT,
  permission: "granted",
  ...over,
});

/**
 * A press that has become a HOLD: the press, then the threshold arriving.
 *
 * The controller's one hold timer is what delivers that tick in the running
 * app (`recording-controller.ts`); here it is spelled out, because the whole
 * §3.1 distinction lives in whether it arrived.
 */
function hold(over: Partial<PressEvent> = {}): RecEvent[] {
  const at = over.at ?? PRESS_AT;
  return [press({ ...over, at }), { type: "tick", at: at + HOLD_TO_RECORD_MS }];
}

/** A press released before the threshold: §3.1's tap. */
function tap(over: Partial<PressEvent> = {}): RecEvent[] {
  const at = over.at ?? PRESS_AT;
  return [press({ ...over, at }), { type: "release", at: at + HOLD_TO_RECORD_MS - 1 }];
}

describe("one button: tap switches, hold records (§3.1)", () => {
  it("a tap switches the mode and records nothing at all", () => {
    const r = run(tap());
    expect(r.state).toEqual(IDLE);
    // Exactly one effect, and it is not a capture: the composer's only
    // pointer route to the other mode, and the whole reason a tap must not
    // leave a take behind.
    expect(r.effects).toEqual([{ type: "toggle-mode" }]);
  });

  it("a mouse click is the same gesture as a finger tap", () => {
    // The old rule made a click start a LOCKED recording, so the camera could
    // not be reached with a mouse at all once the second button was gone.
    for (const pointer of ["mouse", "touch", "pen"] as const) {
      expect(run(tap({ pointer })).effects).toEqual([{ type: "toggle-mode" }]);
    }
  });

  it("a press asks for nothing until it becomes a hold", () => {
    // Not even the permission prompt or the denied card: both belong to a
    // gesture that meant to record, and a tap did not.
    expect(run(tap({ permission: "prompt" })).effects).toEqual([{ type: "toggle-mode" }]);
    expect(run(tap({ permission: "unknown" })).effects).toEqual([{ type: "toggle-mode" }]);
    expect(run(tap({ permission: "denied" })).effects).toEqual([{ type: "toggle-mode" }]);
  });

  it("the press sits in `pressing` with nothing captured until the threshold", () => {
    const pressed = run([press()]);
    expect(pressed.state.phase).toBe("pressing");
    expect(pressed.effects).toEqual([]);
    // A tick a millisecond early changes nothing.
    const early = run([{ type: "tick", at: HELD_AT - 1 }], pressed.state);
    expect(early.state.phase).toBe("pressing");
    expect(early.effects).toEqual([]);
  });

  it("the threshold opens the microphone, not a millisecond before", () => {
    const r = run(hold());
    expect(r.state).toMatchObject({ phase: "recording", startedAt: HELD_AT });
    expect(r.effects).toEqual([{ type: "start-capture", kind: "voice" }]);
  });

  it("a release exactly at the threshold is a hold that never got going", () => {
    // A throttled tab can hold the timer past the threshold. The honest answer
    // is the hint - the person was holding - and never a mode switch they did
    // not ask for.
    const r = run([press(), { type: "release", at: HELD_AT }]);
    expect(r.state).toEqual(IDLE);
    expect(r.effects).toEqual([{ type: "hint", hint: "hold-to-record" }]);
  });

  it("a press that drifts past the slop switches nothing on release", () => {
    // A finger already travelling toward the cancel target: flipping the mode
    // under it would be the last thing it meant.
    const drifted = run([
      press(),
      { type: "move", x: 100 - TAP_SLOP_PX - 1, y: 100 },
      { type: "release", at: HELD_AT - 1 },
    ]);
    expect(drifted.state).toEqual(IDLE);
    expect(drifted.effects).toEqual([]);
    // Drift within the slop is still a tap: a thumb is never perfectly still.
    const steady = run([
      press(),
      { type: "move", x: 100 - TAP_SLOP_PX, y: 100 + TAP_SLOP_PX },
      { type: "release", at: HELD_AT - 1 },
    ]);
    expect(steady.effects).toEqual([{ type: "toggle-mode" }]);
  });

  it("a press already in the cancel target never opens the microphone", () => {
    const r = run([
      press(),
      { type: "move", x: 100 - CANCEL_SLIDE_PX, y: 100 },
      { type: "tick", at: HELD_AT },
    ]);
    expect(r.state).toEqual(IDLE);
    expect(r.effects).toEqual([]);
  });

  it("Esc during a press leaves nothing behind", () => {
    const r = run([press(), { type: "cancel" }]);
    expect(r.state).toEqual(IDLE);
    expect(r.effects).toEqual([]);
  });
});

describe("hold and release", () => {
  it("a held release sends", () => {
    const r = run([...hold(), { type: "release", at: HELD_AT + MIN_TAKE_MS + 1 }]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toEqual([
      { type: "start-capture", kind: "voice" },
      { type: "finish", disposition: "send" },
    ]);
  });

  it("a take shorter than the minimum is discarded with the hint and switches nothing", () => {
    // §3.1 - the accidental press. The microphone was already open, so this is
    // NOT re-read as a tap on the mode switch: the two are told apart by how
    // long the pointer was down, never by what the recorder produced.
    for (const pointer of ["mouse", "touch", "pen"] as const) {
      const r = run([...hold({ pointer }), { type: "release", at: HELD_AT + MIN_TAKE_MS - 1 }]);
      expect(r.state.phase).toBe("idle");
      expect(r.effects).toContainEqual({ type: "finish", disposition: "discard" });
      expect(r.effects).toContainEqual({ type: "hint", hint: "hold-to-record" });
      expect(r.effects).not.toContainEqual({ type: "toggle-mode" });
      expect(r.effects).not.toContainEqual({ type: "finish", disposition: "send" });
    }
  });

  it("exactly the minimum length is a real take", () => {
    const r = run([...hold(), { type: "release", at: HELD_AT + MIN_TAKE_MS }]);
    expect(r.effects).toContainEqual({ type: "finish", disposition: "send" });
  });

  it("keyboard activation starts a locked recording in one step", () => {
    const r = run([{ type: "activate", kind: "voice", at: 50, permission: "granted" }]);
    expect(r.state).toEqual({ phase: "locked", kind: "voice", startedAt: 50 });
    expect(r.effects).toEqual([{ type: "start-capture", kind: "voice" }]);
  });
});

describe("slide gestures", () => {
  it("sliding left past the threshold cancels", () => {
    const r = run([...hold(), { type: "move", x: 100 - CANCEL_SLIDE_PX, y: 100 }]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toContainEqual({ type: "finish", disposition: "discard" });
    expect(r.effects).toContainEqual({ type: "hint", hint: "cancelled" });
  });

  it("sliding left short of the threshold only tracks the offset", () => {
    const r = run([...hold(), { type: "move", x: 100 - CANCEL_SLIDE_PX + 1, y: 100 }]);
    expect(r.state).toMatchObject({ phase: "recording", dx: -CANCEL_SLIDE_PX + 1 });
    expect(r.effects).toEqual([{ type: "start-capture", kind: "voice" }]);
  });

  it("sliding up locks, and a release afterwards neither sends nor stops", () => {
    const r = run([
      ...hold(),
      { type: "move", x: 100, y: 100 - LOCK_SLIDE_PX },
      { type: "release", at: 5000 },
    ]);
    expect(r.state).toEqual({ phase: "locked", kind: "voice", startedAt: HELD_AT });
    expect(r.effects).toEqual([{ type: "start-capture", kind: "voice" }]);
  });

  it("locked Stop moves to preview, and Send from preview sends it", () => {
    const locked = run([...hold(), { type: "move", x: 100, y: 0 }]).state;
    const stopped = run([{ type: "stop", at: 3000 }], locked);
    expect(stopped.state).toEqual({ phase: "finalizing", kind: "voice" });
    expect(stopped.effects).toEqual([{ type: "finish", disposition: "preview" }]);
    const preview = run([{ type: "preview-ready", durationMs: 2000 }], stopped.state);
    expect(preview.state).toEqual({ phase: "preview", kind: "voice", durationMs: 2000 });
    expect(run([{ type: "send" }], preview.state).effects).toEqual([{ type: "send-preview" }]);
    expect(run([{ type: "delete" }], preview.state).effects).toEqual([{ type: "discard-preview" }]);
    expect(run([{ type: "cancel" }], preview.state).effects).toEqual([{ type: "discard-preview" }]);
  });

  it("locked Delete and Esc discard", () => {
    const locked: RecState = { phase: "locked", kind: "voice", startedAt: 0 };
    expect(run([{ type: "delete" }], locked).effects).toEqual([{ type: "finish", disposition: "discard" }]);
    expect(run([{ type: "cancel" }], locked).effects).toEqual([{ type: "finish", disposition: "discard" }]);
  });

  it("Esc while holding cancels", () => {
    const r = run([...hold(), { type: "cancel" }]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toContainEqual({ type: "finish", disposition: "discard" });
  });
});

describe("caps", () => {
  it("voice stops into preview at 15 minutes, not a millisecond before", () => {
    const recording = run(hold()).state;
    expect(run([{ type: "tick", at: HELD_AT + VOICE_CAP_MS - 1 }], recording).state.phase).toBe("recording");
    const capped = run([{ type: "tick", at: HELD_AT + VOICE_CAP_MS }], recording);
    expect(capped.state).toEqual({ phase: "finalizing", kind: "voice" });
    expect(capped.effects).toEqual([{ type: "finish", disposition: "preview" }]);
  });

  it("a video note stops at 60 seconds, including while locked", () => {
    const locked: RecState = { phase: "locked", kind: "video_note", startedAt: 0 };
    expect(run([{ type: "tick", at: VIDEO_NOTE_CAP_MS - 1 }], locked).state.phase).toBe("locked");
    expect(run([{ type: "tick", at: VIDEO_NOTE_CAP_MS }], locked).effects).toEqual([
      { type: "finish", disposition: "preview" },
    ]);
  });

  it("a locked Stop shorter than the minimum is discarded", () => {
    const locked: RecState = { phase: "locked", kind: "voice", startedAt: 0 };
    const r = run([{ type: "stop", at: MIN_TAKE_MS - 1 }], locked);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toContainEqual({ type: "finish", disposition: "discard" });
  });

  it("a finished take shorter than the minimum never reaches preview", () => {
    const r = run([{ type: "preview-ready", durationMs: MIN_TAKE_MS - 1 }], {
      phase: "finalizing",
      kind: "voice",
    });
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toContainEqual({ type: "discard-preview" });
  });
});

describe("permissions", () => {
  it("a known prompt: the hold that raised it does not record", () => {
    const r = run([...hold({ permission: "prompt" }), { type: "permission-result", granted: true, at: HELD_AT + 100 }]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toEqual([
      { type: "request-permission", kind: "voice" },
      { type: "release-stream" },
    ]);
  });

  it("an unknown permission granted fast while still held records", () => {
    // The inference budget runs from the REQUEST, not from the press: charging
    // the hold threshold against it would read a prompt-less grant as a prompt.
    const r = run([
      ...hold({ permission: "unknown" }),
      { type: "permission-result", granted: true, at: HELD_AT + PROMPT_INFERENCE_MS },
    ]);
    expect(r.state).toMatchObject({ phase: "recording", startedAt: HELD_AT + PROMPT_INFERENCE_MS });
    expect(r.effects).toContainEqual({ type: "start-capture", kind: "voice" });
  });

  it("an unknown permission granted slowly is treated as a prompt", () => {
    const r = run([
      ...hold({ permission: "unknown" }),
      { type: "permission-result", granted: true, at: HELD_AT + PROMPT_INFERENCE_MS + 1 },
    ]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).not.toContainEqual({ type: "start-capture", kind: "voice" });
  });

  it("a hold released before the grant arrives hints, and never switches the mode", () => {
    const r = run([
      ...hold({ permission: "unknown" }),
      { type: "release", at: HELD_AT + 10 },
      { type: "permission-result", granted: true, at: HELD_AT + 20 },
    ]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toEqual([
      { type: "request-permission", kind: "voice" },
      { type: "release-stream" },
      { type: "hint", hint: "hold-to-record" },
    ]);
  });

  it("denied shows the card when the press becomes a hold", () => {
    const r = run(hold({ permission: "denied" }));
    expect(r.state).toEqual(IDLE);
    expect(r.effects).toEqual([{ type: "show-denied", kind: "voice" }]);
  });

  it("refused at the prompt shows the card", () => {
    const r = run([
      ...hold({ permission: "prompt", kind: "video_note" }),
      { type: "permission-result", granted: false, at: HELD_AT + 200 },
    ]);
    expect(r.effects).toContainEqual({ type: "show-denied", kind: "video_note" });
  });

  it("revoked mid-recording discards the take and shows the card", () => {
    const r = run([...hold(), { type: "revoked" }]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toContainEqual({ type: "finish", disposition: "discard" });
    expect(r.effects).toContainEqual({ type: "show-denied", kind: "voice" });
    expect(r.effects).not.toContainEqual({ type: "finish", disposition: "send" });
  });

  it("a second press while a take is running is ignored", () => {
    const recording = run(hold()).state;
    const r = run([press({ at: 2000 })], recording);
    expect(r.state).toEqual(recording);
    expect(r.effects).toEqual([]);
  });
});

describe("formatting", () => {
  it("rounds a total duration up so a sub-second message never reads 0:00", () => {
    expect(formatTotalClock(0)).toBe("0:00");
    expect(formatTotalClock(-5)).toBe("0:00");
    expect(formatTotalClock(Number.NaN)).toBe("0:00");
    expect(formatTotalClock(1)).toBe("0:01");
    expect(formatTotalClock(480)).toBe("0:01");
    expect(formatTotalClock(999)).toBe("0:01");
    expect(formatTotalClock(1_000)).toBe("0:01");
    expect(formatTotalClock(1_001)).toBe("0:02");
    expect(formatTotalClock(59_500)).toBe("1:00");
    expect(formatTotalClock(VOICE_CAP_MS)).toBe("15:00");
    // The elapsed / position clock stays rounded down.
    expect(formatClock(480)).toBe("0:00");
  });

  it("formats the clock and the spoken duration", () => {
    expect(formatClock(7_400)).toBe("0:07");
    expect(formatClock(62_000)).toBe("1:02");
    expect(formatClock(VOICE_CAP_MS)).toBe("15:00");
    expect(spokenDuration(12_000)).toBe("12 seconds");
    expect(spokenDuration(1_000)).toBe("1 second");
    expect(spokenDuration(65_000)).toBe("1 minute 5 seconds");
    expect(spokenDuration(120_000)).toBe("2 minutes");
  });
});
