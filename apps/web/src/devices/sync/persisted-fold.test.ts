// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A persisted fold this build refuses is "no synced state held", against the
 * REAL core.
 *
 * `SyncedState.toBytes()` opens with an encoding version and
 * `SyncedState.fromBytes` reads exactly one: the saved-messages register
 * (ADR-0065) made it version 2 and the link records (ADR-0079) version 3;
 * an older blob is refused, with no
 * migration. Two places read such bytes back: the sync runtime's seed from
 * this device's own row, and the merge of a restored backup. Both go through
 * `openPersistedFold` / `mergeRestoredFoldBytes`, tested here: a refused blob
 * never throws, is logged once, and leaves the device on an empty fold whose
 * next write replaces the row.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import initWasm, * as wasm from "../../../wasm/lakko_ffi_wasm";
import { stampEdits, type RegisterEdit } from "../../chat/folders/folder-model";
import { decodeSyncState, encodeSyncState } from "./sync-state-store";
import {
  buildFolderChangeSet,
  mergeRestoredFoldBytes,
  openPersistedFold,
  projectSyncedState,
} from "./synced-projection";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

let warn: ReturnType<typeof vi.spyOn>;
beforeEach(() => {
  warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
});
afterEach(() => {
  warn.mockRestore();
});

let clock = 5_000n;
const mint = (): bigint => (clock += 1n);

/** The bytes of a fold holding one named folder. */
function foldWithFolder(idHex: string, name: string): Uint8Array {
  const s = new wasm.SyncedState();
  const edits: RegisterEdit[] = [
    { t: "meta", folderIdHex: idHex, name, includeKinds: 0, excludeFlags: 0 },
  ];
  const cs = buildFolderChangeSet(wasm, stampEdits(edits, mint));
  try {
    s.applyFolderChanges(cs);
    return s.toBytes();
  } finally {
    cs.free();
    s.free();
  }
}

function folderNames(bytes: Uint8Array): string[] {
  const s = wasm.SyncedState.fromBytes(bytes);
  try {
    return projectSyncedState(s)
      .folders.map((f) => f.name)
      .sort();
  } finally {
    s.free();
  }
}

const F1 = "a1".repeat(16);
const F2 = "b2".repeat(16);

