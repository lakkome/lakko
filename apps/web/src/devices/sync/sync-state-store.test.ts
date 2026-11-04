// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The stored sync row (ADR-0058): the Rust fold's own bytes, plus the folder
 * edits that have not been sent yet. The fold is real here - these tests load
 * the wasm core - so "round-trips" means the state a reload restores is the
 * state that was written, not merely that some bytes came back.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../../wasm/lakko_ffi_wasm";
import {
  decodeSyncState,
  encodeSyncState,
  isEmptySyncState,
  SYNC_STATE_VERSION,
  type SyncStateRow,
} from "./sync-state-store";
import type { StampedEdit } from "../../chat/folders/folder-model";
import { projectSyncedState } from "./synced-projection";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP = new Uint8Array(32).fill(0xab);
const FOLDER_HEX = "11".repeat(16);

const pendingBatch: StampedEdit[] = [
  {
    t: "meta",
    hlc: 5n,
    folderIdHex: FOLDER_HEX,
    name: "Work",
    includeKinds: 2,
    excludeFlags: 4,
  },
  { t: "member", hlc: 6n, folderIdHex: FOLDER_HEX, ref: { kind: 3, id: new Uint8Array([1, 2]) }, state: 1 },
  { t: "pins", hlc: 7n, folderIdHex: FOLDER_HEX, pinned: [{ kind: 2, id: GROUP }] },
  { t: "removed", hlc: 8n, folderIdHex: FOLDER_HEX },
  { t: "order", hlc: 9n, folderIdsHex: [FOLDER_HEX] },
  // ADR-0087 section 9: names the conversation, the register is read at send.
  { t: "conversation-end", hlc: 10n, ref: { kind: 2, id: GROUP } },
];

describe("encode / decode", () => {
  it("round-trips the fold, so a reload restores what was folded", () => {
    const s = new wasm.SyncedState();
    s.applyFlags(9n, GROUP, true, true, false, false);
    s.applyDraft(7n, GROUP, "half a sentence", new Uint8Array([1, 2, 3]));
    s.applyReadCursor(11n, GROUP, new Uint8Array([9, 9]), 1_700_000_000_000n);
    const row: SyncStateRow = { synced: s.toBytes(), pending: [] };
    const back = decodeSyncState(encodeSyncState(row));
    expect(back).not.toBeNull();
    const restored = wasm.SyncedState.fromBytes(back!.synced);
    const p = projectSyncedState(restored);
    const hex = "ab".repeat(32);
    expect(p.flags[hex]).toEqual({
      pinned: true,
      muted: true,
      archived: false,
      unreadOverride: false,
      updatedAtHlc: 9n,
    });
    expect(p.drafts[hex]?.text).toBe("half a sentence");
    expect(p.readCursors[hex]?.readAtMs).toBe(1_700_000_000_000n);
  });

  it("keeps every kind of pending folder edit, stamps exact", () => {
    const back = decodeSyncState(
      encodeSyncState({ synced: new Uint8Array(0), pending: [pendingBatch] }),
    );
    expect(back?.pending).toEqual([pendingBatch]);
  });

  it("keeps a stamp at the top of the u64 range exactly", () => {
    const big = 2n ** 64n - 1n;
    const back = decodeSyncState(
      encodeSyncState({
        synced: new Uint8Array(0),
        pending: [[{ t: "removed", hlc: big, folderIdHex: FOLDER_HEX }]],
      }),
    );
    expect(back?.pending[0]?.[0]?.hlc).toBe(big);
  });
});

describe("what is refused", () => {
  it("a row from the version before the Rust fold", () => {
    expect(decodeSyncState(JSON.stringify({ v: 1, d: {}, f: {}, c: {}, o: null }))).toBeNull();
    expect(SYNC_STATE_VERSION).toBe(2);
  });

  it("anything that is not a row", () => {
    for (const bad of ["", "null", "[]", "{", '{"v":2}', '{"v":2,"s":7}']) {
      expect(decodeSyncState(bad)).toBeNull();
    }
  });

  it("a pending batch with one bad register is dropped whole, the others kept", () => {
    const json = encodeSyncState({ synced: new Uint8Array(0), pending: [pendingBatch, pendingBatch] });
    const parsed = JSON.parse(json) as { p: { s?: number }[][] };
    const first = parsed.p[0];
    if (first !== undefined && first[1] !== undefined) first[1].s = 9;
    const back = decodeSyncState(JSON.stringify(parsed));
    expect(back?.pending).toEqual([pendingBatch]);
  });
});

describe("isEmptySyncState", () => {
  it("is empty with no fold and nothing waiting", () => {
    expect(isEmptySyncState({ synced: new Uint8Array(0), pending: [] })).toBe(true);
    expect(isEmptySyncState({ synced: new Uint8Array(1), pending: [] })).toBe(false);
    expect(isEmptySyncState({ synced: new Uint8Array(0), pending: [pendingBatch] })).toBe(false);
  });
});
