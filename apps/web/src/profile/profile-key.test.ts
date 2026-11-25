// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { profileKeyFor } from "./profile-key";

const stamps =
  (held: Record<string, bigint>) =>
  (k: string): bigint | undefined =>
    held[k];

describe("profileKeyFor (ADR-0073 R.2, UX 16.3)", () => {
  it("a key with its own profile is its own answer", () => {
    expect(profileKeyFor("new", new Map([["old", "new"]]), stamps({ new: 2n, old: 1n }))).toBe("new");
  });

  it("the NEW key of a reset contact falls back to the profile their old key sent (2026-11-25)", () => {
    expect(profileKeyFor("new", new Map([["old", "new"]]), stamps({ old: 1n }))).toBe("old");
  });

  it("of several retired keys, the newest profile names the person", () => {
    const aliases = new Map([
      ["first", "new"],
      ["second", "new"],
    ]);
    expect(profileKeyFor("new", aliases, stamps({ first: 5n, second: 9n }))).toBe("second");
    expect(profileKeyFor("new", aliases, stamps({ first: 9n, second: 5n }))).toBe("first");
  });

  it("a retired key falls back to the current key, then to its sibling retired keys", () => {
    const aliases = new Map([
      ["first", "new"],
      ["second", "new"],
    ]);
    expect(profileKeyFor("first", aliases, stamps({ new: 1n, second: 9n }))).toBe("new");
    expect(profileKeyFor("first", aliases, stamps({ second: 9n }))).toBe("second");
  });

  it("never borrows another person's profile", () => {
    const aliases = new Map([
      ["old", "new"],
      ["bobOld", "bob"],
    ]);
    expect(profileKeyFor("new", aliases, stamps({ bobOld: 3n, bob: 4n }))).toBeUndefined();
    expect(profileKeyFor("stranger", aliases, stamps({ old: 1n }))).toBeUndefined();
  });
});
