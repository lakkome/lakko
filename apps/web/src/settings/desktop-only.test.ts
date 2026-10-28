// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { sectionsForBuild, showsShortcutList } from "./desktop-only";

const all = [{ id: "security" }, { id: "notifications" }, { id: "folders" }];

describe("desktop-only Settings", () => {
  it("drops Notifications from the browser build", () => {
    expect(sectionsForBuild(all, false).map((s) => s.id)).toEqual(["security", "folders"]);
  });
  it("keeps it in the desktop app, in place", () => {
    expect(sectionsForBuild(all, true).map((s) => s.id)).toEqual([
      "security",
      "notifications",
      "folders",
    ]);
  });
  it("lists the shortcuts on the desktop only", () => {
    expect(showsShortcutList(true)).toBe(true);
    expect(showsShortcutList(false)).toBe(false);
  });
});
