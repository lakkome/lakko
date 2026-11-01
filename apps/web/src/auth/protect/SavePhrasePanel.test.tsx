// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Save a recovery phrase" (docs/ux/identity-recovery.md 19.5) as wiring: the
 * server chooses the path, the words path changes no key and counts as saved
 * only when `POST /account/phrase/saved` answered, and everything else is the
 * rotation it always was.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const WORDS = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima";

const auth = {
  state: vi.fn(() => ({ kind: "session" as const, account: { accountId: "acct", username: "alice" } })),
  identitySecrets: vi.fn(() => ({ sessionToken: "token", accountIdentitySeed: new Uint8Array(64) })),
};
vi.mock("../AuthContext", () => ({ useAuth: () => auth }));

const rotation = {
  record: vi.fn(() => null),
  savePhrase: vi.fn(async () => ({ kind: "ran", outcome: { kind: "complete" }, error: null })),
  notNow: vi.fn(async () => undefined),
};
vi.mock("../../devices/key-rotation-runtime", () => ({ useKeyRotation: () => rotation }));

const client = {
  phraseStatus: vi.fn(async () => ({ saved: false, rotationNeeded: false })),
  markPhraseSaved: vi.fn(async () => undefined),
};
vi.mock("./recovery-email-client", () => ({
  RecoveryEmailClient: function RecoveryEmailClient() {
    return client;
  },
}));

const device = { phrase: WORDS as string | null };
vi.mock("../../backup/device-phrase", () => ({ readDevicePhrase: async () => device.phrase }));
vi.mock("../../wasm", () => ({
  loadWasm: async () => ({
    RecoveryMnemonic: {
      fromPhrase: () => ({ toSeed: () => new Uint8Array(64), free: () => undefined }),
    },
  }),
}));

const gate = {
  ensure: vi.fn(async () => "grant"),
  failed: vi.fn(() => ({ kind: "other" })),
  clear: vi.fn(),
  view: () => ({ visible: false, ready: true }),
  wait: { waiting: () => false, remainingMs: () => 0 },
};
vi.mock("../proof-gate", () => ({ createProofGate: () => gate }));
vi.mock("../ProofBlock", () => ({ ProofBlock: () => null }));
vi.mock("../recover/FormNotices", () => ({ WaitNotice: () => null }));

const protection = { saved: vi.fn(), refreshed: vi.fn() };
vi.mock("./protection-state", () => ({
  notePhraseSaved: (a: string) => protection.saved(a),
  refreshProtection: (a: string) => protection.refreshed(a),
}));

import { AuthHttpError } from "../client";
import { all, installLocalStorage, one, present, renderUi, settle } from "../../test/ui";
import { readSelfMarks } from "./notices";
import { SavePhrasePanel } from "./SavePhrasePanel";

const EFFECT = "This gives your account new keys";
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
  client.phraseStatus.mockResolvedValue({ saved: false, rotationNeeded: false });
  client.markPhraseSaved.mockResolvedValue(undefined);
  gate.ensure.mockResolvedValue("grant");
  rotation.record.mockReturnValue(null);
});

describe("the path", () => {
  it("rotation_needed keeps the rotation, with its paragraph, and no word comes from the device", async () => {
    client.phraseStatus.mockResolvedValue({ saved: false, rotationNeeded: true });
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} />);
    await settle();
    expect(text()).toContain(EFFECT);
    button("Show my 12 words")?.click();
    await settle();
    expect(rotation.savePhrase).toHaveBeenCalledWith("grant");
    expect(present(".lk-phrase-cols")).toBe(false);
    expect(client.markPhraseSaved).not.toHaveBeenCalled();
  });

  it("a device that does not hold the phrase rotates", async () => {
    device.phrase = null;
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} />);
    await settle();
    expect(text()).toContain(EFFECT);
    button("Show my 12 words")?.click();
    await settle();
    expect(rotation.savePhrase).toHaveBeenCalledTimes(1);
  });

  it("a failed read shows the failure and Try again, and offers no path", async () => {
    client.phraseStatus.mockRejectedValueOnce(new Error("offline"));
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} />);
    await settle();
    expect(text()).toContain("We could not finish. Nothing changed.");
    expect(button("Show my 12 words")).toBeUndefined();
    button("Try again")?.click();
    await settle();
    expect(client.phraseStatus).toHaveBeenCalledTimes(2);
    expect(button("Show my 12 words")).toBeDefined();
    expect(text()).not.toContain("We could not finish.");
  });

  it("the extra line about the new keys goes with them", async () => {
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} extraLine="so whoever opened it loses it" />);
    await settle();
    expect(text()).not.toContain("whoever opened it");
  });
});

