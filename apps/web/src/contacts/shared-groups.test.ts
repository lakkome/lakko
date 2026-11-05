// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { sharedGroupsWith } from "./shared-groups";

interface G {
  readonly groupIdB64: string;
  readonly people: readonly string[];
  readonly oneToOne: boolean;
  readonly active: boolean;
  readonly at: number;
}

const run = (groups: G[], own: string | null = "me", person: string[] = ["anna"]) =>
  sharedGroupsWith({
    groups,
    personKeysB64: person,
    ownKeyB64: own,
    isOneToOne: (g) => g.oneToOne,
    isActive: (g) => g.active,
    peopleOf: (g) => g.people,
    newestMs: (g) => g.at,
  });

describe("sharedGroupsWith (who-can-find-me 6.4)", () => {
  it("lists the groups both are current members of, with their newest message time", () => {
    const walk: G = { groupIdB64: "walk", people: ["me", "anna", "boris"], oneToOne: false, active: true, at: 7 };
    expect(run([walk])).toEqual([{ groupIdB64: "walk", newestMs: 7 }]);
  });

  it("never a one-to-one, a group this device left, or a group the person is not in", () => {
    expect(run([{ groupIdB64: "a", people: ["me", "anna"], oneToOne: true, active: true, at: 1 }])).toEqual([]);
    expect(run([{ groupIdB64: "b", people: ["me", "anna", "c"], oneToOne: false, active: false, at: 1 }])).toEqual([]);
    expect(run([{ groupIdB64: "c", people: ["me", "boris"], oneToOne: false, active: true, at: 1 }])).toEqual([]);
  });

  it("finds the person under a key they rotated from; nothing before a session", () => {
    const g: G = { groupIdB64: "d", people: ["me", "anna-old", "c"], oneToOne: false, active: true, at: 1 };
    expect(run([g], "me", ["anna", "anna-old"])).toHaveLength(1);
    expect(run([g], null, ["anna", "anna-old"])).toEqual([]);
  });
});
