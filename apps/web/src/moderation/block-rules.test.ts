// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The block routing decisions against the REAL shared rule (wasm
 * `blockVerdict`), so a change in Rust shows up here as a failing test rather
 * than a path that silently stops dropping.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { entriesFromFlat } from "./block-registry";
import {
  blockVerdictFor,
  dropsAtReceipt,
  receiveDecision,
  receiveKindOf,
  withholdsSignal,
} from "./block-rules";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const blocked = (k: string | null | undefined): boolean => k === "B";

describe("the rule's answers", () => {
  it("shows everything from someone not blocked, without asking the rule", () => {
    expect(blockVerdictFor(null, "one-to-one-message", false)).toBe("show");
    expect(blockVerdictFor(wasm, "group-message", false)).toBe("show");
  });

  it("names one verdict per path for a blocked person", () => {
    expect(blockVerdictFor(wasm, "one-to-one-message", true)).toBe("drop");
    expect(blockVerdictFor(wasm, "group-message", true)).toBe("collapse");
    expect(blockVerdictFor(wasm, "contact-request", true)).toBe("drop");
    expect(blockVerdictFor(wasm, "incoming-call", true)).toBe("decline-silently");
    expect(blockVerdictFor(wasm, "call-tile", true)).toBe("label");
    expect(blockVerdictFor(wasm, "notification", true)).toBe("drop");
    expect(blockVerdictFor(wasm, "outgoing-signal", true)).toBe("withhold");
    expect(blockVerdictFor(wasm, "incoming-signal", true)).toBe("drop");
  });

  it("fails toward showing when the rule cannot be asked", () => {
    expect(blockVerdictFor(null, "one-to-one-message", true)).toBe("show");
    const broken = {
      blockVerdict: (): string => {
        throw new Error("not loaded");
      },
    };
    expect(blockVerdictFor(broken, "one-to-one-message", true)).toBe("show");
    expect(blockVerdictFor({ blockVerdict: () => "something-new" }, "group-message", true)).toBe("show");
  });
});

describe("the receive decision", () => {
  it("drops a blocked person's message, edit, reaction, typing and receipt in the one-to-one", () => {
    for (const event of [
      { kind: "application" },
      { kind: "op", op: "edit" },
      { kind: "op", op: "reaction" },
      { kind: "op", op: "typing" },
      { kind: "op", op: "read-receipt" },
      { kind: "profile" },
      { kind: "profileRequest" },
    ]) {
      expect(dropsAtReceipt(wasm, "one-to-one", { ...event, senderKey: "B" }, blocked)).toBe(true);
    }
  });

  it("keeps a blocked person's group message (it is collapsed when drawn)", () => {
    expect(dropsAtReceipt(wasm, "group", { kind: "application", senderKey: "B" }, blocked)).toBe(false);
    expect(dropsAtReceipt(wasm, "group", { kind: "op", op: "typing", senderKey: "B" }, blocked)).toBe(false);
  });

  it("keeps anything from someone else, and service rows from anyone", () => {
    expect(dropsAtReceipt(wasm, "one-to-one", { kind: "application", senderKey: "C" }, blocked)).toBe(false);
    expect(dropsAtReceipt(wasm, "one-to-one", { kind: "pushRoutingSecret", senderKey: "B" }, blocked)).toBe(false);
    expect(dropsAtReceipt(wasm, "one-to-one", { kind: "callSignaling", senderKey: "B" }, blocked)).toBe(false);
  });

  it("classifies kinds", () => {
    expect(receiveKindOf("application")).toBe("message");
    expect(receiveKindOf("op", "delete")).toBe("message");
    expect(receiveKindOf("op", "typing")).toBe("signal");
    expect(receiveKindOf("system")).toBeNull();
    expect(receiveDecision(wasm, "one-to-one", "message", false)).toBe("keep");
  });
});

describe("withholding signals", () => {
  it("withholds only in the one-to-one with a blocked person", () => {
    expect(withholdsSignal(wasm, "one-to-one", "B", blocked)).toBe(true);
    expect(withholdsSignal(wasm, "one-to-one", "C", blocked)).toBe(false);
    expect(withholdsSignal(wasm, "group", "B", blocked)).toBe(false);
    expect(withholdsSignal(wasm, "one-to-one", null, blocked)).toBe(false);
  });
});

describe("the register read", () => {
  it("splits the flat keys and keeps the times in order", () => {
    const flat = new Uint8Array(64);
    flat.fill(1, 0, 32);
    flat.fill(2, 32, 64);
    const entries = entriesFromFlat(flat, new BigUint64Array([20n, 10n]), (b) => String(b[0]));
    expect(entries).toEqual([
      { keyB64: "1", blockedAtMs: 20 },
      { keyB64: "2", blockedAtMs: 10 },
    ]);
    expect(entriesFromFlat(new Uint8Array(31), [], () => "x")).toEqual([]);
  });

  it("round-trips a block through the real synced state", () => {
    const state = new wasm.SyncedState();
    const key = new Uint8Array(32).fill(9);
    const envelope = state.setBlocked(1n, key, true, 1000n);
    expect(envelope).toBeInstanceOf(Uint8Array);
    expect(wasm.isBlockedEnvelope(envelope as Uint8Array)).toBe(true);
    expect(state.isBlocked(key)).toBe(true);
    const other = new wasm.SyncedState();
    expect(other.applyBlockedEnvelope(envelope as Uint8Array)).toBe(true);
    expect(other.isBlocked(key)).toBe(true);
    expect(entriesFromFlat(other.blockedAccounts(), other.blockedAccountTimes(), (b) => String(b[0]))).toEqual([
      { keyB64: "9", blockedAtMs: 1000 },
    ]);
    state.free();
    other.free();
  });
});
