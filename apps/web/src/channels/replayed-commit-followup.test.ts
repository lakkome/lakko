// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it, vi } from "vitest";

import { afterReplayedPublisherCommits, type ReplayFollowupPorts } from "./replayed-commit-followup";

const SEED = new Uint8Array(32).fill(7);

function ports(over: Partial<Pick<ReplayFollowupPorts, "seedOf" | "reissueRecord">> = {}) {
  const landed = vi.fn<ReplayFollowupPorts["landed"]>();
  const reissueRecord = over.reissueRecord ?? vi.fn<ReplayFollowupPorts["reissueRecord"]>(async () => undefined);
  const seedOf = over.seedOf ?? vi.fn<ReplayFollowupPorts["seedOf"]>(async () => SEED);
  return { landed, reissueRecord, seedOf };
}

describe("afterReplayedPublisherCommits", () => {
  it("a replayed approve takes the new seed and issues the channel record again", async () => {
    const p = ports();
    await afterReplayedPublisherCommits([{ channelIdHex: "aa", kind: "approve", serverTargetEpoch: 1 }], p);
    expect(p.landed).toHaveBeenCalledWith("aa", 1, SEED);
    expect(p.reissueRecord).toHaveBeenCalledWith("aa");
  });

  it("once per channel, at the newest epoch its entries landed at", async () => {
    const p = ports();
    await afterReplayedPublisherCommits(
      [
        { channelIdHex: "aa", kind: "approve", serverTargetEpoch: 1 },
        { channelIdHex: "aa", kind: "rotation", serverTargetEpoch: 2 },
        { channelIdHex: "bb", kind: "removal", serverTargetEpoch: 5 },
      ],
      p,
    );
    expect(p.landed.mock.calls).toEqual([
      ["aa", 2, SEED],
      ["bb", 5, SEED],
    ]);
    expect(p.reissueRecord).toHaveBeenCalledTimes(2);
  });

  it("skips a channel with no active group, and one failure does not stop the others", async () => {
    const p = ports({
      seedOf: vi.fn<ReplayFollowupPorts["seedOf"]>(async (id) => (id === "aa" ? null : SEED)),
      reissueRecord: vi.fn<ReplayFollowupPorts["reissueRecord"]>(async (id) => {
        if (id === "bb") throw new Error("offline");
      }),
    });
    await afterReplayedPublisherCommits(
      [
        { channelIdHex: "aa", kind: "approve", serverTargetEpoch: 1 },
        { channelIdHex: "bb", kind: "approve", serverTargetEpoch: 1 },
        { channelIdHex: "cc", kind: "approve", serverTargetEpoch: 1 },
      ],
      p,
    );
    expect(p.landed.mock.calls.map((c) => c[0])).toEqual(["bb", "cc"]);
    expect(p.reissueRecord).toHaveBeenCalledWith("cc");
  });
});
