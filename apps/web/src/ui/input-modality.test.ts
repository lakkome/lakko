// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { nextPointerInput, POINTER_INPUT_ATTR } from "./input-modality";

describe("input modality", () => {
  it("a pointer press sets it, a key press clears it, anything else keeps it", () => {
    expect(nextPointerInput("pointerdown", false)).toBe(true);
    expect(nextPointerInput("mousedown", false)).toBe(true);
    expect(nextPointerInput("touchstart", false)).toBe(true);
    expect(nextPointerInput("keydown", true)).toBe(false);
    expect(nextPointerInput("focus", true)).toBe(true);
    expect(nextPointerInput("focus", false)).toBe(false);
  });

  it("every bubble ring is suppressed after a pointer press", () => {
    const css = readFileSync(new URL("../styles.css", import.meta.url), "utf8");
    for (const bubble of [".lk-bubble", ".lk-call-chat-bubble", ".lk-channel-post-bubble"]) {
      expect(css).toContain(`html[${POINTER_INPUT_ATTR}] ${bubble}:focus-visible`);
    }
  });
});
