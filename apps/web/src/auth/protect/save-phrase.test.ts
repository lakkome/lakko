// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";

import { phraseOpensAccount, savePhraseOutcome, wordsToSave, type PhraseWasm } from "./save-phrase";

const read = (rel: string): string => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

/** A core whose "seed" of a phrase is its bytes. */
const wasm: PhraseWasm = {
  RecoveryMnemonic: {
    fromPhrase(phrase: string) {
      if (phrase === "not a phrase") throw new Error("invalid");
      return { toSeed: () => new TextEncoder().encode(phrase), free: () => undefined };
    },
  },
};
const seedOf = (phrase: string): Uint8Array => new TextEncoder().encode(phrase);

describe("the words a device shows (UX 19.5, amended 2026-11-26)", () => {
  it("are the phrase it holds when that phrase opens the account", async () => {
    expect(
      await wordsToSave({ heldPhrase: async () => "alpha beta", opensAccount: async () => true }),
    ).toBe("alpha beta");
  });

  it("are none when the device holds no phrase: nothing can be saved here", async () => {
    const opens = vi.fn(async () => true);
    expect(await wordsToSave({ heldPhrase: async () => null, opensAccount: opens })).toBeNull();
    expect(opens).not.toHaveBeenCalled();
  });

  it("are none when the held phrase does not open the account as it is now", async () => {
    expect(
      await wordsToSave({ heldPhrase: async () => "old words", opensAccount: async () => false }),
    ).toBeNull();
  });

  it("a phrase opens the account only when it derives the account's seed", () => {
    expect(phraseOpensAccount(wasm, "alpha beta", seedOf("alpha beta"))).toBe(true);
    expect(phraseOpensAccount(wasm, "alpha beta", seedOf("alpha beti"))).toBe(false);
    expect(phraseOpensAccount(wasm, "alpha beta", seedOf("alpha"))).toBe(false);
    expect(phraseOpensAccount(wasm, "not a phrase", seedOf("not a phrase"))).toBe(false);
  });
});

describe("what counts as saved", () => {
  it("only a published protection: a failed publish saved nothing", () => {
    expect(savePhraseOutcome("published")).toBe("saved");
    expect(savePhraseOutcome("retry")).toBe("failed");
  });
});

describe("the panel", () => {
  const panel = read("./SavePhrasePanel.tsx");

  it("changes no key and tells no server that a phrase was saved", () => {
    expect(panel.includes("useKeyRotation")).toBe(false);
    expect(panel.includes("rotation.")).toBe(false);
    expect(panel.includes("markPhraseSaved")).toBe(false);
    expect(panel.includes("RecoveryEmailClient")).toBe(false);
  });

  it("publishes only after the three-word check, and marks the phrase saved only on its success", () => {
    const confirmed = panel.slice(panel.indexOf("async function onWordsConfirmed()"));
    const body = confirmed.slice(0, confirmed.indexOf("\n  }\n"));
    const publish = body.indexOf("await auth.savePhraseSeal()");
    const failed = body.indexOf("setSaveFailed(true);", publish);
    const noted = body.indexOf("notePhraseSaved(acct);");
    const closed = body.indexOf("props.onSaved();");
    expect(publish).toBeGreaterThan(0);
    expect(failed).toBeGreaterThan(publish);
    expect(noted).toBeGreaterThan(failed);
    expect(closed).toBeGreaterThan(noted);
    // The check form is what calls it.
    expect(panel.includes("onConfirmed={() => void onWordsConfirmed()}")).toBe(true);
  });

  it("a failed publish stays on the three-word screen with the line and Try again", () => {
    expect(panel.includes('submitLabel={saveFailed() ? t("common.try_again") : t("common.done")}')).toBe(true);
    expect(panel.includes('failure={saveFailed() ? t("protect.phrase.save.failed") : null}')).toBe(true);
  });

  it("shows the words only behind the proof, and no progress line of a key change", () => {
    const show = panel.slice(panel.indexOf("async function onShowWords()"));
    const body = show.slice(0, show.indexOf("\n  }\n"));
    expect(body.indexOf("await gate.ensure()")).toBeLessThan(body.indexOf("setPhrase(words);"));
    expect(panel.includes("protect.phrase.save.effect")).toBe(false);
    expect(panel.includes("protect.phrase.save.progress")).toBe(false);
    expect(panel.includes("protect.phrase.save.finishing")).toBe(false);
  });
});
