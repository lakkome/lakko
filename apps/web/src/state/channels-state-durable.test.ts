// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `channelsActions.persistPublisherGroupDurable` — the fail-closed channel-log
 * write the co-publisher knock runs before it publishes its proposal. It
 * resolves only when the row carrying a provider snapshot taken NOW was
 * written; every other outcome rejects, so the knock is never sent.
 *
 * The store is mocked: node has no IndexedDB, and what is pinned here is the
 * contract between the flush and the store, not the store itself.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const saveChannelLog = vi.fn<(...args: unknown[]) => Promise<void>>();

vi.mock("../channels/channel-log-store", () => ({
  saveChannelLog: (...args: unknown[]) => saveChannelLog(...args),
  loadChannelLog: vi.fn(async () => null),
  deleteChannelLog: vi.fn(async () => undefined),
}));

const DEVICE_KEY = {} as unknown as CryptoKey;

function fakeIdentity(): {
  signaturePublicKey: Uint8Array;
  restoreStorage: ReturnType<typeof vi.fn>;
  snapshotStorage: ReturnType<typeof vi.fn>;
} {
  return {
    signaturePublicKey: new Uint8Array(32).fill(7),
    restoreStorage: vi.fn(),
    snapshotStorage: vi.fn(() => new Uint8Array([1, 2, 3])),
  };
}

async function armed(snapshotProvider: () => Uint8Array | null): Promise<typeof import("./channels-state")> {
  const mod = await import("./channels-state");
  await mod.hydrateChannelsState({
    accountIdB64: "acct",
    deviceKey: DEVICE_KEY,
    identity: fakeIdentity() as never,
    snapshotProvider,
  });
  // The hydration's own write is not what is under test.
  saveChannelLog.mockClear();
  return mod;
}

describe("channelsActions.persistPublisherGroupDurable", () => {
  beforeEach(() => {
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    saveChannelLog.mockReset();
    saveChannelLog.mockResolvedValue(undefined);
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.resetModules();
  });

  it("writes now, durably, with the provider snapshot taken now", async () => {
    let provider = new Uint8Array([9]);
    const mod = await armed(() => provider);
    provider = new Uint8Array([4, 5, 6]);

    await expect(mod.channelsActions.persistPublisherGroupDurable()).resolves.toBeUndefined();

    expect(saveChannelLog).toHaveBeenCalledTimes(1);
    const [account, , payload, options] = saveChannelLog.mock.calls[0] ?? [];
    expect(account).toBe("acct");
    expect(options).toEqual({ durable: true });
    // Base64 of [4, 5, 6]: the snapshot of the provider as it is at the call.
    expect(JSON.stringify(payload)).toContain("BAUG");
  });

  it("rejects when the write fails, and the failure does not poison later writes", async () => {
    const mod = await armed(() => new Uint8Array([1]));
    saveChannelLog.mockRejectedValueOnce(new Error("the local stores of this account are sealed"));

    await expect(mod.channelsActions.persistPublisherGroupDurable()).rejects.toThrow("sealed");
    await expect(mod.channelsActions.persistPublisherGroupDurable()).resolves.toBeUndefined();
  });

  it("rejects when the account identity is gone instead of reusing the last snapshot", async () => {
    let live = true;
    const mod = await armed(() => (live ? new Uint8Array([1]) : null));
    live = false;

    await expect(mod.channelsActions.persistPublisherGroupDurable()).rejects.toBeInstanceOf(
      mod.ChannelLogUnavailableError,
    );
    expect(saveChannelLog).not.toHaveBeenCalled();
  });

  it("rejects when no session armed the store", async () => {
    const mod = await import("./channels-state");

    await expect(mod.channelsActions.persistPublisherGroupDurable()).rejects.toBeInstanceOf(
      mod.ChannelLogUnavailableError,
    );
    expect(saveChannelLog).not.toHaveBeenCalled();
  });
});
