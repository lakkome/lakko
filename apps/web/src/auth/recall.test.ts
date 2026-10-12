// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * "Was it you?" and taking the account back: every pure decision
 * (ADR-0066 part 1 and "T3 as built"; UX 18.4).
 */
import { describe, expect, it } from "vitest";

import {
  deleteBlockedUntilMs,
  RECALL_LOCAL_WINDOW_MS,
  RECALL_SCREEN_START,
  recallAttemptFailure,
  recallExpired,
  recallFormReady,
  recallNoEnabled,
  recallReduce,
  recallStatusVerdict,
  signOutWipe,
  type RecallCheck,
  asksPhraseRecall,
  parseRecallStatus,
  phraseRecallOffered,
  type RecallEvent,
  type RecallScreen,
  type RecallStatus,
} from "./recall";
import { bearerRefusal, refreshRefusal } from "./session-401";

const NOW = 1_800_000_000_000;
const STATUS: RecallStatus = {
  anchorSeq: 1,
  expiresAtMs: NOW + 10 * 86_400_000,
  changedAtMs: NOW - 86_400_000,
  attemptsRemaining: 20,
  phraseArm: false,
  accountIdB64Url: null,
};
const OPEN: RecallCheck = { kind: "open", status: STATUS };
const body = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
  anchor_seq: 1,
  expires_at_ms: STATUS.expiresAtMs,
  changed_at_ms: STATUS.changedAtMs,
  attempts_remaining: 20,
  ...over,
});
const run = (events: readonly RecallEvent[], from: RecallScreen = RECALL_SCREEN_START): RecallScreen =>
  events.reduce(recallReduce, from);

describe("which wipe runs", () => {
  it("staged only after a key reset on a device that held the account key", () => {
    expect(signOutWipe({ cause: "keys-reset", holdsAccountKey: true, deviceKeyPresent: true })).toBe("staged");
  });

  it("full on a device that never held the keys: it keeps nothing", () => {
    expect(signOutWipe({ cause: "keys-reset", holdsAccountKey: false, deviceKeyPresent: true })).toBe("full");
    expect(signOutWipe({ cause: "keys-reset", holdsAccountKey: false, deviceKeyPresent: false })).toBe("full");
    // No device key to seal a record under.
    expect(signOutWipe({ cause: "keys-reset", holdsAccountKey: true, deviceKeyPresent: false })).toBe("full");
  });

  it("a removed device never keeps a recall record, whatever it holds", () => {
    for (const holdsAccountKey of [true, false]) {
      for (const deviceKeyPresent of [true, false]) {
        expect(signOutWipe({ cause: "device-removed", holdsAccountKey, deviceKeyPresent })).toBe("full");
        expect(signOutWipe({ cause: "identity-mismatch", holdsAccountKey, deviceKeyPresent })).toBe("full");
      }
    }
  });

  it("follows the server's code: superseded may be staged, device-revoked is always full", () => {
    const wipeFor = (errorCode: string): string => {
      const refusal = bearerRefusal({ status: 401, errorCode, retried: false, refreshAvailable: true });
      const cause = refusal === "superseded" ? "keys-reset" : "device-removed";
      return `${refusal}:${signOutWipe({ cause, holdsAccountKey: true, deviceKeyPresent: true })}`;
    };
    expect(wipeFor("key-generation-superseded")).toBe("superseded:staged");
    expect(wipeFor("device-revoked")).toBe("device-revoked:full");
    expect(refreshRefusal("key-generation-superseded")).toBe("superseded");
    expect(refreshRefusal("device-revoked")).toBe("device-revoked");
  });
});

describe("the record's own end", () => {
  it("is 30 days after the device learned of the reset", () => {
    expect(RECALL_LOCAL_WINDOW_MS).toBe(30 * 86_400_000);
    expect(recallExpired(NOW, NOW)).toBe(false);
    expect(recallExpired(NOW, NOW + RECALL_LOCAL_WINDOW_MS - 1)).toBe(false);
    expect(recallExpired(NOW, NOW + RECALL_LOCAL_WINDOW_MS)).toBe(true);
  });

  it("is not reached by a clock that went backwards", () => {
    expect(recallExpired(NOW, NOW - 5 * RECALL_LOCAL_WINDOW_MS)).toBe(false);
  });
});

