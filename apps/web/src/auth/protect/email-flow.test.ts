// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CODE_TRIES,
  emailFailure,
  emailFailureKey,
  ENROL_START,
  enrolReduce,
  enrolSealsVault,
  normalizeEmailCode,
  RECOVER_START,
  recoverReduce,
  resendRemainingMs,
  type EmailFailure,
  type EnrolState,
  type RecoverEvent,
  type RecoverState,
} from "./email-flow";
import { t } from "../../l10n";

const NOW = 1_800_000_000_000;
const answer = (status: number | null, errorCode: string | null = null, retryAfterSecs: number | null = null) => ({
  status,
  errorCode,
  retryAfterSecs,
});
const run = (events: readonly RecoverEvent[], from: RecoverState = RECOVER_START): RecoverState =>
  events.reduce(recoverReduce, from);
const started: RecoverEvent = { type: "started", challengeId: "c1", resendAfterSecs: 45, nowMs: NOW };

describe("reading the server's answers", () => {
  it("a wrong code and a code that was never sent are one line", () => {
    const f = emailFailure("recover-verify", answer(401, "code-refused"));
    expect(f).toEqual({ kind: "code-wrong" });
    expect(t(emailFailureKey(f) ?? "")).toBe("That code is not right.");
  });

  it("an expired enrol code says so", () => {
    const f = emailFailure("enrol-finish", answer(410, "code-expired"));
    expect(t(emailFailureKey(f) ?? "")).toBe("That code has expired.");
  });

  it("too many wrong codes ask for a new code; too many letters are the countdown", () => {
    expect(emailFailure("recover-verify", answer(429, "too-many-attempts", 60))).toEqual({ kind: "used-up" });
    expect(emailFailure("enrol-finish", answer(429, "too-many-attempts", 60))).toEqual({ kind: "used-up" });
    expect(emailFailure("recover-totp", answer(429, "too-many-attempts", 60))).toEqual({ kind: "used-up" });
    const limited = emailFailure("recover-start", answer(429, "email-rate-limited", 90));
    expect(limited).toEqual({ kind: "rate-limited", retryAfterSecs: 90 });
    // The countdown is the line.
    expect(emailFailureKey(limited)).toBeNull();
    // A letter route's own attempt limit is a wait too, not "send a new code".
    expect(emailFailure("recover-start", answer(429, "too-many-attempts", 30))).toEqual({
      kind: "rate-limited",
      retryAfterSecs: 30,
    });
    expect(emailFailureKey({ kind: "rate-limited", retryAfterSecs: null })).toBe(
      "common.error.too_many_attempts",
    );
  });

  it("names the letter that could not be sent, mail that is off, and no connection", () => {
    expect(emailFailureKey(emailFailure("enrol-start", answer(502, "mail-send-failed")))).toBe(
      "recovery_email.send_failed",
    );
    expect(emailFailureKey(emailFailure("recover-start", answer(503, "recovery-email-disabled")))).toBe(
      "recovery_email.unavailable",
    );
    expect(emailFailureKey(emailFailure("recover-start", answer(null)))).toBe("common.error.offline");
    // The limiter's own 503 is not "mail is off".
    expect(emailFailure("recover-start", answer(503, "rate-limiter-unavailable"))).toEqual({ kind: "error" });
  });

  it("a missing proof is the proof block, not an error line", () => {
    const f = emailFailure("enrol-start", answer(403, "fresh-auth-required"));
    expect(f).toEqual({ kind: "proof-required" });
    expect(emailFailureKey(f)).toBeNull();
  });

  it("an address the rule refuses, and a guest account", () => {
    expect(emailFailure("enrol-start", answer(400, "address-invalid"))).toEqual({ kind: "address-invalid" });
    expect(emailFailureKey(emailFailure("enrol-start", answer(409, "username-required")))).toBe(
      "recovery_email.needs_username",
    );
  });

  it("a ticket that is gone or out of step starts the flow over", () => {
    for (const [status, code] of [
      [401, "ticket-refused"],
      [403, "totp-required"],
      [409, "wrong-mode"],
    ] as const) {
      expect(emailFailure("recover-open", answer(status, code))).toEqual({ kind: "start-over" });
    }
  });
});

describe("codes and the resend clock", () => {
  it("takes six digits, with spaces or a dash", () => {
    expect(normalizeEmailCode("123456")).toBe("123456");
    expect(normalizeEmailCode(" 123 456 ")).toBe("123456");
    expect(normalizeEmailCode("123-456")).toBe("123456");
    expect(normalizeEmailCode("12345")).toBeNull();
    expect(normalizeEmailCode("12345a")).toBeNull();
  });

  it("counts down to Send again and stops at zero", () => {
    expect(resendRemainingMs(NOW + 45_000, NOW)).toBe(45_000);
    expect(resendRemainingMs(NOW, NOW + 1)).toBe(0);
  });
});

