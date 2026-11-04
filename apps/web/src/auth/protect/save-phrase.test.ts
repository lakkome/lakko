// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which way "Save a recovery phrase" goes (ADR-0075 section 3 as amended
 * 2026-11-01), and what the last call's answer means.
 */
import { describe, expect, it, vi } from "vitest";

import { AuthHttpError, AuthNetworkError } from "../client";
import {
  phraseOpensAccount,
  phraseSavedLeftNoNotice,
  phraseSavedVerdict,
  readSavePath,
} from "./save-phrase";

const PHRASE = "one two three";

describe("readSavePath", () => {
  it("rotation_needed keeps the rotation, and the held phrase is not even read", async () => {
    const heldPhrase = vi.fn(async () => PHRASE);
    const path = await readSavePath({
      status: async () => ({ saved: false, rotationNeeded: true }),
      heldPhrase,
    });
    expect(path).toEqual({ kind: "rotation" });
    expect(heldPhrase).not.toHaveBeenCalled();
  });

  it("no rotation needed and the device holds the phrase: its words", async () => {
    await expect(
      readSavePath({
        status: async () => ({ saved: false, rotationNeeded: false }),
        heldPhrase: async () => PHRASE,
      }),
    ).resolves.toEqual({ kind: "words", phrase: PHRASE });
  });

  it("a device that does not hold the phrase rotates", async () => {
    await expect(
      readSavePath({
        status: async () => ({ saved: false, rotationNeeded: false }),
        heldPhrase: async () => null,
      }),
    ).resolves.toEqual({ kind: "rotation" });
  });

  it("a failed read is a failure, never a guess", async () => {
    await expect(
      readSavePath({
        status: async () => {
          throw new Error("offline");
        },
        heldPhrase: async () => PHRASE,
      }),
    ).rejects.toThrow("offline");
  });
});

describe("phraseSavedVerdict", () => {
  const http = (status: number, error?: string): AuthHttpError =>
    new AuthHttpError(
      status,
      "/account/phrase/saved",
      error === undefined ? "" : JSON.stringify({ error }),
    );

  it("an answer saved it", () => {
    expect(phraseSavedVerdict(null)).toBe("saved");
  });

  it("409 phrase-save-needs-rotation hands over to the rotation", () => {
    expect(phraseSavedVerdict(http(409, "phrase-save-needs-rotation"))).toBe("needs-rotation");
  });

  it("another 409 is a refusal", () => {
    expect(phraseSavedVerdict(http(409, "something-else"))).toBe("refused");
  });

  it("the proof block answers a 403 and a limit", () => {
    expect(phraseSavedVerdict(http(403, "fresh-auth-required"))).toBe("proof");
    expect(phraseSavedVerdict(http(429, "too-many-attempts"))).toBe("proof");
  });

  it("no answer, or a server failure, may have landed", () => {
    expect(phraseSavedVerdict(new AuthNetworkError("/account/phrase/saved", new Error("x")))).toBe("unknown");
    expect(phraseSavedVerdict(http(503))).toBe("unknown");
  });

  it("only an answer that wrote nothing gives up the own-action mark", () => {
    expect(phraseSavedLeftNoNotice("needs-rotation")).toBe(true);
    expect(phraseSavedLeftNoNotice("proof")).toBe(true);
    expect(phraseSavedLeftNoNotice("refused")).toBe(true);
    expect(phraseSavedLeftNoNotice("unknown")).toBe(false);
    expect(phraseSavedLeftNoNotice("saved")).toBe(false);
  });
});

describe("phraseOpensAccount", () => {
  const wasmOf = (seed: Uint8Array) => ({
    RecoveryMnemonic: {
      fromPhrase: (phrase: string) => {
        if (phrase !== PHRASE) throw new Error("bad phrase");
        return { toSeed: () => seed.slice(), free: () => undefined };
      },
    },
  });
  const seed = new Uint8Array(64).fill(7);

  it("is true for the words of the account's seed", () => {
    expect(phraseOpensAccount(wasmOf(seed), PHRASE, seed)).toBe(true);
  });

  it("is false for another seed: words a rotation left behind are not shown", () => {
    expect(phraseOpensAccount(wasmOf(seed), PHRASE, new Uint8Array(64).fill(8))).toBe(false);
  });

  it("is false for a phrase that does not parse", () => {
    expect(phraseOpensAccount(wasmOf(seed), "nonsense", seed)).toBe(false);
  });
});