describe("the status answer", () => {
  it("200 is open, with the date, the reset time and the tries", () => {
    expect(recallStatusVerdict({ status: 200, errorCode: null, body: body() }, NOW)).toEqual(OPEN);
  });

  it("401 recall-refused closes: the account cannot be taken back from here", () => {
    expect(recallStatusVerdict({ status: 401, errorCode: "recall-refused", body: null }, NOW)).toEqual({
      kind: "closed",
    });
  });

  it("a window the server says has passed is closed", () => {
    expect(
      recallStatusVerdict({ status: 200, errorCode: null, body: body({ expires_at_ms: NOW }) }, NOW),
    ).toEqual({ kind: "closed" });
  });

  it("no tries left is exhausted", () => {
    expect(
      recallStatusVerdict({ status: 200, errorCode: null, body: body({ attempts_remaining: 0 }) }, NOW),
    ).toEqual({ kind: "exhausted" });
  });

  it("no answer is offline and keeps the record", () => {
    expect(recallStatusVerdict({ status: null, errorCode: null, body: null }, NOW)).toEqual({
      kind: "unknown",
      offline: true,
    });
  });

  it("nothing but the server's own refusal destroys the record", () => {
    const unknown = { kind: "unknown", offline: false };
    for (const answer of [
      { status: 401, errorCode: null },
      { status: 401, errorCode: "wrong-password" },
      { status: 401, errorCode: "key-generation-superseded" },
      { status: 403, errorCode: "recall-refused" },
      { status: 429, errorCode: "too-many-attempts" },
      { status: 400, errorCode: null },
      { status: 500, errorCode: null },
      { status: 503, errorCode: "rate-limiter-unavailable" },
    ]) {
      expect(recallStatusVerdict({ ...answer, body: null }, NOW), JSON.stringify(answer)).toEqual(unknown);
    }
    // A 200 this client cannot read.
    for (const bad of [null, "ok", {}, body({ anchor_seq: "1" }), body({ expires_at_ms: null })]) {
      expect(recallStatusVerdict({ status: 200, errorCode: null, body: bad }, NOW)).toEqual(unknown);
    }
  });
});

describe("the answers of start and finish", () => {
  const answer = (status: number | null, errorCode: string | null, retryAfterSecs: number | null = null) =>
    recallAttemptFailure({ wrongPasswordLocally: false, status, errorCode, retryAfterSecs });

  it("a login that does not finish on the device is the wrong old password", () => {
    expect(
      recallAttemptFailure({ wrongPasswordLocally: true, status: null, errorCode: null, retryAfterSecs: null }),
    ).toEqual({ kind: "wrong-password" });
  });

  it("401 wrong-password (finish) is the wrong old password", () => {
    expect(answer(401, "wrong-password")).toEqual({ kind: "wrong-password" });
  });

  it("401 recall-refused is closed", () => {
    expect(answer(401, "recall-refused")).toEqual({ kind: "closed" });
  });

  it("429 carries the wait", () => {
    expect(answer(429, "too-many-attempts", 840)).toEqual({ kind: "rate-limited", retryAfterSecs: 840 });
    expect(answer(429, "too-many-attempts")).toEqual({ kind: "rate-limited", retryAfterSecs: null });
  });

  it("no answer is offline", () => {
    expect(answer(null, null)).toEqual({ kind: "offline" });
  });

  it("400, a server error and an unnamed 401 are errors that close nothing", () => {
    expect(answer(400, null)).toEqual({ kind: "error" });
    expect(answer(500, null)).toEqual({ kind: "error" });
    expect(answer(401, null)).toEqual({ kind: "error" });
    expect(answer(503, "rate-limiter-unavailable")).toEqual({ kind: "error" });
  });
});

