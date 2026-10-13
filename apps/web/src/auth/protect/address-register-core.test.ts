// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recovery-email register of the synced state, through the real wasm
 * core, the way this client drives it (ADR-0075, amended 2026-10-13): a
 * write on one device reaches a sibling as a service envelope, survives the
 * encrypted row (`toBytes`) and the backup merge, and a removal wins by its
 * later stamp.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../../wasm/lakko_ffi_wasm";
import { mergeRestoredFold } from "../../devices/sync/synced-projection";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

describe("the recovery-email register", () => {
  it("keeps the normalised address and reads it back after a restart", () => {
    const state = new wasm.SyncedState();
    expect(state.recoveryEmail()).toBeUndefined();
    expect(state.setRecoveryEmail(10n, " Name@Example.COM ")).toBe(true);
    expect(state.recoveryEmail()).toBe("name@example.com");
    const reloaded = wasm.SyncedState.fromBytes(state.toBytes());
    expect(reloaded.recoveryEmail()).toBe("name@example.com");
  });

  it("reaches a sibling device as a service envelope, routed before any other", () => {
    const here = new wasm.SyncedState();
    here.setRecoveryEmail(10n, "name@example.com");
    const envelope = here.recoveryEmailEnvelope();
    expect(envelope).toBeDefined();
    expect(wasm.isRecoveryEmailEnvelope(envelope as Uint8Array)).toBe(true);
    // It is not a lineage envelope: the two routes cannot take each other's.
    expect(wasm.isLineageEnvelope(envelope as Uint8Array)).toBe(false);
    const sibling = new wasm.SyncedState();
    expect(sibling.applyRecoveryEmailEnvelope(envelope as Uint8Array)).toBe(true);
    expect(sibling.recoveryEmail()).toBe("name@example.com");
    // A re-send (the growth catch-up) changes nothing.
    expect(sibling.applyRecoveryEmailEnvelope(envelope as Uint8Array)).toBe(false);
  });

  it("a removal is a write too: it travels, and a later stamp wins", () => {
    const here = new wasm.SyncedState();
    here.setRecoveryEmail(10n, "name@example.com");
    const added = here.recoveryEmailEnvelope() as Uint8Array;
    expect(here.setRecoveryEmail(20n, null)).toBe(true);
    expect(here.recoveryEmail()).toBeUndefined();
    const removed = here.recoveryEmailEnvelope() as Uint8Array;
    const sibling = new wasm.SyncedState();
    sibling.applyRecoveryEmailEnvelope(removed);
    // The older "added" arriving late does not bring the address back.
    expect(sibling.applyRecoveryEmailEnvelope(added)).toBe(false);
    expect(sibling.recoveryEmail()).toBeUndefined();
    expect(sibling.recoveryEmailRegister()?.address).toBeUndefined();
  });

  it("rides the backup: a restore merges it into an empty device", () => {
    const backedUp = new wasm.SyncedState();
    backedUp.setRecoveryEmail(10n, "name@example.com");
    const fresh = new wasm.SyncedState();
    const fold = wasm.SyncedState.fromBytes(backedUp.toBytes());
    expect(mergeRestoredFold(fresh, fold)).toBe(true);
    expect(fresh.recoveryEmail()).toBe("name@example.com");
  });

  it("refuses an address the one rule refuses, and keeps nothing", () => {
    const state = new wasm.SyncedState();
    expect(() => state.setRecoveryEmail(10n, "not an address")).toThrow();
    expect(state.recoveryEmail()).toBeUndefined();
    expect(state.recoveryEmailEnvelope()).toBeUndefined();
  });
});
