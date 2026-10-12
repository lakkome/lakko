// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import type { RecoveryEmailStatus } from "./account-protection";
import {
  protectStepDone,
  protectStepExit,
  protectStepRows,
  protectStepSettled,
  skipProtectConfirmation,
} from "./protect-step";
import { phraseSavedBy, savePhraseView } from "./save-phrase";

const status = (over: Partial<RecoveryEmailStatus> = {}): RecoveryEmailStatus => ({
  available: true,
  enabled: false,
  vault: false,
  phraseSaved: false,
  totp: false,
  ...over,
});
const read = (rel: string): string => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

describe("the Protect your account step", () => {
  it("offers the email, then a passkey, then the phrase", () => {
    expect(protectStepRows(status(), true, true)).toEqual(["email", "passkey", "phrase"]);
  });

  it("has the passkey row only where this browser can use passkeys", () => {
    expect(protectStepRows(status(), true, false)).toEqual(["email", "phrase"]);
  });

  it("has no email row when mail is not set up, or while that is not known", () => {
    expect(protectStepRows(status({ available: false }), true, true)).toEqual(["passkey", "phrase"]);
    expect(protectStepRows(null, true, false)).toEqual(["phrase"]);
  });

  it("has no email and no passkey row for an account without a username", () => {
    expect(protectStepRows(status(), false, true)).toEqual(["phrase"]);
  });

  it("marks the rows that are done", () => {
    const none = { status: status(), hasPasskey: false, phraseProtectedHere: false };
    expect(protectStepDone(none)).toEqual([]);
    expect(protectStepDone({ ...none, status: status({ enabled: true, vault: true }) })).toEqual(["email"]);
    expect(protectStepDone({ ...none, hasPasskey: true })).toEqual(["passkey"]);
    expect(protectStepDone({ ...none, phraseProtectedHere: true })).toEqual(["phrase"]);
    // An email whose vault is gone (and no phrase) is not done: it is to be added again.
    expect(protectStepDone({ ...none, status: status({ enabled: true }) })).toEqual([]);
    expect(protectStepDone({ ...none, status: null })).toEqual([]);
    expect(
      protectStepDone({ status: status({ enabled: true, vault: true }), hasPasskey: true, phraseProtectedHere: false }),
    ).toEqual(["email", "passkey"]);
  });

  it("Not now until a row is done, then the primary Continue", () => {
    expect(protectStepExit([])).toBe("not-now");
    expect(protectStepExit(["passkey"])).toBe("continue");
    expect(protectStepExit(["email", "passkey"])).toBe("continue");
  });

  it("only a saved phrase ends the step by itself; an email or a passkey marks its row", () => {
    expect(protectStepSettled(status(), false)).toBe(false);
    expect(protectStepSettled(null, false)).toBe(false);
    expect(protectStepSettled(status({ enabled: true, vault: true }), false)).toBe(false);
    expect(protectStepSettled(status({ phraseSaved: true }), false)).toBe(true);
    // The phrase was saved on this device and the server was not read yet.
    expect(protectStepSettled(status(), true)).toBe(true);
  });

  it("Not now asks once, with the safe choice as the main action", () => {
    const q = skipProtectConfirmation(true);
    expect(q.title).toBe("Continue without protection?");
    expect(q.body).toBe(
      "If you forget your password, or lose this phone, you will not be able to get back into this account.",
    );
    expect(q.confirmLabel).toBe("Add an email");
    expect(q.cancelLabel).toBe("Continue without");
    expect(q.tone).toBe("neutral");
    // Without mail there is no email to add: the main action stays on the step.
    expect(skipProtectConfirmation(false).confirmLabel).toBe("Back");
  });
});

