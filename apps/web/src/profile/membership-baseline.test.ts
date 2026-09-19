// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 trigger 3 — the growth baseline persisted across page sessions.
 *
 * The defect this pins: the chat view kept its previous roster in a variable,
 * so a join that landed while the tab was closed was a baseline to the next
 * session rather than growth, and the newcomer never got this device's name.
 * "grows across two independent reads" below is that case — the second call
 * models a fresh session that reads the store cold.
 */
import { describe, expect, it } from "vitest";

import { loadMemberBaseline, observeMembers } from "./membership-baseline";
import type { KeyValueStorage } from "./profile-request";

class MemoryStorage implements KeyValueStorage {
  readonly map = new Map<string, string>();
  writes = 0;
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.writes += 1;
    this.map.set(k, v);
  }
}

const set = (...keys: string[]): Set<string> => new Set(keys);

describe("observeMembers", () => {
  it("the first observation is a baseline, not growth", () => {
    const s = new MemoryStorage();
    expect(observeMembers("acct", "g", set("a", "b"), s)).toBe(false);
    expect(loadMemberBaseline("acct", "g", s)).toEqual(set("a", "b"));
  });

  it("grows across two independent reads of the store", () => {
    const s = new MemoryStorage();
    observeMembers("acct", "g", set("a", "b"), s);
    // A new session: nothing in memory, only the store.
    expect(observeMembers("acct", "g", set("a", "b", "c"), s)).toBe(true);
    expect(loadMemberBaseline("acct", "g", s)).toEqual(set("a", "b", "c"));
  });

  it("a remove and an add in one commit is still growth", () => {
    const s = new MemoryStorage();
    observeMembers("acct", "g", set("a", "b"), s);
    expect(observeMembers("acct", "g", set("a", "c"), s)).toBe(true);
  });

  it("a shrink moves the baseline but is not growth", () => {
    const s = new MemoryStorage();
    observeMembers("acct", "g", set("a", "b"), s);
    expect(observeMembers("acct", "g", set("a"), s)).toBe(false);
    // Whoever is added next must read as a newcomer against the SHRUNK set.
    expect(observeMembers("acct", "g", set("a", "b"), s)).toBe(true);
  });

  it("an unchanged roster writes nothing", () => {
    const s = new MemoryStorage();
    observeMembers("acct", "g", set("a", "b"), s);
    const writes = s.writes;
    expect(observeMembers("acct", "g", set("b", "a"), s)).toBe(false);
    expect(s.writes).toBe(writes);
  });

  it("a torn read leaves the baseline alone", () => {
    const s = new MemoryStorage();
    observeMembers("acct", "g", set("a", "b"), s);
    expect(observeMembers("acct", "g", set(), s)).toBe(false);
    expect(loadMemberBaseline("acct", "g", s)).toEqual(set("a", "b"));
  });

  it("baselines are per (account, group)", () => {
    const s = new MemoryStorage();
    observeMembers("acct", "g1", set("a"), s);
    expect(observeMembers("acct", "g2", set("a", "b"), s)).toBe(false);
    expect(observeMembers("other", "g1", set("a", "b"), s)).toBe(false);
  });

  it("a corrupt row reads as no baseline and is re-seeded", () => {
    const s = new MemoryStorage();
    s.setItem("lakko.member-baseline.v1.acct.g", "{not json");
    expect(loadMemberBaseline("acct", "g", s)).toBeNull();
    expect(observeMembers("acct", "g", set("a"), s)).toBe(false);
    expect(loadMemberBaseline("acct", "g", s)).toEqual(set("a"));
  });

  it("survives an absent storage", () => {
    expect(observeMembers("acct", "g", set("a"), undefined)).toBe(false);
    expect(loadMemberBaseline("acct", "g", undefined)).toBeNull();
  });
});
