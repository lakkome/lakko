// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import {
  afterProvenRotation,
  afterReset,
  bindingAccepted,
  buildPredecessorTable,
  conversationKeyOf,
  currentKeyOf,
  samePerson,
  type AccountKeys,
} from "./predecessor";

const alice: AccountKeys = { currentKeyB64: "A1", provenPredecessorsB64: [] };

describe("the proven-predecessor rule (ADR-0074 section 3.5)", () => {
  it("a proven rotation makes the old key the same person", () => {
    const rotated = afterProvenRotation(alice, "A2");
    expect(rotated).toEqual({ currentKeyB64: "A2", provenPredecessorsB64: ["A1"] });
    const table = buildPredecessorTable([rotated, { currentKeyB64: "B1", provenPredecessorsB64: [] }]);
    expect(currentKeyOf("A1", table)).toBe("A2");
    expect(samePerson("A1", "A2", table)).toBe(true);
    expect(samePerson("A1", "B1", table)).toBe(false);
    // A leaf bound to the old key is still that account's device.
    expect(bindingAccepted("A1", rotated)).toBe(true);
    expect(bindingAccepted("A2", rotated)).toBe(true);
    expect(bindingAccepted("B1", rotated)).toBe(false);
  });

  it("a chain of rotations keeps every proven key, and the leaves stay under the first", () => {
    const twice = afterProvenRotation(afterProvenRotation(alice, "A2"), "A3");
    expect(twice.provenPredecessorsB64).toEqual(["A1", "A2"]);
    const table = buildPredecessorTable([twice]);
    expect(samePerson("A1", "A3", table)).toBe(true);
    expect(samePerson("A2", "A3", table)).toBe(true);
    expect(samePerson("A1", "A2", table)).toBe(true);
    expect(conversationKeyOf(twice)).toBe("A1");
    expect(conversationKeyOf(alice)).toBe("A1");
  });

  it("an unproven change never gets predecessor treatment", () => {
    const reset = afterReset(alice, "A9");
    expect(reset.provenPredecessorsB64).toEqual([]);
    const table = buildPredecessorTable([reset]);
    expect(samePerson("A1", "A9", table)).toBe(false);
    expect(bindingAccepted("A1", reset)).toBe(false);
  });

  it("a reset after a proven rotation ends every predecessor, proven or not", () => {
    const reset = afterReset(afterProvenRotation(alice, "A2"), "A9");
    const table = buildPredecessorTable([reset]);
    expect(samePerson("A1", "A9", table)).toBe(false);
    expect(samePerson("A2", "A9", table)).toBe(false);
    expect(conversationKeyOf(reset)).toBe("A9");
  });

  it("an unknown key names itself", () => {
    const table = buildPredecessorTable([afterProvenRotation(alice, "A2")]);
    expect(currentKeyOf("Z", table)).toBe("Z");
    expect(samePerson("Z", "Z", table)).toBe(true);
    expect(samePerson("Z", "A2", table)).toBe(false);
  });

  it("a current key is never read as someone's predecessor, and an ambiguous one names nobody", () => {
    const mallory: AccountKeys = { currentKeyB64: "M1", provenPredecessorsB64: ["A2", "X"] };
    const carol: AccountKeys = { currentKeyB64: "C1", provenPredecessorsB64: ["X"] };
    const table = buildPredecessorTable([afterProvenRotation(alice, "A2"), mallory, carol]);
    // A2 is Alice's current key: Mallory's claim on it is ignored.
    expect(currentKeyOf("A2", table)).toBe("A2");
    expect(samePerson("A2", "M1", table)).toBe(false);
    // X is claimed by two accounts: it resolves to neither.
    expect(currentKeyOf("X", table)).toBe("X");
  });

  it("re-observing the same rotation changes nothing", () => {
    const rotated = afterProvenRotation(alice, "A2");
    expect(afterProvenRotation(rotated, "A2")).toBe(rotated);
  });
});
