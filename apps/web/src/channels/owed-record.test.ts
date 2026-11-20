// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it, vi } from "vitest";

import type { DeviceCipher } from "../auth/device-cipher";

// The channel log's durable write, stubbed (no IndexedDB in node). `fail`
// makes the next write refused.
const store = vi.hoisted(() => ({ fail: false, saves: 0 }));
vi.mock("./channel-log-store", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./channel-log-store")>();
  return {
    ...actual,
    loadChannelLog: vi.fn(async () => null),
    saveChannelLog: vi.fn(async () => {
      store.saves += 1;
      if (store.fail) throw new Error("disk full");
    }),
  };
});

import { settleOwedRecord } from "./owed-record";
import { channelsActions, hydrateChannelsState, installPublisherProviderPersister } from "../state/channels-state";
import type { PublisherCommitEntry } from "./publisher-commit-outbox";

function entry(channelIdHex: string): PublisherCommitEntry {
  return {
    id: `e-${channelIdHex}`,
    kind: "approve",
    channelIdHex,
    groupIdHex: "ab".repeat(32),
    targetEpoch: "1",
    serverTargetEpoch: 1,
    commitB64: null,
    welcomeB64: null,
    joinerKeyB64: null,
    replyGrantB64: null,
    proofEpoch: null,
    proofSeedB64: null,
    journalLost: false,
    done: [],
  };
}

beforeAll(async () => {
  // ADR-0090: the device provider the PublisherGroups live in is written
  // beside the channel log; here a stand-in that always lands.
  installPublisherProviderPersister(async () => undefined);
  await hydrateChannelsState({
    accountIdB64: "acct",
    deviceKey: {} as unknown as DeviceCipher,
    identity: {
      signaturePublicKey: new Uint8Array(32).fill(7),
      restoreStorage: () => undefined,
      snapshotStorage: () => new Uint8Array([1]),
    } as never,
    snapshotProvider: () => new Uint8Array([1]),
  });
});

describe("the channel record owed after a commit", () => {
  it("recording a commit makes the record owed, in the same durable write", async () => {
    const id = "55".repeat(16);
    const before = store.saves;
    await channelsActions.recordPublisherCommit(entry(id));
    expect(store.saves).toBe(before + 1);
    expect(channelsActions.accessOf(id).recordOwed).toBe(true);
    await channelsActions.finishPublisherCommit(entry(id).id);
    // The commit's record is gone; the owed record is not.
    expect(channelsActions.pendingPublisherCommits()).toEqual([]);
    expect(channelsActions.accessOf(id).recordOwed).toBe(true);
  });

  it("a failed re-post stays owed and the next pass posts it", async () => {
    const id = "55".repeat(16);
    const failing = vi.fn(async (): Promise<boolean> => {
      throw new Error("offline");
    });
    expect(await settleOwedRecord(id, failing)).toBe("failed");
    expect(channelsActions.accessOf(id).recordOwed).toBe(true);

    const ok = vi.fn(async () => true);
    expect(await settleOwedRecord(id, ok)).toBe("posted");
    expect(ok).toHaveBeenCalledTimes(1);
    expect(channelsActions.accessOf(id).recordOwed).toBeUndefined();

    // Settled: the next pass posts nothing.
    expect(await settleOwedRecord(id, ok)).toBe("none");
    expect(ok).toHaveBeenCalledTimes(1);
  });

  it("nothing to post (no name on this device) settles the mark too", async () => {
    const id = "66".repeat(16);
    channelsActions.updateAccess(id, (r) => ({ ...r, recordOwed: true }));
    expect(await settleOwedRecord(id, async () => false)).toBe("posted");
    expect(channelsActions.accessOf(id).recordOwed).toBeUndefined();
  });

  it("nothing is posted when no record is owed", async () => {
    const post = vi.fn(async () => true);
    expect(await settleOwedRecord("44".repeat(16), post)).toBe("none");
    expect(post).not.toHaveBeenCalled();
  });

  // Last: a refused commit record poisons channel writes for the session.
  it("a commit whose record was refused leaves no owed record behind", async () => {
    const id = "77".repeat(16);
    store.fail = true;
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    await expect(channelsActions.recordPublisherCommit(entry(id))).rejects.toThrow();
    expect(channelsActions.accessOf(id).recordOwed).toBeUndefined();
  });
});
