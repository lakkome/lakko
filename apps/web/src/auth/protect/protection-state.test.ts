// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { MemoryStorage } from "../../test/fake-storage";
import { markSignupPhraseOwed } from "../phrase-gate";
import type { OwnSealFacts } from "../seal/own-seals";
import { isPhraseProtected, protectionLookedBefore, setPhraseProtected } from "./account-protection";
import { phraseFlagAfterSeals, syncPhraseFlagFromSeals } from "./protection-state";

const seals = (phrase: boolean): OwnSealFacts => ({ password: true, phrase, passkeyTagsHex: [], seq: "3" });

describe("the per-device 'phrase saved' flag follows the published protection", () => {
  it("unknown facts change nothing", () => {
    expect(phraseFlagAfterSeals({ facts: null, flag: true, newWordsOwed: false })).toBe(true);
    expect(phraseFlagAfterSeals({ facts: null, flag: false, newWordsOwed: false })).toBe(false);
  });

  it("a listed phrase sets it", () => {
    expect(phraseFlagAfterSeals({ facts: seals(true), flag: false, newWordsOwed: false })).toBe(true);
    expect(phraseFlagAfterSeals({ facts: seals(true), flag: false, newWordsOwed: true })).toBe(true);
  });

  it("no listed phrase clears it", () => {
    expect(phraseFlagAfterSeals({ facts: seals(false), flag: true, newWordsOwed: false })).toBe(false);
  });

  it("but not while new words are owed: the flag is what decides that they are shown", () => {
    // After a key reset or a rotation the new protection lists no phrase
    // yet; the words of an account that HAD saved one are still shown.
    expect(phraseFlagAfterSeals({ facts: seals(false), flag: true, newWordsOwed: true })).toBe(true);
    expect(phraseFlagAfterSeals({ facts: seals(false), flag: false, newWordsOwed: true })).toBe(false);
  });

  it("writes the flag and remembers that this device looked", () => {
    const local = new MemoryStorage();
    expect(syncPhraseFlagFromSeals("alice", seals(false), local)).toBe(false);
    expect(protectionLookedBefore(local, "alice")).toBe(true);
    expect(isPhraseProtected(local, "alice")).toBe(false);
    // Saved on another device since: told once, and the flag follows.
    expect(syncPhraseFlagFromSeals("alice", seals(true), local)).toBe(true);
    expect(isPhraseProtected(local, "alice")).toBe(true);
    // The next read tells nothing more.
    expect(syncPhraseFlagFromSeals("alice", seals(true), local)).toBe(false);
  });

  it("a device that never looked is new: it is told nothing about a phrase saved before it", () => {
    const local = new MemoryStorage();
    expect(syncPhraseFlagFromSeals("alice", seals(true), local)).toBe(false);
    expect(isPhraseProtected(local, "alice")).toBe(true);
  });

  it("a phrase saved on this device is not told back to it", () => {
    const local = new MemoryStorage();
    syncPhraseFlagFromSeals("alice", seals(false), local);
    // What `notePhraseSaved` does on the saving device, before the facts
    // are read again.
    setPhraseProtected(local, "alice", true);
    expect(syncPhraseFlagFromSeals("alice", seals(true), local)).toBe(false);
  });

  it("unknown facts neither write nor count as a look", () => {
    const local = new MemoryStorage();
    expect(syncPhraseFlagFromSeals("alice", null, local)).toBe(false);
    expect(protectionLookedBefore(local, "alice")).toBe(false);
    expect(local.entries()).toEqual([]);
  });

  it("keeps the flag through a reset's new protection while the new words are owed", () => {
    const local = new MemoryStorage();
    setPhraseProtected(local, "alice", true);
    markSignupPhraseOwed(local, "alice");
    syncPhraseFlagFromSeals("alice", seals(false), local);
    expect(isPhraseProtected(local, "alice")).toBe(true);
  });
});
