// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { ApiError, coreErrorMessage, errorMessage, isSignedOut } from "../api/errors";
import { b64ToBytes, bytesToB64, bytesToHex, hexToBytes } from "./bytes";
import { appealWarning, formatTime, untilPhrase } from "./format";
import {
  ACTIONS,
  REASONS,
  TARGET_KINDS,
  actionLabel,
  ageBandLabel,
  reasonByCode,
  reasonByName,
  targetKindByByte,
} from "./names";
import { numbersColumns } from "./numbers";
import { moderatorForCredential, prfOutput } from "./webauthn-rules";

describe("the wire vocabularies", () => {
  it("has the eight reasons in sheet order with codes 1..8", () => {
    expect(REASONS.map((r) => [r.name, r.code])).toEqual([
      ["spam", 1],
      ["harassment", 2],
      ["hate", 3],
      ["violence", 4],
      ["sexual", 5],
      ["child-safety", 6],
      ["impersonation", 7],
      ["other", 8],
    ]);
    expect(reasonByCode(6)?.name).toBe("child-safety");
    expect(reasonByName("other")?.code).toBe(8);
  });

  it("has the five target kinds with their bytes and lengths", () => {
    expect(TARGET_KINDS.map((k) => [k.name, k.byte, k.length])).toEqual([
      ["account", 1, 32],
      ["group-handle", 2, 64],
      ["channel", 3, 16],
      ["name", 4, 64],
      ["blob", 5, 32],
    ]);
    expect(targetKindByByte(3)?.name).toBe("channel");
  });

  it("has the fifteen actions with their bytes and target kinds", () => {
    expect(ACTIONS.map((a) => [a.name, a.byte, a.target])).toEqual([
      ["suspend", 1, "account"],
      ["lift", 2, "account"],
      ["take-back-name", 3, "name"],
      ["release-name", 4, "name"],
      ["hide-from-search", 5, "account"],
      ["show-in-search", 6, "account"],
      ["channel-delist", 7, "channel"],
      ["channel-block-handle", 8, "channel"],
      ["channel-remove", 9, "channel"],
      ["channel-restore", 10, "channel"],
      ["group-delist", 11, "group-handle"],
      ["group-block-handle", 12, "group-handle"],
      ["group-restore", 13, "group-handle"],
      ["blob-delete", 14, "blob"],
      ["keep-suspension", 15, "account"],
    ]);
    expect(actionLabel("no-action")).toBe("No action");
  });

  it("words the age bands", () => {
    expect(ageBandLabel("day")).toBe("Under a day old");
    expect(ageBandLabel(null)).toBe("Age unknown");
  });
});

describe("bytes", () => {
  it("round-trips base64 (standard and url-safe) and hex", () => {
    const b = new Uint8Array([0, 251, 255, 16]);
    expect(b64ToBytes(bytesToB64(b))).toEqual(b);
    expect(b64ToBytes("APv_EA")).toEqual(b);
    expect(hexToBytes(bytesToHex(b))).toEqual(b);
    expect(hexToBytes("abc")).toBeNull();
    expect(hexToBytes("zz")).toBeNull();
    expect(hexToBytes("AB")).toEqual(new Uint8Array([171]));
  });
});

describe("errors", () => {
  it("turns API codes into sentences and spots a lost session", () => {
    expect(errorMessage(new ApiError(409, "locked", null))).toBe("Another moderator is reviewing this case.");
    expect(errorMessage(new ApiError(400, "made-up", null))).toContain("(made-up)");
    expect(errorMessage(new ApiError(0, null, null))).toContain("Couldn't reach");
    expect(errorMessage(new ApiError(503, null, null))).toContain("status 503");
    expect(isSignedOut(new ApiError(401, "session-required", null))).toBe(true);
    expect(isSignedOut(new ApiError(401, "bad-assertion", null))).toBe(false);
  });

  it("reads the core's code, never its prose", () => {
    expect(coreErrorMessage(new Error("cannot-open: not sealed to this key"), "report")).toContain("wasn't sealed to your key");
    expect(coreErrorMessage(new Error("malformed: truncated"), "appeal")).toContain("This appeal is damaged");
    expect(coreErrorMessage(new Error("weird"), "statement")).toContain("decision couldn't be processed");
  });
});

describe("format", () => {
  it("writes UTC times and suspension ends", () => {
    expect(formatTime(Date.UTC(2026, 10, 19, 14, 3))).toBe("2026-11-19 14:03 UTC");
    expect(untilPhrase(null)).toBe("with no end date");
    expect(untilPhrase(0)).toBe("with no end date");
    expect(untilPhrase(Date.UTC(2026, 10, 26, 10, 0))).toBe("until 2026-11-26 10:00 UTC");
  });

  it("warns, and only warns, the moderator who decided the appealed suspension", () => {
    expect(appealWarning(true)).toContain("You decided this suspension");
    expect(appealWarning(false)).toBeNull();
  });
});

describe("webauthn rules", () => {
  it("finds the moderator whose credential answered", () => {
    const creds = [
      { moderator_id_b64: "AAAA", credential_id_b64: bytesToB64(new Uint8Array([1, 2])) },
      { moderator_id_b64: "BBBB", credential_id_b64: bytesToB64(new Uint8Array([3, 4])) },
    ];
    expect(moderatorForCredential(new Uint8Array([3, 4]), creds)).toBe("BBBB");
    expect(moderatorForCredential(new Uint8Array([9]), creds)).toBeNull();
  });

  it("accepts only a 32-byte PRF result", () => {
    const out = new Uint8Array(32).fill(5);
    expect(prfOutput({ prf: { results: { first: out.buffer } } })).toEqual(out);
    expect(prfOutput({ prf: { results: { first: out } } })).toEqual(out);
    expect(prfOutput({ prf: { enabled: true } })).toBeNull();
    expect(prfOutput({ prf: { results: { first: new Uint8Array(16).buffer } } })).toBeNull();
    expect(prfOutput({})).toBeNull();
    expect(prfOutput(null)).toBeNull();
  });
});

describe("numbersColumns", () => {
  it("shows only reasons and decisions that occur, in table order, no action last", () => {
    const cols = numbersColumns([
      {
        week: "2026-W47",
        start_day: "2026-11-16",
        reports: { hate: 1, spam: 14 },
        decisions: { "no-action": 2, lift: 1, suspend: 3 },
        appeals: { sent: 2, declined: 1, lifted: 1 },
      },
    ]);
    expect(cols.map((c) => c.label)).toEqual([
      "Reports",
      "Spam",
      "Hate",
      "Decisions",
      "Suspended",
      "Suspension lifted",
      "No action",
      "Appeals",
      "Declined",
      "Lifted",
    ]);
    const w = {
      week: "x",
      start_day: "x",
      reports: { spam: 3 },
      decisions: {},
      appeals: { sent: 0, declined: 0, lifted: 0 },
    };
    expect(cols[0]?.value(w)).toBe(3);
    expect(cols[2]?.value(w)).toBe(0);
  });
});
