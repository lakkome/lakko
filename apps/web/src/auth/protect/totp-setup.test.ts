// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import {
  completeTotpCode,
  groupedTotpKey,
  TOTP_SETUP_START,
  totpSetupFailure,
  totpSetupFailureKey,
  totpSetupReduce,
  type TotpSetupEvent,
  type TotpSetupState,
} from "./totp-setup";

const read = (rel: string): string => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");
const answer = (status: number | null, errorCode: string | null = null, retryAfterSecs: number | null = null) => ({
  status,
  errorCode,
  retryAfterSecs,
});
const run = (events: readonly TotpSetupEvent[], from: TotpSetupState = TOTP_SETUP_START): TotpSetupState =>
  events.reduce(totpSetupReduce, from);
const STARTED: TotpSetupEvent = { type: "started", otpauthUri: "otpauth://totp/x", secretB32: "ABCDEFGH" };

describe("reading the server's answers (UX 19.1b)", () => {
  it("a wrong or expired code on the confirmation", () => {
    expect(totpSetupFailure("finish", answer(401))).toEqual({ kind: "wrong" });
  });

  it("a 401 on the start is not a wrong code", () => {
    expect(totpSetupFailure("start", answer(401))).toEqual({ kind: "other" });
  });

  it("the proof is needed", () => {
    expect(totpSetupFailure("start", answer(403, "fresh-auth-required"))).toEqual({ kind: "proof-required" });
    expect(totpSetupFailure("finish", answer(403, "fresh-auth-required"))).toEqual({ kind: "proof-required" });
  });

  it("already set up", () => {
    expect(totpSetupFailure("start", answer(409))).toEqual({ kind: "already" });
    expect(totpSetupFailure("finish", answer(409))).toEqual({ kind: "already" });
  });

  it("too many tries, with the wait when the server names one", () => {
    expect(totpSetupFailure("finish", answer(429, "too-many-attempts", 90))).toEqual({
      kind: "rate-limited",
      retryAfterSecs: 90,
    });
  });

  it("no answer is offline; anything else is the generic failure", () => {
    expect(totpSetupFailure("start", answer(null))).toEqual({ kind: "offline" });
    expect(totpSetupFailure("start", answer(503))).toEqual({ kind: "other" });
    expect(totpSetupFailure("finish", answer(500))).toEqual({ kind: "other" });
    expect(totpSetupFailure("start", answer(403, "account-suspended"))).toEqual({ kind: "other" });
  });
});

describe("the setup's steps", () => {
  it("starting shows the spinner, then the code and the key", () => {
    expect(run([{ type: "start" }])).toEqual({ step: "starting" });
    expect(run([{ type: "start" }, STARTED])).toEqual({
      step: "ready",
      otpauthUri: "otpauth://totp/x",
      secretB32: "ABCDEFGH",
      checking: false,
      failure: null,
    });
  });

  it("checking disables the field; a confirmed code is done", () => {
    const checking = run([{ type: "start" }, STARTED, { type: "check" }]);
    expect(checking).toMatchObject({ step: "ready", checking: true, failure: null });
    expect(totpSetupReduce(checking, { type: "confirmed" })).toEqual({ step: "done" });
  });

  it("a wrong code keeps the QR and the key, with its line", () => {
    const wrong = run([{ type: "start" }, STARTED, { type: "check" }, { type: "failed", failure: { kind: "wrong" } }]);
    expect(wrong).toMatchObject({ step: "ready", secretB32: "ABCDEFGH", checking: false, failure: { kind: "wrong" } });
    expect(totpSetupFailureKey({ kind: "wrong" })).toBe("settings.recovery.totp.error.wrong");
  });

  it("offline keeps the QR and the key", () => {
    const offline = run([{ type: "start" }, STARTED, { type: "check" }, { type: "failed", failure: { kind: "offline" } }]);
    expect(offline).toMatchObject({ step: "ready", secretB32: "ABCDEFGH", failure: { kind: "offline" } });
    expect(totpSetupFailureKey({ kind: "offline" })).toBe("common.error.offline");
  });

  it("too many tries keeps the screen; the countdown is its line", () => {
    const limited = run([
      { type: "start" },
      STARTED,
      { type: "check" },
      { type: "failed", failure: { kind: "rate-limited", retryAfterSecs: 60 } },
    ]);
    expect(limited).toMatchObject({ step: "ready", checking: false });
    expect(totpSetupFailureKey({ kind: "rate-limited", retryAfterSecs: 60 })).toBeNull();
    expect(totpSetupFailureKey({ kind: "rate-limited", retryAfterSecs: null })).toBe("common.error.too_many_attempts");
  });

  it("a proof that ran out is not an error: the key stays, the block appears", () => {
    const state = run([{ type: "start" }, STARTED, { type: "failed", failure: { kind: "proof-required" } }]);
    expect(state).toMatchObject({ step: "ready", secretB32: "ABCDEFGH", failure: { kind: "proof-required" } });
    expect(totpSetupFailureKey({ kind: "proof-required" })).toBeNull();
  });

  it("any other failure is the generic line with Try again, which starts over with a new key", () => {
    const failed = run([{ type: "start" }, STARTED, { type: "check" }, { type: "failed", failure: { kind: "other" } }]);
    expect(failed).toEqual({ step: "failed" });
    expect(totpSetupReduce(failed, { type: "again" })).toEqual(TOTP_SETUP_START);
    // The old key is gone with the state: nothing carries it over.
    expect(JSON.stringify(totpSetupReduce(failed, { type: "again" })).includes("ABCDEFGH")).toBe(false);
  });

  it("a failed start goes back to the proof step with its line, or the generic failure", () => {
    expect(run([{ type: "start" }, { type: "failed", failure: { kind: "offline" } }])).toEqual({
      step: "proof",
      working: false,
      failure: { kind: "offline" },
    });
    expect(run([{ type: "start" }, { type: "failed", failure: { kind: "proof-required" } }])).toMatchObject({
      step: "proof",
    });
    expect(run([{ type: "start" }, { type: "failed", failure: { kind: "other" } }])).toEqual({ step: "failed" });
  });

  it("already set up is final, from the start or from the code", () => {
    expect(run([{ type: "start" }, { type: "failed", failure: { kind: "already" } }])).toEqual({ step: "already" });
    const fromCode = run([{ type: "start" }, STARTED, { type: "check" }, { type: "failed", failure: { kind: "already" } }]);
    expect(fromCode).toEqual({ step: "already" });
    expect(totpSetupReduce(fromCode, { type: "again" })).toEqual({ step: "already" });
    expect(totpSetupFailureKey({ kind: "already" })).toBe("settings.recovery.totp.already_enabled");
  });

  it("done is final", () => {
    const done = run([{ type: "start" }, STARTED, { type: "check" }, { type: "confirmed" }]);
    expect(totpSetupReduce(done, { type: "failed", failure: { kind: "other" } })).toEqual({ step: "done" });
    expect(totpSetupReduce(done, { type: "again" })).toEqual({ step: "done" });
  });

  it("a confirmation that arrives without a key on screen changes nothing", () => {
    expect(totpSetupReduce(TOTP_SETUP_START, { type: "confirmed" })).toEqual(TOTP_SETUP_START);
    expect(totpSetupReduce({ step: "starting" }, { type: "check" })).toEqual({ step: "starting" });
  });
});

