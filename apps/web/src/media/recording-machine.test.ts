// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0047 / UX spec §2–§3, §6 — the hold-to-record gesture.
 *
 * Each case drives the pure reducer the way the controller does and asserts
 * both the resulting phase and the effects, because the effects are the
 * observable behaviour: `finish: send` is a message reaching the other side,
 * `finish: discard` is a take thrown away.
 */
import { describe, expect, it } from "vitest";

import {
  CANCEL_SLIDE_PX,
  IDLE,
  LOCK_SLIDE_PX,
  MIN_TAKE_MS,
  PROMPT_INFERENCE_MS,
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

const press = (over: Partial<Extract<RecEvent, { type: "press" }>> = {}): RecEvent => ({
  type: "press",
  kind: "voice",
  pointer: "touch",
  x: 100,
  y: 100,
  at: 1000,
  permission: "granted",
  ...over,
});

describe("hold and release", () => {
  it("a granted press records at once and a held release sends", () => {
    const r = run([press(), { type: "release", at: 1000 + MIN_TAKE_MS + 1 }]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toEqual([
      { type: "start-capture", kind: "voice" },
      { type: "finish", disposition: "send" },
    ]);
  });

  it("a touch take shorter than the minimum is discarded with the hint and toggles the mode", () => {
    const r = run([press(), { type: "release", at: 1000 + MIN_TAKE_MS - 1 }]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toContainEqual({ type: "finish", disposition: "discard" });
    expect(r.effects).toContainEqual({ type: "hint", hint: "hold-to-record" });
    expect(r.effects).toContainEqual({ type: "toggle-mode" });
    expect(r.effects).not.toContainEqual({ type: "finish", disposition: "send" });
  });

  it("exactly the minimum length is a real take", () => {
    const r = run([press(), { type: "release", at: 1000 + MIN_TAKE_MS }]);
    expect(r.effects).toContainEqual({ type: "finish", disposition: "send" });
  });

  it("a short MOUSE press is a click and continues as a locked recording", () => {
    const r = run([press({ pointer: "mouse" }), { type: "release", at: 1100 }]);
    expect(r.state).toEqual({ phase: "locked", kind: "voice", startedAt: 1000 });
    // No second capture start and nothing thrown away: the same take goes on.
    expect(r.effects).toEqual([{ type: "start-capture", kind: "voice" }]);
  });

  it("keyboard activation starts a locked recording in one step", () => {
    const r = run([{ type: "activate", kind: "voice", at: 50, permission: "granted" }]);
    expect(r.state).toEqual({ phase: "locked", kind: "voice", startedAt: 50 });
    expect(r.effects).toEqual([{ type: "start-capture", kind: "voice" }]);
  });
});

describe("slide gestures", () => {
  it("sliding left past the threshold cancels", () => {
    const r = run([press(), { type: "move", x: 100 - CANCEL_SLIDE_PX, y: 100 }]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toContainEqual({ type: "finish", disposition: "discard" });
    expect(r.effects).toContainEqual({ type: "hint", hint: "cancelled" });
  });

  it("sliding left short of the threshold only tracks the offset", () => {
    const r = run([press(), { type: "move", x: 100 - CANCEL_SLIDE_PX + 1, y: 100 }]);
    expect(r.state).toMatchObject({ phase: "recording", dx: -CANCEL_SLIDE_PX + 1 });
    expect(r.effects).toEqual([{ type: "start-capture", kind: "voice" }]);
  });

  it("sliding up locks, and a release afterwards neither sends nor stops", () => {
    const r = run([
      press(),
      { type: "move", x: 100, y: 100 - LOCK_SLIDE_PX },
      { type: "release", at: 5000 },
    ]);
    expect(r.state).toEqual({ phase: "locked", kind: "voice", startedAt: 1000 });
    expect(r.effects).toEqual([{ type: "start-capture", kind: "voice" }]);
  });

  it("locked Stop moves to preview, and Send from preview sends it", () => {
    const locked = run([press(), { type: "move", x: 100, y: 0 }]).state;
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
    const r = run([press(), { type: "cancel" }]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toContainEqual({ type: "finish", disposition: "discard" });
  });
});

describe("caps", () => {
  it("voice stops into preview at 15 minutes, not a millisecond before", () => {
    const recording = run([press()]).state;
    expect(run([{ type: "tick", at: 1000 + VOICE_CAP_MS - 1 }], recording).state.phase).toBe("recording");
    const capped = run([{ type: "tick", at: 1000 + VOICE_CAP_MS }], recording);
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
    const r = run([
      press({ permission: "prompt" }),
      { type: "permission-result", granted: true, at: 1100 },
    ]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toEqual([
      { type: "request-permission", kind: "voice" },
      { type: "release-stream" },
    ]);
  });

  it("an unknown permission granted fast while still held records", () => {
    const r = run([
      press({ permission: "unknown" }),
      { type: "permission-result", granted: true, at: 1000 + PROMPT_INFERENCE_MS },
    ]);
    expect(r.state).toMatchObject({ phase: "recording", startedAt: 1000 + PROMPT_INFERENCE_MS });
    expect(r.effects).toContainEqual({ type: "start-capture", kind: "voice" });
  });

  it("an unknown permission granted slowly is treated as a prompt", () => {
    const r = run([
      press({ permission: "unknown" }),
      { type: "permission-result", granted: true, at: 1000 + PROMPT_INFERENCE_MS + 1 },
    ]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).not.toContainEqual({ type: "start-capture", kind: "voice" });
  });

  it("denied up front shows the card without asking", () => {
    const r = run([press({ permission: "denied" })]);
    expect(r.effects).toEqual([{ type: "show-denied", kind: "voice" }]);
  });

  it("refused at the prompt shows the card", () => {
    const r = run([
      press({ permission: "prompt", kind: "video_note" }),
      { type: "permission-result", granted: false, at: 1200 },
    ]);
    expect(r.effects).toContainEqual({ type: "show-denied", kind: "video_note" });
  });

  it("revoked mid-recording discards the take and shows the card", () => {
    const r = run([press(), { type: "revoked" }]);
    expect(r.state.phase).toBe("idle");
    expect(r.effects).toContainEqual({ type: "finish", disposition: "discard" });
    expect(r.effects).toContainEqual({ type: "show-denied", kind: "voice" });
    expect(r.effects).not.toContainEqual({ type: "finish", disposition: "send" });
  });

  it("a second press while a take is running is ignored", () => {
    const recording = run([press()]).state;
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
