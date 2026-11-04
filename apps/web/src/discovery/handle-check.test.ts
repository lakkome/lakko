// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** `discovery/handle-check.ts` — an `@name` field asks discovery as seldom as
 *  search does, Create reuses the check's OPRF answer, and a rate limit is
 *  recognised as such. */
import { beforeEach, describe, expect, it, vi } from "vitest";

const oprfForName = vi.fn();
const lookupRaw = vi.fn();

vi.mock("./handle-lookup", () => {
  class HandleWriteError extends Error {
    constructor(
      public readonly status: number,
      public readonly code: string | null,
    ) {
      super("refused");
    }
  }
  return { oprfForName: (n: string) => oprfForName(n), HandleWriteError };
});
vi.mock("./client", () => {
  class DiscoveryHttpError extends Error {
    constructor(
      public readonly status: number,
      public readonly endpoint: string,
      public readonly body: string,
    ) {
      super("http");
    }
  }
  class DiscoveryRateLimitedError extends Error {}
  class DiscoveryClient {
    lookupRaw(req: unknown) {
      return lookupRaw(req);
    }
  }
  return { DiscoveryClient, DiscoveryHttpError, DiscoveryRateLimitedError };
});
vi.mock("../reach/publication-model", () => ({ TERM_RATE_LIMIT_WAIT_MS: 8000 }));

import { DiscoveryHttpError } from "./client";
import { handleTaken, isRateLimited, namePair, resetHandleChecks, termPairsInBatch } from "./handle-check";

const pair = (n: string) => ({ output: new Uint8Array([1]), lookupKey: new Uint8Array([2]), normalized: n });

beforeEach(() => {
  resetHandleChecks();
  oprfForName.mockReset();
  lookupRaw.mockReset();
  oprfForName.mockImplementation(async (n: string) => pair(n));
});

describe("the taken check", () => {
  it("asks once for one name within 60 s, and again after", async () => {
    lookupRaw.mockResolvedValue({ kind: "handle" });
    let now = 1_000;
    expect(await handleTaken("qa_second_channel", () => now)).toBe(true);
    now += 59_000;
    expect(await handleTaken("@QA_second_channel", () => now)).toBe(true);
    expect(lookupRaw).toHaveBeenCalledTimes(1);
    now += 2_000;
    await handleTaken("qa_second_channel", () => now);
    expect(lookupRaw).toHaveBeenCalledTimes(2);
    // The OPRF round is never repeated for the name.
    expect(oprfForName).toHaveBeenCalledTimes(1);
  });

  it("says nothing when discovery answers nothing usable", async () => {
    lookupRaw.mockRejectedValue(new DiscoveryHttpError(429, "/discovery/lookup", ""));
    expect(await handleTaken("qa_second_channel")).toBeNull();
  });
});

describe("the claim reuses the check's answer", () => {
  it("one OPRF round per name for check and Create", async () => {
    lookupRaw.mockResolvedValue(null);
    await handleTaken("qa_second_channel");
    await namePair("qa_second_channel");
    expect(oprfForName).toHaveBeenCalledTimes(1);
  });

  it("does not keep a failure", async () => {
    const wait = vi.fn(async () => undefined);
    const askBatch = vi.fn(async () => {
      throw new Error("offline");
    });
    await expect(termPairsInBatch(["cha"], askBatch, wait)).rejects.toThrow("offline");
    await expect(termPairsInBatch(["cha"], askBatch, wait)).rejects.toThrow("offline");
    expect(askBatch).toHaveBeenCalledTimes(2);
  });
});

describe("a record's own words go in one batch", () => {
  it("asks the missing words once, in order, and reuses the kept ones", async () => {
    const out = (w: string) => ({ output: new TextEncoder().encode(w), lookupKey: new Uint8Array([0]) });
    const askBatch = vi.fn(async (missing: readonly string[]) => missing.map(out));
    const first = await termPairsInBatch(["sec", "seco", "cha"], askBatch);
    expect(askBatch).toHaveBeenCalledTimes(1);
    expect(askBatch.mock.calls[0]?.[0]).toEqual(["sec", "seco", "cha"]);
    expect(first.map((p) => new TextDecoder().decode(p.output))).toEqual(["sec", "seco", "cha"]);
    await termPairsInBatch(["cha", "chan"], askBatch);
    expect(askBatch).toHaveBeenCalledTimes(2);
    expect(askBatch.mock.calls[1]?.[0]).toEqual(["chan"]);
  });

  it("asks the batch once more after a rate limit", async () => {
    const wait = vi.fn(async () => undefined);
    let calls = 0;
    const askBatch = vi.fn(async (missing: readonly string[]) => {
      calls += 1;
      if (calls === 1) throw new DiscoveryHttpError(429, "/discovery/oprf/batch", "");
      return missing.map(() => ({ output: new Uint8Array([1]), lookupKey: new Uint8Array([2]) }));
    });
    await termPairsInBatch(["qa1"], askBatch, wait);
    expect(askBatch).toHaveBeenCalledTimes(2);
    expect(wait).toHaveBeenCalledTimes(1);
  });
});

describe("isRateLimited", () => {
  it("is a discovery 429 and nothing else", () => {
    expect(isRateLimited(new DiscoveryHttpError(429, "/discovery/oprf", ""))).toBe(true);
    expect(isRateLimited(new DiscoveryHttpError(409, "/discovery/handle/claim", ""))).toBe(false);
    expect(isRateLimited(new Error("x"))).toBe(false);
  });
});
