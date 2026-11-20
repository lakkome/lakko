// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Removing a subscriber from a private channel (UX 7.3) takes the removed
 * device off the next epoch. Only the owner's device rotates (maintainer,
 * 2026-11-08): on a co-publisher's device the removal sends the rotate
 * request instead, and the owner's device rotates at its next run.
 */
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { DeviceCipher } from "../auth/device-cipher";

const api = vi.hoisted(() => ({ removeSubscriberAsPublisher: vi.fn(async () => undefined) }));
vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();
  return { ...actual, removeSubscriberAsPublisher: api.removeSubscriberAsPublisher };
});
// The channel log's durable write, stubbed (no IndexedDB in node); `log`
// records the order of durable writes and requests.
const log = vi.hoisted(() => ({ events: [] as string[] }));
vi.mock("./channel-log-store", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./channel-log-store")>();
  return {
    ...actual,
    loadChannelLog: vi.fn(async () => null),
    saveChannelLog: vi.fn(async () => {
      log.events.push("saved");
    }),
  };
});

import {
  installChannelRotateNow,
  installChannelRotateRequest,
  removeChannelSubscriber,
  settleOwedRotateRequest,
} from "./channel-access";
import { channelsActions, hydrateChannelsState } from "../state/channels-state";
import type { WasmIdentity } from "./composer";

beforeAll(async () => {
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

const KEYS = { seed: new Uint8Array(32).fill(1), signingKeySeed: new Uint8Array(32).fill(2), seedEpoch: 0 };
const OWNED = "11".repeat(16);
const CO = "22".repeat(16);
const identity = {} as WasmIdentity;
const rotateNow = vi.fn(async () => undefined);
const request = vi.fn(async () => {
  log.events.push("request");
});

beforeEach(() => {
  rotateNow.mockClear();
  request.mockClear();
  log.events.length = 0;
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
  api.removeSubscriberAsPublisher.mockClear();
  installChannelRotateNow(rotateNow);
  installChannelRotateRequest(request);
  for (const id of [OWNED, CO]) {
    channelsActions.setKeys(id, KEYS);
    channelsActions.updateAccess(id, (r) => ({ ...r, visibility: "private" }));
  }
  channelsActions.setCoPublisherGroupId(CO, "ab".repeat(32));
});

describe("removeChannelSubscriber", () => {
  it("on the owner's device: removes, then rotates", async () => {
    await removeChannelSubscriber(identity, OWNED, "cc".repeat(32));
    expect(api.removeSubscriberAsPublisher).toHaveBeenCalledTimes(1);
    expect(rotateNow).toHaveBeenCalledWith(OWNED, undefined);
    expect(request).not.toHaveBeenCalled();
  });

  it("on a co-publisher's device: removes, then asks the owner to rotate, never rotates itself", async () => {
    await removeChannelSubscriber(identity, CO, "cc".repeat(32));
    expect(api.removeSubscriberAsPublisher).toHaveBeenCalledTimes(1);
    expect(request).toHaveBeenCalledWith(CO);
    expect(rotateNow).not.toHaveBeenCalled();
    // Owed durably before it was published, and settled once it went out.
    expect(log.events.slice(0, 2)).toEqual(["saved", "request"]);
    expect(channelsActions.accessOf(CO).rotateRequestOwed).toBeUndefined();
  });

  it("a request that could not go out stays owed; the removal succeeds; the next pass sends it", async () => {
    request.mockRejectedValueOnce(new Error("network"));
    await expect(removeChannelSubscriber(identity, CO, "cc".repeat(32))).resolves.toBeUndefined();
    expect(channelsActions.accessOf(CO).rotateRequestOwed).toBe(true);

    // A pass with no network yet: still owed.
    request.mockRejectedValueOnce(new Error("network"));
    expect(await settleOwedRotateRequest(CO)).toBe("failed");
    expect(channelsActions.accessOf(CO).rotateRequestOwed).toBe(true);

    // The next pass sends it and clears it; after that a pass sends nothing.
    expect(await settleOwedRotateRequest(CO)).toBe("sent");
    expect(channelsActions.accessOf(CO).rotateRequestOwed).toBeUndefined();
    request.mockClear();
    expect(await settleOwedRotateRequest(CO)).toBe("none");
    expect(request).not.toHaveBeenCalled();
  });
});