describe("the form", () => {
  const filled = { oldPassword: "before", newPassword: "long enough", repeatPassword: "long enough" };

  it("is ready when the three fields are filled, the new password passes the sign-up rule and the two match", () => {
    expect(recallFormReady(filled)).toBe(true);
    expect(recallFormReady({ ...filled, oldPassword: "" })).toBe(false);
    expect(recallFormReady({ ...filled, newPassword: "short", repeatPassword: "short" })).toBe(false);
    expect(recallFormReady({ ...filled, repeatPassword: "long enough!" })).toBe(false);
    expect(recallFormReady({ ...filled, repeatPassword: "" })).toBe(false);
  });

  it("puts no rule on the old password but that it is typed", () => {
    expect(recallFormReady({ ...filled, oldPassword: "x" })).toBe(true);
  });
});

describe("the screen", () => {
  it("starts on the question, checking, with No disabled", () => {
    expect(RECALL_SCREEN_START).toEqual({ step: "question", check: "checking" });
    expect(recallNoEnabled(RECALL_SCREEN_START)).toBe(false);
  });

  it("enables No only once the server confirmed it is possible", () => {
    const open = run([{ type: "checked", check: OPEN }]);
    expect(open).toEqual({ step: "question", check: OPEN });
    expect(recallNoEnabled(open)).toBe(true);
    // Tapping No before that does nothing.
    expect(run([{ type: "no" }])).toEqual(RECALL_SCREEN_START);
  });

  it("offline: the question stays, No disabled, and a later check opens it", () => {
    const offline = run([{ type: "checked", check: { kind: "unknown", offline: true } }]);
    expect(offline).toEqual({ step: "question", check: { kind: "unknown", offline: true } });
    expect(recallNoEnabled(offline)).toBe(false);
    expect(recallReduce(offline, { type: "no" })).toEqual(offline);
    expect(recallNoEnabled(recallReduce(offline, { type: "checked", check: OPEN }))).toBe(true);
  });

  it("closed on the question ends it, and nothing says why", () => {
    expect(run([{ type: "checked", check: { kind: "closed" } }])).toEqual({ step: "ending", notice: null });
    expect(run([{ type: "checked", check: { kind: "exhausted" } }])).toEqual({ step: "ending", notice: null });
    expect(run([{ type: "expired" }])).toEqual({ step: "ending", notice: null });
  });

  it("Yes asks once inside the screen; Go back returns; Delete and sign in ends it", () => {
    const asked = run([{ type: "checked", check: OPEN }, { type: "yes" }]);
    expect(asked).toEqual({ step: "confirm-yes", check: OPEN });
    expect(recallReduce(asked, { type: "yes-back" })).toEqual({ step: "question", check: OPEN });
    expect(recallReduce(asked, { type: "yes-confirmed" })).toEqual({ step: "ending", notice: null });
    // Not confirmed from the question itself.
    expect(run([{ type: "yes-confirmed" }])).toEqual(RECALL_SCREEN_START);
  });

  it("Yes is available while checking and offline: it sends nothing", () => {
    expect(run([{ type: "yes" }, { type: "yes-confirmed" }])).toEqual({ step: "ending", notice: null });
    expect(
      run([{ type: "checked", check: { kind: "unknown", offline: true } }, { type: "yes" }]).step,
    ).toBe("confirm-yes");
  });

  const form = run([{ type: "checked", check: OPEN }, { type: "no" }]);

  it("No opens the form against the confirmed anchor; Back returns to the question", () => {
    expect(form).toEqual({ step: "form", status: STATUS, working: false, error: null });
    expect(recallReduce(form, { type: "form-back" })).toEqual({ step: "question", check: OPEN });
  });

  it("working: submitted once, Back disabled", () => {
    const working = recallReduce(form, { type: "submitted" });
    expect(working).toEqual({ step: "form", status: STATUS, working: true, error: null });
    expect(recallReduce(working, { type: "submitted" })).toEqual(working);
    expect(recallReduce(working, { type: "form-back" })).toEqual(working);
  });

  const failed = (failure: Parameters<typeof recallAttemptFailure>[0], check: RecallCheck | null): RecallScreen =>
    run(
      [{ type: "submitted" }, { type: "attempt-failed", failure: recallAttemptFailure(failure), check }],
      form,
    );
  const wrong = { wrongPasswordLocally: true, status: null, errorCode: null, retryAfterSecs: null };

  it("wrong password: the line, and the tries left only below five", () => {
    const many = failed(wrong, { kind: "open", status: { ...STATUS, attemptsRemaining: 19 } });
    expect(many).toMatchObject({ step: "form", working: false, error: { kind: "wrong-password", triesLeft: null } });
    const five = failed(wrong, { kind: "open", status: { ...STATUS, attemptsRemaining: 5 } });
    expect(five).toMatchObject({ error: { kind: "wrong-password", triesLeft: null } });
    const four = failed(wrong, { kind: "open", status: { ...STATUS, attemptsRemaining: 4 } });
    expect(four).toMatchObject({
      step: "form",
      status: { attemptsRemaining: 4 },
      error: { kind: "wrong-password", triesLeft: 4 },
    });
    const one = failed(wrong, { kind: "open", status: { ...STATUS, attemptsRemaining: 1 } });
    expect(one).toMatchObject({ error: { kind: "wrong-password", triesLeft: 1 } });
  });

  it("wrong password with the count unknown still says wrong password", () => {
    expect(failed(wrong, null)).toMatchObject({ step: "form", error: { kind: "wrong-password", triesLeft: null } });
    expect(failed(wrong, { kind: "unknown", offline: true })).toMatchObject({
      step: "form",
      error: { kind: "wrong-password", triesLeft: null },
    });
  });

  it("no more tries: the exhausted line, then the wipe finishes", () => {
    expect(failed(wrong, { kind: "exhausted" })).toEqual({ step: "ending", notice: "exhausted" });
    // The anchor was destroyed by the last wrong password.
    expect(failed(wrong, { kind: "closed" })).toEqual({ step: "ending", notice: "exhausted" });
  });

  it("not possible any more while the form was open: the closed line, then the wipe finishes", () => {
    expect(
      failed({ wrongPasswordLocally: false, status: 401, errorCode: "recall-refused", retryAfterSecs: null }, null),
    ).toEqual({ step: "ending", notice: "closed" });
    expect(recallReduce(form, { type: "expired" })).toEqual({ step: "ending", notice: "closed" });
    expect(recallReduce(form, { type: "checked", check: { kind: "closed" } })).toEqual({
      step: "ending",
      notice: "closed",
    });
    expect(recallReduce(form, { type: "checked", check: { kind: "exhausted" } })).toEqual({
      step: "ending",
      notice: "exhausted",
    });
  });

  it("rate limited, offline and an error keep the form and the anchor", () => {
    expect(
      failed({ wrongPasswordLocally: false, status: 429, errorCode: "too-many-attempts", retryAfterSecs: 600 }, null),
    ).toEqual({
      step: "form",
      status: STATUS,
      working: false,
      error: { kind: "rate-limited", retryAfterSecs: 600 },
    });
    expect(
      failed({ wrongPasswordLocally: false, status: null, errorCode: null, retryAfterSecs: null }, null),
    ).toEqual({ step: "form", status: STATUS, working: false, error: { kind: "offline" } });
    expect(
      failed({ wrongPasswordLocally: false, status: 400, errorCode: null, retryAfterSecs: null }, null),
    ).toEqual({ step: "form", status: STATUS, working: false, error: { kind: "error" } });
  });

  it("a new submit clears the error line", () => {
    const again = recallReduce(failed(wrong, null), { type: "submitted" });
    expect(again).toMatchObject({ step: "form", working: true, error: null });
  });

  it("lands only from a submitted form, and nothing moves a finished screen", () => {
    const landed = run([{ type: "submitted" }, { type: "landed" }], form);
    expect(landed).toEqual({ step: "landed" });
    expect(run([{ type: "landed" }])).toEqual(RECALL_SCREEN_START);
    const events: RecallEvent[] = [
      { type: "checked", check: OPEN },
      { type: "expired" },
      { type: "yes" },
      { type: "no" },
      { type: "submitted" },
      { type: "form-back" },
    ];
    for (const event of events) {
      expect(recallReduce(landed, event)).toEqual(landed);
      expect(recallReduce({ step: "ending", notice: "closed" }, event)).toEqual({
        step: "ending",
        notice: "closed",
      });
    }
  });

  it("the form never loses its anchor to an unanswered check", () => {
    expect(recallReduce(form, { type: "checked", check: { kind: "unknown", offline: true } })).toEqual(form);
    const fewer = { ...STATUS, attemptsRemaining: 3 };
    expect(recallReduce(form, { type: "checked", check: { kind: "open", status: fewer } })).toMatchObject({
      step: "form",
      status: fewer,
    });
  });
});

