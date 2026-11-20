// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { bytesToB64, bytesToHex } from "./bytes";
import {
  confirmationSentence,
  decideBody,
  decideOutcome,
  canResend,
  closesNoCase,
  decisionOptions,
  keepSuspensionOption,
  liftOption,
  outcomeSentence,
  resendDeadlineMs,
  statementInputs,
  suspendUntilMs,
  type DecisionOption,
} from "./decisions";
import { ACTIONS, REASONS } from "./names";

const ACCOUNT = "ab".repeat(32);
const CHANNEL = "cd".repeat(16);
const GROUP = "ef".repeat(64);
const NAME = "12".repeat(64);
const id16 = (v: number): Uint8Array => new Uint8Array(16).fill(v);

function option(list: readonly DecisionOption[], id: string): DecisionOption {
  const o = list.find((x) => x.id === id);
  if (o === undefined) throw new Error(`no option ${id}`);
  return o;
}

describe("decisionOptions", () => {
  it("offers an account no action, four suspensions, and nothing about a username it does not know", () => {
    const ids = decisionOptions("account", ACCOUNT, null).map((o) => o.id);
    expect(ids).toEqual(["no-action", "suspend-1d", "suspend-7d", "suspend-30d", "suspend-none"]);
  });

  it("offers Take back @username for each lookup key, targeting the name and closing the account case", () => {
    const key = new Uint8Array(64).fill(7);
    const list = decisionOptions("account", ACCOUNT, {
      has_username: true,
      in_everyone: false,
      name_lookup_keys_b64: [bytesToB64(key)],
    });
    const tb = option(list, "take-back-0");
    expect(tb.label).toBe("Take back @username");
    expect(tb.action).toBe("take-back-name");
    expect(tb.targetKind).toBe("name");
    expect(tb.targetHex).toBe(bytesToHex(key));
    expect(tb.closesCase).toEqual({ kind: "account", targetHex: ACCOUNT });
  });

  it("numbers several usernames", () => {
    const k = bytesToB64(new Uint8Array(64));
    const list = decisionOptions("account", ACCOUNT, { has_username: true, in_everyone: true, name_lookup_keys_b64: [k, k] });
    expect(list.filter((o) => o.action === "take-back-name").map((o) => o.label)).toEqual([
      "Take back @username (1 of 2)",
      "Take back @username (2 of 2)",
    ]);
  });

  it("offers a channel the three channel actions and a group the two group actions", () => {
    expect(decisionOptions("channel", CHANNEL, null).map((o) => o.action)).toEqual([
      "no-action",
      "channel-delist",
      "channel-block-handle",
      "channel-remove",
    ]);
    expect(decisionOptions("group-handle", GROUP, null).map((o) => o.action)).toEqual([
      "no-action",
      "group-delist",
      "group-block-handle",
    ]);
    expect(decisionOptions("name", NAME, null).map((o) => o.action)).toEqual(["no-action", "take-back-name"]);
  });

  it("offers nothing for an unknown kind", () => {
    expect(decisionOptions("planet", ACCOUNT, null)).toEqual([]);
  });

  it("every offered action targets the kind the action requires", () => {
    const all = [
      ...decisionOptions("account", ACCOUNT, { has_username: true, in_everyone: true, name_lookup_keys_b64: [bytesToB64(new Uint8Array(64))] }),
      ...decisionOptions("channel", CHANNEL, null),
      ...decisionOptions("group-handle", GROUP, null),
      ...decisionOptions("name", NAME, null),
      liftOption(ACCOUNT),
    ];
    for (const o of all) {
      if (o.action === "no-action") continue;
      expect(ACTIONS.find((a) => a.name === o.action)?.target).toBe(o.targetKind);
    }
  });
});

describe("suspendUntilMs", () => {
  it("adds whole days and gives 0 for no end", () => {
    expect(suspendUntilMs("1d", 1000)).toBe(1000 + 86_400_000);
    expect(suspendUntilMs("7d", 0)).toBe(7 * 86_400_000);
    expect(suspendUntilMs("30d", 0)).toBe(30 * 86_400_000);
    expect(suspendUntilMs("none", 5)).toBe(0);
  });
});

describe("confirmationSentence", () => {
  const list = decisionOptions("account", ACCOUNT, null);
  it("names the action, length and reason, and that it is published", () => {
    expect(confirmationSentence(option(list, "suspend-7d"), 1)).toBe(
      "Suspend this account for 7 days for spam or a scam. This will be published in the moderation log.",
    );
    expect(confirmationSentence(option(list, "suspend-none"), 2)).toBe(
      "Suspend this account with no end date for harassment or threats. This will be published in the moderation log.",
    );
  });

  it("says no action publishes nothing", () => {
    const s = confirmationSentence(option(list, "no-action"), 8);
    expect(s).toContain("no action for another reason");
    expect(s).toContain("Nothing is published.");
    expect(s).not.toContain("moderation log");
  });

  it("has a sentence ending in the log line for every signed decision", () => {
    const all = [
      ...decisionOptions("channel", CHANNEL, null),
      ...decisionOptions("group-handle", GROUP, null),
      ...decisionOptions("name", NAME, null),
      liftOption(ACCOUNT),
    ].filter((o) => o.action !== "no-action");
    for (const o of all) {
      const s = confirmationSentence(o, 3);
      expect(s).toMatch(/ for hate\b/);
      expect(s.endsWith("This will be published in the moderation log.")).toBe(true);
    }
  });

  it("uses a phrase for every reason", () => {
    for (const r of REASONS) {
      expect(confirmationSentence(option(list, "suspend-1d"), r.code)).toContain(`for 1 day for ${r.phrase}.`);
    }
  });
});