describe("the code and the key", () => {
  it("the code is complete on the sixth digit, spaces and dashes dropped", () => {
    expect(completeTotpCode("12345")).toBeNull();
    expect(completeTotpCode("123456")).toBe("123456");
    expect(completeTotpCode("123 456")).toBe("123456");
    expect(completeTotpCode("123-456")).toBe("123456");
    expect(completeTotpCode("1234567")).toBeNull();
    expect(completeTotpCode("12345a")).toBeNull();
  });

  it("the key is shown in groups of four", () => {
    expect(groupedTotpKey("ABCDEFGHIJKLMNOP")).toBe("ABCD EFGH IJKL MNOP");
    expect(groupedTotpKey("abcdefghij")).toBe("ABCD EFGH IJ");
    expect(groupedTotpKey("ABCD EFGH")).toBe("ABCD EFGH");
    expect(groupedTotpKey("")).toBe("");
  });
});

describe("the one setup screen", () => {
  const screen = read("../RecoveryFactors.tsx");

  it("sends the proof with both calls", () => {
    expect(screen.includes("totpEnrollStart(s.token, grant)")).toBe(true);
    expect(screen.includes("totpEnrollFinish(s.token, digits, grant)")).toBe(true);
  });

  it("draws the proof block with the authenticator's own helper, never a browser dialog", () => {
    expect(screen.includes('helpKey: "proof.add_totp.help"')).toBe(true);
    expect(screen.includes("<ProofBlock gate={gate} />")).toBe(true);
    expect(/window\.(confirm|alert|prompt)\(/.test(screen)).toBe(false);
  });

  it("checks the code on the sixth digit, with no confirm button", () => {
    expect(screen.includes("const digits = completeTotpCode(value);")).toBe(true);
    expect(screen.includes('t("common.confirm")')).toBe(false);
  });

  it("uses the shared QR component and shows the key grouped, with Copy", () => {
    expect(screen.includes("<BrandedQr text={current().otpauthUri} />")).toBe(true);
    expect(screen.includes("groupedTotpKey(current().secretB32)")).toBe(true);
    expect(screen.includes('t("common.copy")')).toBe(true);
  });

  it("does not draw the phones-only button, and never logs or stores the key", () => {
    expect(screen.includes("settings.recovery.totp.open_app")).toBe(false);
    expect(/\b(localStorage|sessionStorage|indexedDB)\b/.test(screen)).toBe(false);
    expect(/console\.[a-z]+\([^)]*secret/i.test(screen)).toBe(false);
  });

  it("is the same component on the step and in Settings", () => {
    expect(read("./ProtectAccountPrompt.tsx").includes("<TotpSetup onDone={back} onBack={back} />")).toBe(true);
    expect(screen.includes("export function TotpEnrollView(")).toBe(true);
    expect(screen.match(/<TotpSetup\b/g)?.length).toBe(1);
    expect(screen.includes('t("settings.recovery.totp.on")')).toBe(true);
  });
});
