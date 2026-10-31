// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Every OPRF evaluation costs an anonymous token. Outputs kept only at the end
 * were lost to the first failure, so each retry paid for every term again and
 * the background publication ran through the account's daily quota.
 */
import { describe, expect, it, vi } from "vitest";

import {
  EMPTY_PUBLICATION,
  evaluateMissingTerms,
  TERM_RATE_LIMIT_WAIT_MS,
  type PublicationState,
  type TermRound,
} from "./publication-model";

const LIMITED = new Error("429");
const deps = (keep: (o: Readonly<Record<string, string>>) => void) => ({
  keep: async (o: Readonly<Record<string, string>>) => keep(o),
  isRateLimited: (e: unknown) => e === LIMITED,
  wait: vi.fn(async () => {}),
});
const round = (key: string): TermRound => ({ key, evaluate: async () => `out:${key}` });

describe("evaluateMissingTerms", () => {
  it("keeps what was paid for when a later term fails", async () => {
    let kept: Readonly<Record<string, string>> = {};
    const terms = [round("a"), round("b"), { key: "c", evaluate: () => Promise.reject(new Error("offline")) }];
    await expect(evaluateMissingTerms(EMPTY_PUBLICATION, terms, deps((o) => (kept = o)))).rejects.toThrow("offline");
    expect(kept).toEqual({ a: "out:a", b: "out:b" });
  });

  it("pays only for the terms it does not hold", async () => {
    const state: PublicationState = { ...EMPTY_PUBLICATION, termOutputs: { a: "out:a" } };
    const first = vi.fn(async () => "new");
    let kept: Readonly<Record<string, string>> = {};
    await evaluateMissingTerms(state, [{ key: "a", evaluate: first }, round("b")], deps((o) => (kept = o)));
    expect(first).not.toHaveBeenCalled();
    expect(kept).toEqual({ a: "out:a", b: "out:b" });
  });

  it("waits out a rate limit and asks once more", async () => {
    let calls = 0;
    const d = deps(() => {});
    const term: TermRound = {
      key: "a",
      evaluate: async () => {
        calls += 1;
        if (calls === 1) throw LIMITED;
        return "out:a";
      },
    };
    await evaluateMissingTerms(EMPTY_PUBLICATION, [term], d);
    expect(calls).toBe(2);
    expect(d.wait).toHaveBeenCalledWith(TERM_RATE_LIMIT_WAIT_MS);
  });

  it("asks a repeated term once", async () => {
    const evaluate = vi.fn(async () => "out:a");
    await evaluateMissingTerms(EMPTY_PUBLICATION, [{ key: "a", evaluate }, { key: "a", evaluate }], deps(() => {}));
    expect(evaluate).toHaveBeenCalledTimes(1);
  });
});
