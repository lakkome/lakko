// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.5 / ADR-0058 section 5.2 - the conversation LINEAGE is in the
 * client's backup and comes back on restore.
 *
 * On web the lineage is not a store of its own: it is the write-once
 * register of the synced fold (`SyncedState`), and the backup payload
 * carries that fold's bytes beside the other synced maps
 * (`backup/client.ts` hands `currentSyncedStateBytes()` to
 * `buildBackupPayload`; the restore merges it with `mergeRestoredFold`).
 * These tests pin that the registers survive exactly that path, through the
 * real core: payload build -> parse -> fold, and the merge into a device
 * that already holds state of its own.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { buildLineage } from "../chat/lineage";
import { mergeRestoredFold } from "../devices/sync/synced-projection";
import { groupIdHex } from "../devices/sync/state";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const ROOT = new Uint8Array(32).fill(0xa1);
const SECOND = new Uint8Array(32).fill(0xb2);
const THIRD = new Uint8Array(32).fill(0xc3);

/** The fold of a device whose conversation ROOT continued twice. */
function foldWithLineage(): InstanceType<typeof wasm.SyncedState> {
  const s = new wasm.SyncedState();
  expect(s.setLineage(10n, SECOND, ROOT)).toBe(true);
  expect(s.setLineage(20n, THIRD, ROOT)).toBe(true);
  return s;
}

/** The backup payload exactly as `BackupClient` builds it, parsed back. */
function throughBackup(synced: Uint8Array): Uint8Array {
  const payload = wasm.buildBackupPayload(
    new Uint8Array(16).fill(7),
    1n,
    new Uint8Array(32).fill(9),
    [],
    // Payload v6: no MLS snapshot argument; the group ids take its place.
    new Uint8Array(0),
    synced,
  );
  const parsed = wasm.parseBackupPayload(payload);
  try {
    return parsed.syncedState;
  } finally {
    parsed.free();
  }
}

/** The segments of ROOT's conversation as `chat/lineage.ts` orders them. */
function segmentsOf(fold: InstanceType<typeof wasm.SyncedState>): readonly string[] {
  const registers = fold.lineageSuccessors(ROOT);
  const lineage = buildLineage(
    registers.map((r) => ({
      successorHex: groupIdHex(r.successorGroupId),
      rootHex: groupIdHex(r.rootConversationId),
      hlc: r.hlc,
    })),
  );
  for (const r of registers) r.free();
  return lineage.segmentsOf(groupIdHex(ROOT));
}

describe("the lineage in the backup", () => {
  it("the payload carries every register, with its stamp, and the restored fold reads the same segments", () => {
    const source = foldWithLineage();
    const restored = wasm.SyncedState.fromBytes(throughBackup(source.toBytes()));
    expect(groupIdHex(restored.conversationIdOf(THIRD))).toBe(groupIdHex(ROOT));
    expect(groupIdHex(restored.conversationIdOf(SECOND))).toBe(groupIdHex(ROOT));
    expect(segmentsOf(restored)).toEqual([groupIdHex(ROOT), groupIdHex(SECOND), groupIdHex(THIRD)]);
    const register = restored.lineageRoot(THIRD);
    expect(register?.hlc).toBe(20n);
    register?.free();
    source.free();
    restored.free();
  });

  it("a restore onto a device that holds state of its own gains the lineage and keeps what it had", () => {
    const source = foldWithLineage();
    const backup = wasm.SyncedState.fromBytes(throughBackup(source.toBytes()));
    // The restoring device already muted a chat and knows one register.
    const local = new wasm.SyncedState();
    const other = new Uint8Array(32).fill(0x55);
    local.applyFlags(5n, other, false, true, false, false);
    expect(local.setLineage(10n, SECOND, ROOT)).toBe(true);
    mergeRestoredFold(local, backup);
    expect(segmentsOf(local)).toEqual([groupIdHex(ROOT), groupIdHex(SECOND), groupIdHex(THIRD)]);
    const flags = local.flags(other);
    expect(flags?.muted).toBe(true);
    flags?.free();
    source.free();
    backup.free();
    local.free();
  });

  it("a backup with no synced state restores no lineage", () => {
    expect(throughBackup(new Uint8Array(0)).length).toBe(0);
  });
});