/** Version 1 blobs: first byte 0x01, followed by anything. */
function versionOneBlobs(): Uint8Array[] {
  // What a version 1 build would have written for the same content: the
  // current body under the old version byte.
  const relabelled = foldWithFolder(F1, "Work");
  relabelled[0] = 0x01;
  const emptyRelabelled = new wasm.SyncedState().toBytes();
  emptyRelabelled[0] = 0x01;
  return [
    new Uint8Array([0x01]),
    new Uint8Array([0x01, 0x00, 0x00, 0x00, 0x00]),
    new Uint8Array([0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
    Uint8Array.from({ length: 300 }, (_, i) => (i === 0 ? 0x01 : (i * 37) & 0xff)),
    emptyRelabelled,
    relabelled,
  ];
}

describe("the encoding version", () => {
  it("this build writes version 3", () => {
    const s = new wasm.SyncedState();
    expect(s.toBytes()[0]).toBe(0x03);
    s.free();
  });

  it("the core refuses every version 1 blob", () => {
    for (const blob of versionOneBlobs()) {
      expect(() => wasm.SyncedState.fromBytes(blob)).toThrow();
    }
  });
});

describe("openPersistedFold", () => {
  it("opens what this build wrote, without a log line", () => {
    const opened = openPersistedFold(wasm, foldWithFolder(F1, "Work"), "stored");
    expect(opened).not.toBeNull();
    expect(projectSyncedState(opened!).folders.map((f) => f.name)).toEqual(["Work"]);
    opened!.free();
    expect(warn).not.toHaveBeenCalled();
  });

  it("a version 1 blob is none held: null, never a throw, logged once", () => {
    for (const blob of versionOneBlobs()) {
      warn.mockClear();
      expect(openPersistedFold(wasm, blob, "stored")).toBeNull();
      expect(warn).toHaveBeenCalledTimes(1);
      expect(String(warn.mock.calls[0]?.[0])).toBe(
        "stored synced state unreadable; treated as none held:",
      );
    }
  });

  it("the log line is plain ASCII and carries no stored content", () => {
    const blob = foldWithFolder(F1, "Secret folder");
    blob[0] = 0x01;
    expect(openPersistedFold(wasm, blob, "stored")).toBeNull();
    const line = warn.mock.calls[0]?.map(String).join(" ") ?? "";
    // eslint-disable-next-line no-control-regex
    expect(/^[\x20-\x7e]*$/.test(line)).toBe(true);
    expect(line).not.toContain("Secret folder");
  });

  it("an empty or truncated blob is none held too", () => {
    expect(openPersistedFold(wasm, new Uint8Array(0), "stored")).toBeNull();
    expect(openPersistedFold(wasm, new Uint8Array([0x03]), "stored")).toBeNull();
  });

  it("the device carries on from an empty fold, and its next write replaces the row", () => {
    const stale = foldWithFolder(F1, "Work");
    stale[0] = 0x01;
    // The stored row decodes (the row schema is separate from the fold's);
    // only its fold is refused.
    const row = decodeSyncState(encodeSyncState({ synced: stale, pending: [] }));
    expect(row).not.toBeNull();
    const fold = openPersistedFold(wasm, row!.synced, "stored") ?? new wasm.SyncedState();
    expect(projectSyncedState(fold).folders).toEqual([]);

    // The next write is the fresh fold's bytes, which read back.
    const rewritten = decodeSyncState(encodeSyncState({ synced: fold.toBytes(), pending: [] }));
    fold.free();
    expect(rewritten!.synced[0]).toBe(0x03);
    warn.mockClear();
    const reopened = openPersistedFold(wasm, rewritten!.synced, "stored");
    expect(reopened).not.toBeNull();
    reopened!.free();
    expect(warn).not.toHaveBeenCalled();
  });
});

describe("mergeRestoredFoldBytes", () => {
  it("a version 1 backup blob holds none: nothing to write, logged once", () => {
    const local = foldWithFolder(F1, "Local");
    for (const blob of versionOneBlobs()) {
      warn.mockClear();
      expect(mergeRestoredFoldBytes(wasm, local, blob)).toBeNull();
      expect(mergeRestoredFoldBytes(wasm, null, blob)).toBeNull();
      expect(warn).toHaveBeenCalledTimes(2);
      expect(String(warn.mock.calls[0]?.[0])).toBe(
        "backup synced state unreadable; treated as none held:",
      );
    }
    // The stored fold was not touched.
    expect(folderNames(local)).toEqual(["Local"]);
  });

  it("nothing stored: the backup's bytes as they are", () => {
    const backup = foldWithFolder(F2, "Backup");
    expect(mergeRestoredFoldBytes(wasm, null, backup)).toBe(backup);
    expect(warn).not.toHaveBeenCalled();
  });

  it("a version 1 stored row holds nothing to keep: the backup replaces it, logged once", () => {
    const backup = foldWithFolder(F2, "Backup");
    const stale = foldWithFolder(F1, "Local");
    stale[0] = 0x01;
    const out = mergeRestoredFoldBytes(wasm, stale, backup);
    expect(out).toBe(backup);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0]?.[0])).toBe(
      "stored synced state unreadable; treated as none held:",
    );
  });

  it("both readable: the merge of the two, in the current version", () => {
    const out = mergeRestoredFoldBytes(
      wasm,
      foldWithFolder(F1, "Local"),
      foldWithFolder(F2, "Backup"),
    );
    expect(out).not.toBeNull();
    expect(out![0]).toBe(0x03);
    expect(folderNames(out!)).toEqual(["Backup", "Local"]);
    expect(warn).not.toHaveBeenCalled();
  });
});