describe("the email vault is sealed only while no phrase is saved", () => {
  it("mail-protected: seal", () => {
    expect(enrolSealsVault(false)).toBe(true);
  });
  it("phrase-protected: none", () => {
    expect(enrolSealsVault(true)).toBe(false);
  });
});

describe("adding the address", () => {
  const sent = { type: "sent", challengeId: "c1", vaultSalt: "c2FsdA==", resendAfterSecs: 45, nowMs: NOW } as const;

  it("address, then the code step with the salt the vault is sealed with", () => {
    const s = [{ type: "submitted" } as const, sent].reduce(enrolReduce, ENROL_START);
    expect(s).toEqual({
      step: "code",
      working: false,
      failure: null,
      challengeId: "c1",
      vaultSalt: "c2FsdA==",
      resendAtMs: NOW + 45_000,
      wrong: 0,
    });
  });

  it("a wrong code keeps the code step; a spent code returns to the address", () => {
    const code = enrolReduce(ENROL_START, sent);
    const wrong = enrolReduce(code, { type: "failed", failure: { kind: "code-wrong" } });
    expect(wrong.step).toBe("code");
    const spent = enrolReduce(code, { type: "failed", failure: { kind: "used-up" } });
    expect(spent).toEqual({ step: "address", working: false, failure: { kind: "used-up" } });
  });

  it("the fifth wrong code is counted here too: a new code is needed (alignment L)", () => {
    let s: EnrolState = enrolReduce(ENROL_START, sent);
    for (let i = 0; i < CODE_TRIES - 1; i += 1) {
      s = enrolReduce(s, { type: "failed", failure: { kind: "code-wrong" } });
      expect(s.step).toBe("code");
    }
    s = enrolReduce(s, { type: "failed", failure: { kind: "code-wrong" } });
    expect(s).toEqual({ step: "address", working: false, failure: { kind: "used-up" } });
  });

  it("Use a different address goes back with nothing kept", () => {
    expect(enrolReduce(enrolReduce(ENROL_START, sent), { type: "other-address" })).toEqual(ENROL_START);
  });

  it("ends done, and stays done", () => {
    const done: EnrolState = enrolReduce(enrolReduce(ENROL_START, sent), { type: "finished", vault: true });
    expect(done).toEqual({ step: "done", vault: true });
    expect(enrolReduce(done, { type: "submitted" })).toBe(done);
  });
});

