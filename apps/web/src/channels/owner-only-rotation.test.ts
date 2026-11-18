// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Only the channel owner's device rotates the channel key (maintainer,
 * 2026-11-08). A co-publisher's rotation was refused by the journal while
 * its device had already merged it and resealed, forking it from the owner.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const registry = vi.hoisted(() => ({ withPublisherGroup: vi.fn() }));
vi.mock("./publisher-group-registry", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./publisher-group-registry")>();
  return { ...actual, withPublisherGroup: registry.withPublisherGroup };
});

// The owner's path loads the core before taking the group; nothing of it
// is used when the group step is stubbed.
vi.mock("../wasm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../wasm")>();
  return {
    ...actual,
    loadWasm: vi.fn(async () => ({
      channelRotationPolicyDefault: () => ({}),
      channelRotationStateNew: () => ({ lastRotationMs: 0n, postsSinceRotation: 0 }),
      channelRotationStateAfterRotation: () => ({ lastRotationMs: 1n, postsSinceRotation: 0 }),
      // Not due: a scheduled check takes no group (ADR-0090: it would wait
      // and re-read the journal first).
      channelShouldRotate: () => false,
    })),
  };
});

import { channelRotatesHere } from "./channel-access";
import { rotateChannel } from "./ChannelPublisherLoops";
import { channelsActions } from "../state/channels-state";
import type { WasmIdentity } from "./composer";

const KEYS = { seed: new Uint8Array(32).fill(1), signingKeySeed: new Uint8Array(32).fill(2), seedEpoch: 0 };
const OWNED = "11".repeat(16);
const CO = "22".repeat(16);
const NONE = "33".repeat(16);
const identity = {} as WasmIdentity;

beforeEach(() => {
  registry.withPublisherGroup.mockReset();
  channelsActions.setKeys(OWNED, KEYS);
  channelsActions.setKeys(CO, KEYS);
  channelsActions.setCoPublisherGroupId(CO, "ab".repeat(32));
});

describe("channelRotatesHere", () => {
  it("is the owner's device only", () => {
    expect(channelRotatesHere(OWNED)).toBe(true);
    expect(channelRotatesHere(CO)).toBe(false);
    expect(channelRotatesHere(NONE)).toBe(false);
  });
});

describe("rotateChannel", () => {
  it("does nothing on a co-publisher's device, scheduled or forced", async () => {
    await rotateChannel(identity, CO, false);
    await rotateChannel(identity, CO, true);
    expect(registry.withPublisherGroup).not.toHaveBeenCalled();
  });

  it("runs on an owner device (a forced rotation: the person's own action, no wait)", async () => {
    registry.withPublisherGroup.mockResolvedValue(undefined);
    await rotateChannel(identity, OWNED, true);
    expect(registry.withPublisherGroup).toHaveBeenCalledTimes(1);
  });

  it("ADR-0090: a scheduled check that is not due takes no group", async () => {
    registry.withPublisherGroup.mockResolvedValue(undefined);
    await rotateChannel(identity, OWNED, false);
    expect(registry.withPublisherGroup).not.toHaveBeenCalled();
  });
});