describe("account deletion while the account can be taken back", () => {
  it("reads the date of a 409 recall-window-open", () => {
    expect(deleteBlockedUntilMs(409, JSON.stringify({ error: "recall-window-open", until_ms: NOW }))).toBe(NOW);
  });

  it("is not blocked by anything else", () => {
    expect(deleteBlockedUntilMs(403, JSON.stringify({ error: "recall-window-open", until_ms: NOW }))).toBeNull();
    expect(deleteBlockedUntilMs(409, JSON.stringify({ error: "something-else", until_ms: NOW }))).toBeNull();
    expect(deleteBlockedUntilMs(409, JSON.stringify({ error: "recall-window-open" }))).toBeNull();
    expect(deleteBlockedUntilMs(409, JSON.stringify({ error: "recall-window-open", until_ms: "soon" }))).toBeNull();
    expect(deleteBlockedUntilMs(409, "not json")).toBeNull();
    expect(deleteBlockedUntilMs(204, "")).toBeNull();
  });
});

// ADR-0067 section 8 (UX 19.7): taking the account back with the phrase.
describe("the phrase arm", () => {
  it("reads the arm and the account id from the status answer", () => {
    const id = "A".repeat(22);
    expect(parseRecallStatus(body({ phrase_arm: true, account_id_b64: id }))).toMatchObject({
      phraseArm: true,
      accountIdB64Url: id,
    });
    // An older server says neither: no arm, no id.
    expect(parseRecallStatus(body())).toMatchObject({ phraseArm: false, accountIdB64Url: null });
    // An account id that is not 16 bytes of URL-safe base64 is not taken.
    expect(parseRecallStatus(body({ account_id_b64: "short" }))?.accountIdB64Url).toBeNull();
  });

  it("is offered only when the server said so for this anchor", () => {
    expect(phraseRecallOffered({ kind: "open", status: { ...STATUS, phraseArm: true } })).toBe(true);
    // A mail-protected account: whoever came in by email holds the same words.
    expect(phraseRecallOffered(OPEN)).toBe(false);
    expect(phraseRecallOffered({ kind: "closed" })).toBe(false);
    expect(phraseRecallOffered({ kind: "unknown", offline: true })).toBe(false);
    expect(phraseRecallOffered(null)).toBe(false);
  });

  it("the 12 words stand in for the old password on the form", () => {
    const words = "a b c d e f g h i j k l";
    const base = { oldPassword: "", newPassword: "longenough1", repeatPassword: "longenough1" };
    expect(recallFormReady({ ...base, phrase: words })).toBe(true);
    expect(recallFormReady({ ...base, phrase: "a b c" })).toBe(false);
    // Without the phrase arm the old password is still required.
    expect(recallFormReady(base)).toBe(false);
    expect(recallFormReady({ ...base, phrase: null })).toBe(false);
    expect(recallFormReady({ ...base, phrase: words, repeatPassword: "different12" })).toBe(false);
  });

  it("Get back in asks about a reset only when the words were refused as not the account's", () => {
    expect(asksPhraseRecall({ status: 401, offline: false, localFailure: false })).toBe(true);
    // The OPAQUE exchange could not finish on this device: a wrong phrase.
    expect(asksPhraseRecall({ status: null, offline: false, localFailure: true })).toBe(true);
    // A limit, a server error, no connection: nothing is asked.
    expect(asksPhraseRecall({ status: 429, offline: false, localFailure: false })).toBe(false);
    expect(asksPhraseRecall({ status: 503, offline: false, localFailure: false })).toBe(false);
    expect(asksPhraseRecall({ status: null, offline: true, localFailure: false })).toBe(false);
    expect(asksPhraseRecall({ status: null, offline: false, localFailure: false })).toBe(false);
  });
});
