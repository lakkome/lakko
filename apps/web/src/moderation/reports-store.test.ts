// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { persistedWithOps } from "../chat/message-ops-store";
import type { PersistedMessage } from "../chat/message-log-store";
import { decodeRootKey, tokenFailureOutcome } from "./report-api";
import { decodeSentReports, SENT_REPORTS_MAX, withSentReport, type SentReport } from "./reports-store";

const report = (id: string, at = 1): SentReport => ({
  receiptId: id.padEnd(22, "a"),
  targetName: "Anna",
  reason: 1,
  sentAtMs: at,
});

describe("Your reports", () => {
  it("keeps the newest first, one row per receipt, within the bound", () => {
    let list: SentReport[] = [];
    list = withSentReport(list, report("x", 1));
    list = withSentReport(list, report("y", 2));
    list = withSentReport(list, report("x", 3));
    expect(list.map((r) => r.sentAtMs)).toEqual([3, 2]);
    for (let i = 0; i < SENT_REPORTS_MAX + 5; i += 1) list = withSentReport(list, report(`r${i}`, i));
    expect(list).toHaveLength(SENT_REPORTS_MAX);
  });

  it("drops a malformed stored row", () => {
    expect(decodeSentReports([report("ok"), { receiptId: "short" }, null, 7])).toEqual([report("ok")]);
    expect(decodeSentReports("nope")).toEqual([]);
  });
});

describe("the pinned root key", () => {
  it("is 32 bytes of base64url or nothing", () => {
    const b64url = btoa(String.fromCharCode(...new Uint8Array(32).fill(250)))
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");
    expect(decodeRootKey(b64url)?.length).toBe(32);
    expect(decodeRootKey("")).toBeNull();
    expect(decodeRootKey(undefined)).toBeNull();
    expect(decodeRootKey("abcd")).toBeNull();
    expect(decodeRootKey("not base64!")).toBeNull();
  });
});

describe("a token that cannot be spent", () => {
  it("is the daily limit for the quota and not available when tokens are off", () => {
    expect(tokenFailureOutcome({ name: "AnonTokenQuotaExhaustedError" })).toEqual({ kind: "limited" });
    expect(tokenFailureOutcome({ name: "AnonTokensDisabledError" })).toEqual({ kind: "unavailable" });
    expect(tokenFailureOutcome(new Error("offline"))).toEqual({ kind: "failed" });
  });
});

describe("a deleted message", () => {
  it("keeps no evidence on disk", () => {
    const m: PersistedMessage = {
      messageId: "5",
      direction: "incoming",
      text: "secret",
      attachments: [],
      createdAt: "2026-11-08T00:00:00Z",
      evidence: { attestation: "verified", plaintextB64: "AAAA", leafKeyB64: "BBBB", bindingB64: null },
    };
    const out = persistedWithOps(m, { deleted: true, held: [], reactions: new Map() } as never);
    expect(out.deleted).toBe(true);
    expect(out.evidence).toBeUndefined();
    expect(out.text).toBe("");
  });
});
