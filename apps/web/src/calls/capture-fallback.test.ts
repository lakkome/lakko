// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import {
  captureFailureReason,
  captureFallbackNotice,
  captureWithFallback,
} from "./capture-fallback";

const named = (name: string): Error => {
  const e = new Error(name);
  e.name = name;
  return e;
};

describe("captureWithFallback", () => {
  it("captures camera and microphone when both work", async () => {
    const calls: Array<[boolean, boolean]> = [];
    const out = await captureWithFallback({ audio: true, video: true }, async (a, v) => {
      calls.push([a, v]);
      return "stream";
    });
    expect(out).toEqual({ stream: "stream", fallback: null });
    expect(calls).toEqual([[true, true]]);
  });

  it("falls back to the microphone when the camera fails", async () => {
    const calls: Array<[boolean, boolean]> = [];
    const out = await captureWithFallback({ audio: true, video: true }, async (a, v) => {
      calls.push([a, v]);
      if (v) throw named("NotFoundError");
      return "mic";
    });
    expect(out).toEqual({ stream: "mic", fallback: { missing: "camera", reason: "not-found" } });
    expect(calls).toEqual([
      [true, true],
      [true, false],
    ]);
  });

  it("joins with nothing when the microphone fails too, keeping the last reason", async () => {
    let n = 0;
    const out = await captureWithFallback({ audio: true, video: true }, async () => {
      n += 1;
      throw n === 1 ? named("NotFoundError") : named("NotAllowedError");
    });
    expect(out).toEqual({
      stream: null,
      fallback: { missing: "camera-and-microphone", reason: "not-allowed" },
    });
  });

  it("an audio-only call that fails joins with nothing, once", async () => {
    let n = 0;
    const out = await captureWithFallback({ audio: true, video: false }, async () => {
      n += 1;
      throw named("OverconstrainedError");
    });
    expect(n).toBe(1);
    expect(out.stream).toBeNull();
    expect(out.fallback).toEqual({ missing: "camera-and-microphone", reason: "not-found" });
  });

  it("asks for nothing when nothing is wanted", async () => {
    let n = 0;
    const out = await captureWithFallback({ audio: false, video: false }, async () => {
      n += 1;
      return "x";
    });
    expect(n).toBe(0);
    expect(out).toEqual({ stream: null, fallback: null });
  });
});

describe("captureFailureReason", () => {
  it("maps the browser error names", () => {
    expect(captureFailureReason(named("NotFoundError"))).toBe("not-found");
    expect(captureFailureReason(named("OverconstrainedError"))).toBe("not-found");
    expect(captureFailureReason(named("NotAllowedError"))).toBe("not-allowed");
    expect(captureFailureReason(named("SecurityError"))).toBe("not-allowed");
    expect(captureFailureReason(named("NotReadableError"))).toBe("other");
    expect(captureFailureReason("junk")).toBe("other");
    expect(captureFailureReason(null)).toBe("other");
  });
});

describe("captureFallbackNotice", () => {
  it("never repeats the browser's words and always says what the person has", () => {
    for (const missing of ["camera", "camera-and-microphone"] as const) {
      for (const reason of ["not-found", "not-allowed", "other"] as const) {
        const line = captureFallbackNotice({ missing, reason });
        expect(line).not.toMatch(/constraint|denied|Error/);
        expect(line).toMatch(/You joined with your microphone|You can listen and watch/);
      }
    }
    expect(captureFallbackNotice({ missing: "camera", reason: "not-found" })).toBe(
      "No camera was found. You joined with your microphone.",
    );
  });
});
