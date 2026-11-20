// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { shownVersion } from "./app-version";

describe("shownVersion", () => {
  it("shows a real version", () => {
    expect(shownVersion("0.1.0")).toBe("0.1.0");
    expect(shownVersion(" 1.2.3-beta ")).toBe("1.2.3-beta");
  });

  it("shows nothing for the placeholder or an empty value (walk 7: Version 0.0.0)", () => {
    expect(shownVersion("0.0.0")).toBeNull();
    expect(shownVersion("0")).toBeNull();
    expect(shownVersion("")).toBeNull();
    expect(shownVersion(undefined)).toBeNull();
    expect(shownVersion(null)).toBeNull();
  });
});