describe("statementInputs", () => {
  const base = {
    reasonCode: 1,
    nowMs: 1_700_000_000_000,
    moderatorId: id16(1),
    listVersion: 3,
    statementId: id16(2),
    reasonSalt: id16(3),
  };

  it("maps a 7-day suspension to the action byte, kind byte, target and end", () => {
    const opt = option(decisionOptions("account", ACCOUNT, null), "suspend-7d");
    const s = statementInputs({ ...base, option: opt });
    expect(s.action).toBe(1);
    expect(s.targetKind).toBe(1);
    expect(bytesToHex(s.target)).toBe(ACCOUNT);
    expect(s.reason).toBe(1);
    expect(s.untilMs).toBe(BigInt(base.nowMs + 7 * 86_400_000));
    expect(s.issuedAtMs).toBe(BigInt(base.nowMs));
    expect(s.listVersion).toBe(3n);
    expect(s.statementId).toBe(base.statementId);
    expect(s.reasonSalt).toBe(base.reasonSalt);
    expect(s.moderatorId).toBe(base.moderatorId);
  });

  it("gives no end for a suspension with no end date and for every other action", () => {
    const none = option(decisionOptions("account", ACCOUNT, null), "suspend-none");
    expect(statementInputs({ ...base, option: none }).untilMs).toBe(0n);
    const remove = option(decisionOptions("channel", CHANNEL, null), "channel-remove");
    const s = statementInputs({ ...base, option: remove });
    expect(s.untilMs).toBe(0n);
    expect(s.action).toBe(9);
    expect(s.targetKind).toBe(3);
    expect(s.target.length).toBe(16);
  });

  it("maps a take-back from an account case to the name kind and its 64-byte key", () => {
    const key = new Uint8Array(64).fill(9);
    const opt = option(
      decisionOptions("account", ACCOUNT, { has_username: true, in_everyone: false, name_lookup_keys_b64: [bytesToB64(key)] }),
      "take-back-0",
    );
    const s = statementInputs({ ...base, option: opt });
    expect(s.action).toBe(3);
    expect(s.targetKind).toBe(4);
    expect(s.target).toEqual(key);
  });

  it("maps a lift and the group actions", () => {
    expect(statementInputs({ ...base, option: liftOption(ACCOUNT) }).action).toBe(2);
    const g = decisionOptions("group-handle", GROUP, null);
    expect(statementInputs({ ...base, option: option(g, "group-delist") })).toMatchObject({ action: 11, targetKind: 2 });
    expect(statementInputs({ ...base, option: option(g, "group-block-handle") })).toMatchObject({ action: 12, targetKind: 2 });
  });

  it("refuses no action, a missing reason, a wrong-length target and short ids", () => {
    const list = decisionOptions("account", ACCOUNT, null);
    expect(() => statementInputs({ ...base, option: option(list, "no-action") })).toThrow("No action isn't signed.");
    expect(() => statementInputs({ ...base, option: option(list, "suspend-1d"), reasonCode: 0 })).toThrow("Choose a reason.");
    expect(() => statementInputs({ ...base, option: option(list, "suspend-1d"), reasonCode: 9 })).toThrow("Choose a reason.");
    const short: DecisionOption = { ...option(list, "suspend-1d"), targetHex: "ab".repeat(16) };
    expect(() => statementInputs({ ...base, option: short })).toThrow("isn't valid");
    expect(() => statementInputs({ ...base, option: option(list, "suspend-1d"), statementId: id16(1).slice(0, 15) })).toThrow();
  });

  it("refuses an action on a target of another kind", () => {
    const wrong: DecisionOption = { id: "x", label: "x", action: "suspend", targetKind: "channel", targetHex: CHANNEL };
    expect(() => statementInputs({ ...base, option: wrong })).toThrow("doesn't apply");
  });
});

describe("decideBody", () => {
  it("carries the statement, reason code and salt, and leaves out an empty note and an own-target case", () => {
    const opt = option(decisionOptions("account", ACCOUNT, null), "suspend-1d");
    const body = decideBody({ signedStatement: new Uint8Array([1, 2, 3]), reasonCode: 5, reasonSalt: id16(4), note: "  ", option: opt });
    expect(body).toEqual({ signed_statement_b64: "AQID", reason: 5, reason_salt_b64: bytesToB64(id16(4)) });
  });

  it("trims a note and names the account case a take-back closes", () => {
    const key = bytesToB64(new Uint8Array(64));
    const opt = option(
      decisionOptions("account", ACCOUNT, { has_username: true, in_everyone: false, name_lookup_keys_b64: [key] }),
      "take-back-0",
    );
    const body = decideBody({ signedStatement: new Uint8Array(1), reasonCode: 7, reasonSalt: id16(0), note: " repeat offender ", option: opt });
    expect(body.note).toBe("repeat offender");
    expect(body.case).toEqual({ kind: "account", target_hex: ACCOUNT });
  });
});

