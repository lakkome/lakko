// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Save a recovery phrase" (docs/ux/identity-recovery.md 19.5 as amended
 * 2026-11-26) as wiring: no key changes, the words the device holds are
 * shown only after the proof, and the phrase counts as saved only when the
 * account's protection was published with it (`auth.savePhraseSeal()`).
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const WORDS = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima";

const auth = {
  state: vi.fn(() => ({ kind: "session" as const, account: { accountId: "acct", username: "alice" } })),
  identitySecrets: vi.fn(() => ({ sessionToken: "token", accountIdentitySeed: new Uint8Array(64) })),
  savePhraseSeal: vi.fn(async (): Promise<"published" | "retry"> => "published"),
};
vi.mock("../AuthContext", () => ({ useAuth: () => auth }));

const device = { phrase: WORDS as string | null, opens: true };
vi.mock("../../backup/device-phrase", () => ({ readDevicePhrase: async () => device.phrase }));
vi.mock("../../wasm", () => ({
  loadWasm: async () => ({
    RecoveryMnemonic: {
      // The account's seed in this test is 64 zero bytes: a phrase that
      // opens it derives the same, one that does not derives ones.
      fromPhrase: () => ({
        toSeed: () => new Uint8Array(64).fill(device.opens ? 0 : 1),
        free: () => undefined,
      }),
    },
  }),
}));

const gate = {
  ensure: vi.fn(async (): Promise<string | null> => "grant"),
  failed: vi.fn(() => ({ kind: "other" })),
  clear: vi.fn(),
  view: () => ({ visible: false, ready: true }),
  wait: { waiting: () => false, remainingMs: () => 0 },
};
vi.mock("../proof-gate", () => ({ createProofGate: () => gate }));
vi.mock("../ProofBlock", () => ({ ProofBlock: () => null }));
vi.mock("../recover/FormNotices", () => ({ WaitNotice: () => null }));

const protection = { saved: vi.fn() };
vi.mock("./protection-state", () => ({
  notePhraseSaved: (a: string) => protection.saved(a),
}));

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { all, installLocalStorage, one, present, renderUi, settle } from "../../test/ui";
import { SavePhrasePanel } from "./SavePhrasePanel";

const FAILED = "We could not finish. Nothing changed.";
const text = (): string => document.body.textContent ?? "";
const button = (label: string): HTMLButtonElement | undefined =>
  all<HTMLButtonElement>("button").find((b) => b.textContent?.trim() === label);

/** Press through to the three-word form, type what it asks and submit. */
async function reachCheckAndSubmit(): Promise<void> {
  button("Show my 12 words")?.click();
  await settle();
  button("I have written them down")?.click();
  await settle();
  const list = WORDS.split(" ");
  for (const input of all<HTMLInputElement>('input:not([type="checkbox"])')) {
    const position = Number(/(\d+)/.exec(input.getAttribute("aria-label") ?? "")?.[1]);
    input.value = list[position - 1] ?? "";
    input.dispatchEvent(new Event("input", { bubbles: true }));
  }
  one<HTMLInputElement>('input[type="checkbox"]').click();
  await settle();
  one<HTMLFormElement>("form").dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  await settle();
}

beforeEach(() => {
  installLocalStorage();
  vi.clearAllMocks();
  device.phrase = WORDS;
  device.opens = true;
  auth.savePhraseSeal.mockResolvedValue("published");
  gate.ensure.mockResolvedValue("grant");
});

describe("the words the device holds", () => {
  it("are shown after the proof, with no paragraph about new keys", async () => {
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} />);
    await settle();
    expect(text()).not.toContain("new keys");
    expect(text()).not.toContain("foxtrot");
    button("Show my 12 words")?.click();
    await settle();
    expect(gate.ensure).toHaveBeenCalledTimes(1);
    expect(text()).toContain("foxtrot");
    // Showing the words publishes nothing.
    expect(auth.savePhraseSeal).not.toHaveBeenCalled();
  });

  it("no proof, no words", async () => {
    gate.ensure.mockResolvedValue(null);
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} />);
    await settle();
    button("Show my 12 words")?.click();
    await settle();
    expect(text()).not.toContain("foxtrot");
    expect(text()).not.toContain(FAILED);
  });

  it("a device that holds no phrase shows the failure and Try again, never words", async () => {
    device.phrase = null;
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} />);
    await settle();
    button("Show my 12 words")?.click();
    await settle();
    expect(text()).toContain(FAILED);
    expect(present(".lk-phrase-cols")).toBe(false);
    expect(button("Try again")).toBeDefined();
  });

  it("words that do not open the account as it is now are never shown", async () => {
    device.opens = false;
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} />);
    await settle();
    button("Show my 12 words")?.click();
    await settle();
    expect(text()).not.toContain("foxtrot");
    expect(text()).toContain(FAILED);
  });
});

describe("saved", () => {
  it("only when the protection was published with the phrase in it", async () => {
    const onSaved = vi.fn();
    renderUi(() => <SavePhrasePanel onSaved={onSaved} />);
    await settle();
    await reachCheckAndSubmit();
    expect(auth.savePhraseSeal).toHaveBeenCalledTimes(1);
    expect(protection.saved).toHaveBeenCalledWith("acct");
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("a person who leaves before the three words saved nothing", async () => {
    const onSaved = vi.fn();
    renderUi(() => <SavePhrasePanel onSaved={onSaved} />);
    await settle();
    button("Show my 12 words")?.click();
    await settle();
    button("I have written them down")?.click();
    await settle();
    expect(auth.savePhraseSeal).not.toHaveBeenCalled();
    expect(protection.saved).not.toHaveBeenCalled();
    expect(onSaved).not.toHaveBeenCalled();
  });

  it("a failed publish stays on the check with the line and Try again, and saves nothing", async () => {
    const onSaved = vi.fn();
    auth.savePhraseSeal.mockResolvedValueOnce("retry");
    renderUi(() => <SavePhrasePanel onSaved={onSaved} />);
    await settle();
    await reachCheckAndSubmit();
    expect(text()).toContain(FAILED);
    expect(button("Try again")).toBeDefined();
    expect(protection.saved).not.toHaveBeenCalled();
    expect(onSaved).not.toHaveBeenCalled();
    one<HTMLFormElement>("form").dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await settle();
    expect(auth.savePhraseSeal).toHaveBeenCalledTimes(2);
    expect(protection.saved).toHaveBeenCalledTimes(1);
    expect(onSaved).toHaveBeenCalledTimes(1);
  });
});

describe("what is gone", () => {
  it("the panel changes no key and asks the server nothing about a saved phrase", () => {
    const source = readFileSync(resolve(__dirname, "SavePhrasePanel.tsx"), "utf8");
    expect(source).not.toContain("key-rotation");
    expect(source).not.toContain("RecoveryEmailClient");
    expect(source).not.toContain("markPhraseSaved");
    expect(source).not.toContain("phraseStatus");
  });
});
