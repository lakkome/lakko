// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 3 — one STAGED PublisherGroup commit end to end
 * (`runStagedPublisherCommit`): recorded before anything leaves the device,
 * abandoned when the record is refused, merged when the journal takes it,
 * abandoned (and nothing resealed) when another owner device's commit took
 * the epoch, kept owed (nothing merged, nothing resealed) when the journal
 * does not answer. The group, the DS and the store are doubles.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const log = vi.hoisted(() => ({ order: [] as string[], recordFails: false }));

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    sealSubscriberSeedEnvelope: () => new Uint8Array([0xaa]),
    DsProofSigner: { forGroup: () => ({ epoch: 4n, verifyingKey: () => new Uint8Array(32), free: () => undefined }) },
  }),
}));
vi.mock("./api", () => ({
  fetchEpoch: async () => ({ epoch: 4 }),
  listSubscriberPage: async () => ({ entries: [], nextAfter: 0 }),
  putPublicSeed: async () => undefined,
  stageRotation: async () => undefined,
  commitRotation: async () => {
    log.order.push("reseal-commit");
    return { epoch: 5 };
  },
  MAX_STAGE_CHUNK_ENTRIES: 5000,
}));
vi.mock("../state/channels-state", () => ({
  channelsActions: {
    recordPublisherCommit: async () => {
      log.order.push("record");
      if (log.recordFails) throw new Error("disk full");
    },
    savePublisherCommit: async () => undefined,
    finishPublisherCommit: async () => {
      log.order.push("finish");
    },
    persistPublisherGroupDurable: async () => {
      log.order.push("persist");
    },
    notePublisherGroupChanged: () => undefined,
    accessOf: () => ({ visibility: null, readKeyB64: null, admittedThroughSeq: 0, admissionSeeds: [] }),
    updateAccessDurable: async () => undefined,
  },
  channelsState: { metaByChannel: () => new Map() },
}));

import { runStagedPublisherCommit } from "./rotation-cron";
import { CommitConflictError } from "../ds/ownership";

function group() {
  let staged = false;
  let epoch = 4n;
  const signer = (e: bigint) => ({ epoch: e, verifyingKey: () => new Uint8Array(32), free: () => undefined });
  return {
    groupId: () => new Uint8Array([1, 2]),
    channelId: () => new Uint8Array(16),
    epoch: () => epoch,
    hasStagedCommit: () => staged,
    dsAccessSeed: () => new Uint8Array(32).fill(4),
    dsAccessSigner: () => signer(epoch),
    dsStagedAccessSigner: () => (staged ? signer(epoch + 1n) : undefined),
    channelSeed: () => {
      log.order.push(`seed@${epoch}`);
      return new Uint8Array(32);
    },
    stageRotate: () => {
      staged = true;
      log.order.push("stage");
      return new Uint8Array([9]);
    },
    mergeStagedCommit: () => {
      staged = false;
      epoch += 1n;
      log.order.push("merge");
      return true;
    },
    abandonStagedCommit: () => {
      staged = false;
      log.order.push("abandon");
      return true;
    },
  };
}

function ctx(g: ReturnType<typeof group>, answer: "201" | "409" | "offline") {
  return {
    publisherGroup: g as never,
    identity: {} as never,
    device: {} as never,
    channelIdHex: "11".repeat(16),
    ds: {
      commitGroupEpoch: async () => {
        log.order.push("commit");
        if (answer === "409") throw new CommitConflictError(Object.assign(new Error("409"), { status: 409 }));
        if (answer === "offline") throw new Error("network");
        return { id: 1, createdAt: "t", resent: false };
      },
      ensureOwnerClaim: async () => "claimed",
    } as never,
  };
}

beforeEach(() => {
  log.order.length = 0;
  log.recordFails = false;
  vi.spyOn(console, "info").mockImplementation(() => undefined);
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
  vi.spyOn(console, "error").mockImplementation(() => undefined);
});

const rotation = (g: ReturnType<typeof group>) => ({
  kind: "rotation" as const,
  stage: () => ({ commit: g.stageRotate(), welcome: null }),
});

describe("a staged PublisherGroup commit (ADR-0090 section 3)", () => {
  it("records first, then the journal decides, merge, then the reseal of the merged seed", async () => {
    const g = group();
    const run = await runStagedPublisherCommit(ctx(g, "201"), rotation(g));
    expect(run).toEqual({ outcome: "landed", serverTargetEpoch: 5 });
    expect(log.order).toEqual(["stage", "record", "commit", "merge", "persist", "seed@5", "reseal-commit", "finish"]);
  });

  it("a refused record abandons the stage and sends nothing", async () => {
    log.recordFails = true;
    const g = group();
    await expect(runStagedPublisherCommit(ctx(g, "201"), rotation(g))).rejects.toThrow("disk full");
    expect(log.order).toEqual(["stage", "record", "abandon"]);
    expect(g.hasStagedCommit()).toBe(false);
  });

  it("C-90: lost to another owner device (409): abandoned, at N, nothing resealed", async () => {
    const g = group();
    const run = await runStagedPublisherCommit(ctx(g, "409"), rotation(g));
    expect(run.outcome).toBe("lost");
    expect(log.order).toEqual(["stage", "record", "commit", "abandon", "persist", "finish"]);
    expect(g.epoch()).toBe(4n);
  });

  it("an unanswered commit stays staged and owed; nothing merged, nothing resealed", async () => {
    const g = group();
    const run = await runStagedPublisherCommit(ctx(g, "offline"), rotation(g));
    expect(run.outcome).toBe("owed");
    expect(log.order).toEqual(["stage", "record", "commit"]);
    expect(g.hasStagedCommit()).toBe(true);
    // A second commit is not staged over it.
    await expect(runStagedPublisherCommit(ctx(g, "201"), rotation(g))).rejects.toThrow(/still staged/);
  });
});