describe("decideOutcome", () => {
  it("classifies applied, logged only, already in effect, refused and failed", () => {
    expect(decideOutcome(200, { reports_closed: 2, forwarded: true, applied: true }).kind).toBe("applied");
    expect(decideOutcome(200, { reports_closed: 2, forwarded: false, applied: false }).kind).toBe("logged-only");
    expect(decideOutcome(200, { reports_closed: 0, forwarded: true, applied: false, superseded: true })).toEqual({
      kind: "no-change",
      closed: 0,
      superseded: true,
      noPendingAppeal: false,
    });
    expect(decideOutcome(422, { reports_closed: 1, detail: "unknown-account" })).toEqual({
      kind: "not-applied",
      closed: 1,
      detail: "unknown-account",
      failed: false,
    });
    expect(decideOutcome(502, { reports_closed: 1 })).toMatchObject({ kind: "not-applied", failed: true });
  });

  it("tells the moderator a logged but unapplied decision can be sent again", () => {
    expect(outcomeSentence(decideOutcome(502, { reports_closed: 1 }))).toContain("sent again for 24 hours");
    expect(outcomeSentence(decideOutcome(200, { reports_closed: 3, forwarded: true, applied: true }))).toBe(
      "Published in the moderation log and applied. 3 reports closed.",
    );
  });
});

describe("keeping a suspension on appeal", () => {
  it("is a signed keep-suspension statement on the account (byte 15), not an unsigned decline", () => {
    const opt = keepSuspensionOption(ACCOUNT);
    expect(opt.action).toBe("keep-suspension");
    const inputs = statementInputs({
      option: opt,
      reasonCode: 1,
      nowMs: 1_000,
      moderatorId: id16(1),
      listVersion: 3,
      statementId: id16(2),
      reasonSalt: id16(3),
    });
    expect(inputs.action).toBe(15);
    expect(inputs.targetKind).toBe(1);
    expect(bytesToHex(inputs.target)).toBe(ACCOUNT);
    expect(inputs.untilMs).toBe(0n);
    const body = decideBody({ signedStatement: new Uint8Array([9]), reasonCode: 1, reasonSalt: id16(3), note: "", option: opt });
    expect(body).toEqual({ signed_statement_b64: "CQ==", reason: 1, reason_salt_b64: bytesToB64(id16(3)) });
  });

  it("names the decision and says it is published", () => {
    expect(confirmationSentence(keepSuspensionOption(ACCOUNT), 1)).toBe(
      "Keep this account's suspension for spam or a scam and decline the appeal. The person sees that their appeal was declined. This will be published in the moderation log.",
    );
  });

  it("reads a kept suspension and one with no appeal waiting", () => {
    expect(
      outcomeSentence(decideOutcome(200, { reports_closed: 0, forwarded: true, applied: true }), { action: "keep-suspension" }),
    ).toContain("The suspension stays");
    const none = decideOutcome(200, { reports_closed: 0, forwarded: true, applied: false, no_pending_appeal: true });
    expect(none).toMatchObject({ kind: "no-change", noPendingAppeal: true });
    expect(outcomeSentence(none, { action: "keep-suspension" })).toContain("no appeal was waiting");
  });
});

describe("which decisions close a case", () => {
  it("never promises closed reports for an undo or a file deletion", () => {
    for (const a of ["lift", "release-name", "show-in-search", "channel-restore", "group-restore", "blob-delete", "keep-suspension"]) {
      expect(closesNoCase(a)).toBe(true);
    }
    expect(closesNoCase("suspend")).toBe(false);
    expect(closesNoCase("take-back-name")).toBe(false);
    const lifted = outcomeSentence(decideOutcome(200, { reports_closed: 0, forwarded: true, applied: true }), { action: "lift" });
    expect(lifted).toBe("Published in the moderation log and applied.");
    expect(lifted).not.toContain("report");
  });
});

describe("sending a decision again", () => {
  it("is possible for 24 hours after signing and says until when", () => {
    const issued = Date.UTC(2026, 10, 20, 10, 0);
    expect(resendDeadlineMs(issued)).toBe(issued + 86_400_000);
    expect(canResend(issued, issued + 86_400_000)).toBe(true);
    expect(canResend(issued, issued + 86_400_001)).toBe(false);
    const s = outcomeSentence(decideOutcome(502, { reports_closed: 1 }), { action: "suspend", issuedAtMs: issued });
    expect(s).toContain("until 2026-11-21 10:00 UTC");
    expect(s).toContain("sign a new decision");
  });

  it("words a service's refusal code instead of showing it", () => {
    const s = outcomeSentence(decideOutcome(422, { reports_closed: 0, detail: "stale-statement" }), { action: "suspend" });
    expect(s).toContain("signed more than 24 hours ago");
    expect(s).not.toContain("stale-statement");
  });
});
