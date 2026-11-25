// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { appHeightFor } from "./keyboard-viewport";

describe("appHeightFor", () => {
  it("is the visible height above the keyboard", () => {
    expect(appHeightFor({ height: 412.4, scale: 1 })).toBe(412);
  });

  it("leaves a pinch-zoomed page at full height", () => {
    expect(appHeightFor({ height: 300, scale: 1.6 })).toBeNull();
  });

  it("ignores a viewport with no height", () => {
    expect(appHeightFor({ height: 0, scale: 1 })).toBeNull();
    expect(appHeightFor({ height: Number.NaN, scale: 1 })).toBeNull();
  });
});