describe("sign-up shows no recovery phrase (ADR-0067 D2)", () => {
  const ctx = read("../AuthContext.tsx");
  const start = ctx.indexOf("    async signup(username, password, options) {");
  const body = ctx.slice(start, ctx.indexOf("\n    async signupGuest(", start));

  it("stores the phrase on the device exactly as before", () => {
    expect(start).toBeGreaterThan(0);
    expect(body.includes('await storeDevicePhrase(accountIdB64, result.recoveryPhrase, deviceKey, "signup");')).toBe(
      true,
    );
  });

  it("owes the protect step, and neither shows the words nor marks them owed", () => {
    expect(body.includes("markProtectStepOwed(plainStorage(), accountIdB64);")).toBe(true);
    expect(body.includes("setSignupPhrase(")).toBe(false);
    expect(body.includes("markSignupPhraseOwed(")).toBe(false);
    // The step is owed before the session lands, so no guard sees a session
    // without it.
    expect(body.indexOf("markProtectStepOwed(")).toBeLessThan(body.indexOf('kind: "session"'));
  });

  it("the sign-up page renders no phrase and no confirmation of one", () => {
    const page = read("../../routes/Signup.tsx");
    expect(/PhraseGrid|WordSlots|recovery_phrase\./.test(page)).toBe(false);
  });

  it("the step uses the app's own dialog, never the browser's", () => {
    const prompt = read("./ProtectAccountPrompt.tsx");
    expect(prompt.includes("requestConfirm(skipProtectConfirmation(")).toBe(true);
    expect(/window\.(confirm|alert|prompt)\(/.test(prompt)).toBe(false);
  });
});

describe("Save a recovery phrase", () => {
  it("shows the words only when the change has gone through", () => {
    expect(savePhraseView({ kind: "complete" })).toEqual({ kind: "done" });
  });

  it("after the commit there is no way back: the rest finishes by itself", () => {
    expect(savePhraseView({ kind: "publishing", owed: ["directory"] })).toEqual({ kind: "finishing" });
    expect(phraseSavedBy({ kind: "publishing", owed: ["directory"] })).toBe(true);
    expect(phraseSavedBy({ kind: "complete" })).toBe(true);
  });

  it("before the commit a failure changed nothing", () => {
    for (const reason of ["prepare", "unconfirmed", "other", "account-key-required", "removal-finishing"] as const) {
      expect(savePhraseView({ kind: "not-started", reason })).toEqual({ kind: "failed" });
      expect(phraseSavedBy({ kind: "not-started", reason })).toBe(false);
    }
    expect(savePhraseView(null)).toEqual({ kind: "failed" });
    expect(phraseSavedBy(null)).toBe(false);
  });

  it("a missing proof is the proof block and a limit is the wait, neither an error", () => {
    expect(savePhraseView({ kind: "not-started", reason: "proof-required" })).toEqual({ kind: "proof" });
    expect(savePhraseView({ kind: "not-started", reason: "rate-limited" })).toEqual({ kind: "rate-limited" });
  });
});

describe("no surface logs or keeps the address or a code", () => {
  const files = [
    "./RecoveryEmailForm.tsx",
    "./RemoveRecoveryEmail.tsx",
    "./ProtectionCard.tsx",
    "./AccountNotices.tsx",
    "./recovery-email-client.ts",
    "./enrol-run.ts",
    "./email-vault.ts",
    "./email-vault-job.ts",
    "./email-vault-worker.ts",
    "../recover/EmailRecover.tsx",
  ];

  it("has no console call and no storage write in the email surfaces", () => {
    for (const file of files) {
      const code = read(file);
      expect(/console\.(log|info|warn|error|debug)\(/.test(code), file).toBe(false);
      expect(/\b(localStorage|sessionStorage|indexedDB)\b/.test(code), file).toBe(false);
      expect(/\.setItem\(/.test(code), file).toBe(false);
    }
  });
});

describe("new words are shown only to a phrase-protected account (alignment A and I)", () => {
  const ctx = read("../AuthContext.tsx");

  it("owed words of any other account are dropped, never withheld", () => {
    const start = ctx.indexOf("async function restoreOwedSignupPhrase(");
    const body = ctx.slice(start, ctx.indexOf("\n  }\n", start));
    const rule = body.indexOf("if (!newWordsShown(isPhraseProtected(storage, accountIdB64),");
    expect(rule).toBeGreaterThan(0);
    expect(body.indexOf("clearSignupPhraseOwed(storage, accountIdB64);", rule)).toBeGreaterThan(rule);
    // Decided before anything reads the phrase off the device.
    expect(rule).toBeLessThan(body.indexOf("readDevicePhraseWith("));
  });

  it("a recall records whether the account had a saved phrase before anything is sent", () => {
    const start = ctx.indexOf("function runRecall(");
    const body = ctx.slice(start, ctx.indexOf("\n  }\n", start));
    const flag = body.indexOf("setPhraseProtected(plainStorage(), accountIdB64, check.status.phraseArm);");
    expect(flag).toBeGreaterThan(0);
    expect(flag).toBeLessThan(body.indexOf("await client.recallPrepare("));
  });

  it("the reset / guest phrase screen lands by itself when no words are shown", () => {
    const prompt = read("../SaveFreshPhrasePrompt.tsx");
    expect(prompt.includes("newWordsShown(phraseProtectedHere(s.account.accountId)")).toBe(true);
    // A guest sign-up has no phrase screen any more.
    expect(prompt.includes("recovery_phrase.guest.lede")).toBe(false);
  });

  it("only the save-phrase flow sends phrase_saved with the rotation", () => {
    const runtime = read("../../devices/key-rotation-runtime.ts");
    expect(runtime.includes("{ phraseSaved: rotationSavesPhrase(current) }")).toBe(true);
    const client = read("../client.ts");
    expect(client.includes("...(args.phraseSaved === true ? { phrase_saved: true } : {})")).toBe(true);
  });
});