describe("getting back in with the code", () => {
  it("step 2 is the same whatever was typed: the start event carries nothing that tells", () => {
    // The server answers a matching pair, a wrong pair, an unknown username
    // and an account with no email alike; whatever challenge id and timing
    // it returns, the screen reaches the same step with the same fields.
    const a = run([{ type: "submitted" }, started]);
    const b = run([{ type: "submitted" }, { ...started, challengeId: "decoy" }]);
    expect(a.step).toBe("code");
    expect(b.step).toBe("code");
    expect(Object.keys(a).sort()).toEqual(Object.keys(b).sort());
    expect({ ...a, challengeId: "" }).toEqual({ ...b, challengeId: "" });
    // The event type has no field for a match.
    expect(Object.keys(started).sort()).toEqual(["challengeId", "nowMs", "resendAfterSecs", "type"]);
  });

  it("without an authenticator the code leads straight to the new password", () => {
    const s = run([started, { type: "verified", ticket: "t1", totpRequired: false }]);
    expect(s).toEqual({
      step: "password",
      working: false,
      failure: null,
      ticket: "t1",
      phase: "idle",
      vaultOpened: false,
    });
  });

  it("with an authenticator its code is asked first, and there is no way past it", () => {
    const totp = run([started, { type: "verified", ticket: "t1", totpRequired: true }]);
    expect(totp).toEqual({ step: "totp", working: false, failure: null, ticket: "t1" });
    // Nothing but the accepted authenticator code reaches the password step.
    expect(recoverReduce(totp, { type: "opening" })).toBe(totp);
    expect(recoverReduce(totp, { type: "vault-opened" })).toBe(totp);
    expect(recoverReduce(totp, { type: "verified", ticket: "t2", totpRequired: false })).toBe(totp);
    const wrong = recoverReduce(totp, { type: "failed", failure: { kind: "code-wrong" } });
    expect(wrong.step).toBe("totp");
    expect(recoverReduce(totp, { type: "totp-passed" }).step).toBe("password");
  });

  it("the authenticator step appears only after the mailed code was accepted", () => {
    expect(recoverReduce(RECOVER_START, { type: "totp-passed" })).toBe(RECOVER_START);
    expect(recoverReduce(RECOVER_START, { type: "verified", ticket: "t", totpRequired: true })).toBe(
      RECOVER_START,
    );
  });

  const atPassword = run([started, { type: "verified", ticket: "t1", totpRequired: false }]);

  it("vault mode: opening, then restoring the account, then landed", () => {
    const opening = recoverReduce(atPassword, { type: "opening" });
    expect(opening).toMatchObject({ step: "password", working: true, phase: "opening" });
    const restoring = recoverReduce(opening, { type: "vault-opened" });
    expect(restoring).toMatchObject({ step: "password", working: true, phase: "restoring", vaultOpened: true });
    expect(recoverReduce(restoring, { type: "landed" })).toEqual({ step: "landed" });
  });

  it("password mode: no vault is opened; the session lands", () => {
    const opening = recoverReduce(atPassword, { type: "opening" });
    const landed = recoverReduce(opening, { type: "landed" });
    expect(landed).toEqual({ step: "landed" });
  });

  it("nothing before the new password tells which kind of account this is", () => {
    // The states up to and including the idle password step have no field
    // that differs between the two kinds.
    expect(Object.keys(atPassword).sort()).toEqual(
      ["failure", "phase", "step", "ticket", "vaultOpened", "working"].sort(),
    );
    expect(atPassword).toMatchObject({ phase: "idle", vaultOpened: false });
  });

  it("a vault that does not open ends on Get back in, and stays there", () => {
    const notOpened = recoverReduce(recoverReduce(atPassword, { type: "opening" }), { type: "vault-not-opened" });
    expect(notOpened).toEqual({ step: "not-opened" });
    expect(recoverReduce(notOpened, { type: "submitted" })).toBe(notOpened);
    expect(recoverReduce(notOpened, { type: "landed" })).toBe(notOpened);
  });

  it("a restore that did not finish keeps the opened vault for the retry", () => {
    const restoring = run([{ type: "opening" }, { type: "vault-opened" }], atPassword);
    const back = recoverReduce(restoring, { type: "restore-failed" });
    expect(back).toMatchObject({ step: "password", working: false, phase: "idle", vaultOpened: true });
  });

  it("a spent code or a lost ticket returns to step 1 with the line", () => {
    const code = run([started]);
    const spent: EmailFailure = { kind: "used-up" };
    expect(recoverReduce(code, { type: "failed", failure: spent })).toEqual({
      step: "address",
      working: false,
      failure: spent,
    });
    expect(recoverReduce(atPassword, { type: "failed", failure: { kind: "start-over" } }).step).toBe("address");
  });

  it("five wrong codes for one code return to step 1; a new code starts the count again (alignment L)", () => {
    let s = run([started]);
    for (let i = 0; i < CODE_TRIES - 1; i += 1) {
      s = recoverReduce(s, { type: "failed", failure: { kind: "code-wrong" } });
      expect(s).toMatchObject({ step: "code", wrong: i + 1 });
    }
    expect(recoverReduce(s, { type: "failed", failure: { kind: "code-wrong" } })).toEqual({
      step: "address",
      working: false,
      failure: { kind: "used-up" },
    });
    // "Send again" is a new code with its own five tries.
    expect(recoverReduce(s, started)).toMatchObject({ step: "code", wrong: 0 });
    expect(CODE_TRIES).toBe(5);
  });

  it("nothing before the password step can open the account (alignment C)", () => {
    // `opening` is the event the screen sends right before `recover/open`.
    // It is accepted on the password step only.
    const code = run([started]);
    const totp = run([started, { type: "verified", ticket: "t1", totpRequired: true }]);
    for (const early of [RECOVER_START, code, totp]) {
      expect(recoverReduce(early, { type: "opening" })).toBe(early);
      expect(recoverReduce(early, { type: "vault-opened" })).toBe(early);
      expect(recoverReduce(early, { type: "vault-not-opened" })).toBe(early);
    }
    expect(recoverReduce(atPassword, { type: "opening" })).toMatchObject({ step: "password", phase: "opening" });
  });

  it("mail that is not set up ends the method", () => {
    expect(recoverReduce(RECOVER_START, { type: "failed", failure: { kind: "unavailable" } })).toEqual({
      step: "unavailable",
    });
  });

  it("Back on the code step returns to step 1", () => {
    expect(recoverReduce(run([started]), { type: "back" })).toEqual(RECOVER_START);
  });
});
