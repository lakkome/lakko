// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import {
  videoNoteAsideFirst,
  videoNoteDurationLabel,
  videoNotePillCornerInside,
} from "./video-note-layout";

describe("video note layout (media-messages section 3, 2026-11-22)", () => {
  it("keeps both pills inside the circle at every diameter", () => {
    for (const d of [96, 160, 200, 240, 320]) {
      expect(videoNotePillCornerInside(d)).toBe(true);
    }
  });

  it("puts Aa on the side away from the screen edge", () => {
    expect(videoNoteAsideFirst(true)).toBe(false);
    expect(videoNoteAsideFirst(false)).toBe(true);
  });

  it("shows the total until the note plays with sound, then the elapsed time", () => {
    expect(videoNoteDurationLabel({ mode: "poster", soundHint: false, positionMs: 0, durationMs: 4200 })).toEqual({
      kind: "total",
      ms: 4200,
    });
    expect(videoNoteDurationLabel({ mode: "muted", soundHint: false, positionMs: 1500, durationMs: 4200 })).toEqual({
      kind: "total",
      ms: 4200,
    });
    expect(videoNoteDurationLabel({ mode: "sound", soundHint: false, positionMs: 1500, durationMs: 4200 })).toEqual({
      kind: "elapsed",
      ms: 1500,
    });
  });

  it("says Tap for sound on a muted loop refused sound, and nothing without a duration", () => {
    expect(videoNoteDurationLabel({ mode: "muted", soundHint: true, positionMs: 0, durationMs: 4200 })).toEqual({
      kind: "sound-hint",
    });
    expect(videoNoteDurationLabel({ mode: "poster", soundHint: false, positionMs: 0, durationMs: undefined })).toBeNull();
    expect(videoNoteDurationLabel({ mode: "sound", soundHint: false, positionMs: -5, durationMs: undefined })).toEqual({
      kind: "elapsed",
      ms: 0,
    });
  });
});
