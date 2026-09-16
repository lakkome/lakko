// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { groupTitle, groupTitleFromNames, isUnnamedGroupLabel } from "./group-title";

describe("isUnnamedGroupLabel", () => {
  it("treats the form default and every store stand-in as unnamed", () => {
    expect(isUnnamedGroupLabel("New group")).toBe(true);
    expect(isUnnamedGroupLabel("Group aB3_-x")).toBe(true);
    expect(isUnnamedGroupLabel("Joined group (Welcome 42)")).toBe(true);
    expect(isUnnamedGroupLabel("Group")).toBe(true);
    expect(isUnnamedGroupLabel("Direct chat")).toBe(true);
    expect(isUnnamedGroupLabel("")).toBe(true);
  });

  it("keeps a real name", () => {
    expect(isUnnamedGroupLabel("Weekend plans")).toBe(false);
    expect(isUnnamedGroupLabel("Group therapy")).toBe(false);
  });
});

describe("groupTitleFromNames", () => {
  it("lists up to three names", () => {
    expect(groupTitleFromNames(["Anna"])).toBe("Anna");
    expect(groupTitleFromNames(["Anna", "Ivan", "Maria"])).toBe("Anna, Ivan, Maria");
  });

  it("counts the rest beyond three", () => {
    expect(groupTitleFromNames(["Anna", "Ivan", "Maria", "Olga", "Pavel"])).toBe(
      "Anna, Ivan, Maria +2",
    );
  });

  it("falls back to the default when nobody else is in the group", () => {
    expect(groupTitleFromNames([])).toBe("New group");
  });
});

describe("groupTitle", () => {
  it("prefers a real name", () => {
    expect(groupTitle("Weekend plans", ["Anna"])).toBe("Weekend plans");
  });

  it("never shows the reload stand-in", () => {
    expect(groupTitle("Group aB3_-x", ["Anna", "Ivan"])).toBe("Anna, Ivan");
  });
});