describe("the words the device holds", () => {
  it("shows them after the proof, with no paragraph about new keys and no rotation", async () => {
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} />);
    await settle();
    expect(text()).not.toContain(EFFECT);
    button("Show my 12 words")?.click();
    await settle();
    expect(gate.ensure).toHaveBeenCalled();
    expect(text()).toContain("foxtrot");
    expect(rotation.savePhrase).not.toHaveBeenCalled();
  });

  it("no proof, no words", async () => {
    gate.ensure.mockResolvedValue(null as unknown as string);
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} />);
    await settle();
    button("Show my 12 words")?.click();
    await settle();
    expect(text()).not.toContain("foxtrot");
  });

  it("saved only when the server answered, marked as this device's own first", async () => {
    const onSaved = vi.fn();
    let marksAtPost = 0;
    client.markPhraseSaved.mockImplementation(async () => {
      marksAtPost = readSelfMarks(globalThis.localStorage, "acct").length;
    });
    renderUi(() => <SavePhrasePanel onSaved={onSaved} />);
    await settle();
    await reachCheckAndSubmit();
    expect(client.markPhraseSaved).toHaveBeenCalledWith("token", "grant");
    expect(marksAtPost).toBe(1);
    expect(protection.saved).toHaveBeenCalledWith("acct");
    expect(onSaved).toHaveBeenCalledTimes(1);
    expect(rotation.savePhrase).not.toHaveBeenCalled();
  });

  it("a failed call stays on the check with the line and Try again, and saves nothing", async () => {
    const onSaved = vi.fn();
    client.markPhraseSaved.mockRejectedValueOnce(new AuthHttpError(500, "/account/phrase/saved", ""));
    renderUi(() => <SavePhrasePanel onSaved={onSaved} />);
    await settle();
    await reachCheckAndSubmit();
    expect(text()).toContain("We could not finish. Nothing changed.");
    expect(button("Try again")).toBeDefined();
    expect(protection.saved).not.toHaveBeenCalled();
    expect(onSaved).not.toHaveBeenCalled();
    one<HTMLFormElement>("form").dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await settle();
    expect(client.markPhraseSaved).toHaveBeenCalledTimes(2);
    expect(onSaved).toHaveBeenCalledTimes(1);
    // One notice, one mark, however many tries.
    expect(readSelfMarks(globalThis.localStorage, "acct")).toHaveLength(1);
  });

  it("409 falls back to the rotation, says so, and leaves no mark behind", async () => {
    client.markPhraseSaved.mockRejectedValueOnce(
      new AuthHttpError(409, "/account/phrase/saved", JSON.stringify({ error: "phrase-save-needs-rotation" })),
    );
    renderUi(() => <SavePhrasePanel onSaved={() => undefined} />);
    await settle();
    await reachCheckAndSubmit();
    expect(text()).toContain(EFFECT);
    expect(protection.saved).not.toHaveBeenCalled();
    expect(readSelfMarks(globalThis.localStorage, "acct")).toHaveLength(0);
    button("Show my 12 words")?.click();
    await settle();
    expect(rotation.savePhrase).toHaveBeenCalledTimes(1);
  });
});
