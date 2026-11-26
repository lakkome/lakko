// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0101 sections 1 to 5 - the reset side's window and previous key. */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  RESET_CONTINUITY_WINDOW_MS,
  announcementOwed,
  withAnnounced,
  clearResetContinuity,
  continuityMailboxReadable,
  previousKeyStanding,
  readResetContinuity,
  resetContinuityExpiryMs,
  writeResetContinuity,
} from "./reset-continuity";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

function memory(): { getItem(k: string): string | null; setItem(k: string, v: string): void; removeItem(k: string): void } {
  const m = new Map<string, string>();
  return {
    getItem: (k) => m.get(k) ?? null,
    setItem: (k, v) => void m.set(k, v),
    removeItem: (k) => void m.delete(k),
  };
}

describe("the window", () => {
  it("is the core's, counted from the attestation's signed_at", () => {
    expect(BigInt(RESET_CONTINUITY_WINDOW_MS)).toBe(wasm.continuityWindowMs());
    expect(resetContinuityExpiryMs(5)).toBe(5 + RESET_CONTINUITY_WINDOW_MS);
  });
  it("a binding signed for that expiry verifies (not too far ahead)", () => {
    const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(3));
    const now = 1_700_000_000_000;
    const binding = wasm.signContinuityBinding(id, BigInt(resetContinuityExpiryMs(now)));
    expect(wasm.verifyContinuityBinding(binding, id.signaturePublicKey, BigInt(now)).length).toBe(32);
    id.free();
  });
  it("the mailbox is read strictly before the expiry, and never without a record", () => {
    expect(continuityMailboxReadable({ expiresAtMs: 10 }, 9)).toBe(true);
    expect(continuityMailboxReadable({ expiresAtMs: 10 }, 10)).toBe(false);
    expect(continuityMailboxReadable(null, 0)).toBe(false);
  });
});

describe("the record", () => {
  it("round-trips, and a damaged row reads as absent", () => {
    const s = memory();
    const rec = { v: 1 as const, expiresAtMs: 42, previousKeyB64: "K", keyGeneration: "3", announced: false };
    writeResetContinuity(s, "a", rec);
    expect(readResetContinuity(s, "a")).toEqual(rec);
    expect(announcementOwed(rec, 41)).toBe(true);
    expect(announcementOwed(withAnnounced(rec), 41)).toBe(false);
    expect(announcementOwed(rec, 42)).toBe(false);
    s.setItem("lakko:reset-continuity:c", JSON.stringify({ ...rec, keyGeneration: "x" }));
    expect(readResetContinuity(s, "c")).toBeNull();
    s.setItem("lakko:reset-continuity:b", "{oops");
    expect(readResetContinuity(s, "b")).toBeNull();
    clearResetContinuity(s, "a");
    expect(readResetContinuity(s, "a")).toBeNull();
  });
});

describe("the previous own key", () => {
  it("is the served key when the log confirms it (or no log is pinned)", () => {
    expect(previousKeyStanding({ servedKeyB64: "OLD", logStatus: "confirmed", newKeyB64: "NEW" })).toBe("OLD");
    expect(previousKeyStanding({ servedKeyB64: "OLD", logStatus: "off", newKeyB64: "NEW" })).toBe("OLD");
  });
  it("is none on any other verdict, with nothing served, or when it is the new key", () => {
    for (const logStatus of ["absent", "mismatch", "rollback", "unreachable", null] as const) {
      expect(previousKeyStanding({ servedKeyB64: "OLD", logStatus, newKeyB64: "NEW" })).toBeNull();
    }
    expect(previousKeyStanding({ servedKeyB64: null, logStatus: "confirmed", newKeyB64: "NEW" })).toBeNull();
    expect(previousKeyStanding({ servedKeyB64: "NEW", logStatus: "confirmed", newKeyB64: "NEW" })).toBeNull();
  });
});
