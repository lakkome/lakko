// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Creating a channel makes the backup due, through the real core: the
 * channel-subscription register the owner device writes changes
 * `SyncedState.toBytes()`, and the automatic backup's synced-state check sees
 * it. Seen live 2026-11-14: before the check existed, a channel created on one
 * device never reached a backup until an unrelated change sent one out.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { writeChannelSubscription } from "../channels/channel-subscription-wasm";
import { mergeRestoredFold, mergeRestoredFoldBytes } from "../devices/sync/synced-projection";
import { MemoryStorage } from "../test/fake-storage";
import type { Wasm } from "../wasm";
import { loadAutoBackupState, markedUploaded, saveAutoBackupState, syncedStateChanged, CLEAN_STATE } from "./auto-backup";

const core = wasm as unknown as Wasm;
const ACCOUNT = "c3luY2VkLXN0YXRl";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});
beforeEach(() => {
  (globalThis as unknown as Record<string, unknown>)["localStorage"] = new MemoryStorage();
});
afterEach(() => {
  delete (globalThis as unknown as Record<string, unknown>)["localStorage"];
});

describe("a channel created on this device", () => {
  it("moves the synced state the backup compares, so the backup becomes due", () => {
    const synced = new wasm.SyncedState();
    try {
      // The state as of the last upload: reported, clean.
      expect(syncedStateChanged(ACCOUNT, synced.toBytes())).toBe(true);
      saveAutoBackupState(ACCOUNT, markedUploaded(loadAutoBackupState(ACCOUNT), 1));
      expect(syncedStateChanged(ACCOUNT, synced.toBytes())).toBe(false);

      const written = writeChannelSubscription(synced, core, 5n, "00112233445566778899aabbccddeeff", {
        role: "owner",
        subscriberSecret: new Uint8Array(32).fill(7),
        readKey: new Uint8Array(32).fill(9),
        publisherGroupIdHex: "",
        ownerAccountKey: new Uint8Array(0),
        name: "News",
        handle: "",
      });
      expect(written).not.toBeNull();
      expect(syncedStateChanged(ACCOUNT, synced.toBytes())).toBe(true);
    } finally {
      synced.free();
    }
  });

  it("the key is stable for the same fold", () => {
    const synced = new wasm.SyncedState();
    try {
      expect(syncedStateChanged(ACCOUNT, synced.toBytes())).toBe(true);
      expect(syncedStateChanged(ACCOUNT, synced.toBytes())).toBe(false);
      expect(CLEAN_STATE.lastSyncedStateKey).toBeNull();
    } finally {
      synced.free();
    }
  });
});

const CHANNEL = "00112233445566778899aabbccddeeff";
const OTHER_CHANNEL = "ffeeddccbbaa99887766554433221100";

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function ownerValue(name: string): Parameters<typeof writeChannelSubscription>[4] {
  return {
    role: "owner",
    subscriberSecret: new Uint8Array(32).fill(7),
    readKey: new Uint8Array(32).fill(9),
    publisherGroupIdHex: "",
    ownerAccountKey: new Uint8Array(0),
    name,
    handle: "",
  };
}

/** A backup fold holding an owned channel's register and one admission seed. */
function backupFoldBytes(): Uint8Array {
  const b = new wasm.SyncedState();
  try {
    writeChannelSubscription(b, core, 10n, CHANNEL, ownerValue("News"));
    expect(b.setChannelAdmissionSeed(11n, hexBytes(CHANNEL), 2, new Uint8Array(32).fill(0x33))).toBe(true);
    return b.toBytes();
  } finally {
    b.free();
  }
}

function expectBackupRegisters(fold: InstanceType<Wasm["SyncedState"]>): void {
  const reg = fold.channelSubscription(hexBytes(CHANNEL));
  expect(reg).toBeDefined();
  reg?.free();
  expect(Array.from(fold.channelAdmissionSeedsFor(hexBytes(CHANNEL), 2))).toEqual(Array.from(new Uint8Array(32).fill(0x33)));
}

describe("a restore onto a device that kept its synced state (Android finding, checked on web)", () => {
  it("the merged bytes keep the backup's channel register and admission seed beside the local registers", () => {
    const local = new wasm.SyncedState();
    let merged: Uint8Array | null;
    try {
      writeChannelSubscription(local, core, 5n, OTHER_CHANNEL, ownerValue("Local"));
      merged = mergeRestoredFoldBytes(core, local.toBytes(), backupFoldBytes());
    } finally {
      local.free();
    }
    expect(merged).not.toBeNull();
    const fold = wasm.SyncedState.fromBytes(merged as Uint8Array);
    try {
      expectBackupRegisters(fold);
      const kept = fold.channelSubscription(hexBytes(OTHER_CHANNEL));
      expect(kept).toBeDefined();
      kept?.free();
    } finally {
      fold.free();
    }
  });

  it("the running fold (a restore landing while the runtime is up) folds them in too", () => {
    const running = new wasm.SyncedState();
    const backup = wasm.SyncedState.fromBytes(backupFoldBytes());
    try {
      writeChannelSubscription(running, core, 5n, OTHER_CHANNEL, ownerValue("Local"));
      expect(mergeRestoredFold(running, backup)).toBe(true);
      expectBackupRegisters(running);
    } finally {
      running.free();
      backup.free();
    }
  });

  it("an admission seed set on this device makes the backup due", () => {
    const synced = new wasm.SyncedState();
    try {
      expect(syncedStateChanged(ACCOUNT, synced.toBytes())).toBe(true);
      expect(synced.setChannelAdmissionSeed(3n, hexBytes(CHANNEL), 1, new Uint8Array(32).fill(0x44))).toBe(true);
      expect(syncedStateChanged(ACCOUNT, synced.toBytes())).toBe(true);
    } finally {
      synced.free();
    }
  });
});
