// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Unit tests for the ADR-0030 chunked staged-rotation fanout
 * orchestration (`stageAndCommitRotation` in `rotation-cron.ts`).
 * The `./api` transport is mocked (only `stageRotation` /
 * `commitRotation` are replaced; the real `MAX_STAGE_CHUNK_ENTRIES`
 * constant is preserved) so the tests exercise the pure chunking +
 * sequencing logic without a network:
 *
 *   - a >MAX batch splits into ceil(N/MAX) stage chunks;
 *   - commit runs exactly once, AFTER every chunk has staged
 *     (C-42: the epoch is bumped nowhere but in the commit);
 *   - an empty batch stages nothing and commits directly
 *     (still bumps the epoch — the "no subscribers yet" case);
 *   - a stage failure aborts before commit (no partial rotation).
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();
  return {
    ...actual,
    stageRotation: vi.fn(),
    commitRotation: vi.fn(),
  };
});

import { commitRotation, MAX_STAGE_CHUNK_ENTRIES, stageRotation } from "./api";
import { stageAndCommitRotation } from "./rotation-cron";

const CH = "00112233445566778899aabbccddeeff";
/** The account identity the stage and commit calls are signed with; the
 *  transport is mocked, so only its identity (pass-through) matters. */
const ID = { tag: "account" } as unknown as Parameters<typeof stageAndCommitRotation>[3];

function entry(i: number) {
  return {
    subscriber_pubkey_hex: i.toString(16).padStart(2, "0"),
    envelope_b64: "ZW52",
  };
}

beforeEach(() => {
  vi.mocked(stageRotation).mockReset();
  vi.mocked(commitRotation).mockReset();
  vi.mocked(stageRotation).mockResolvedValue({ targetEpoch: 0, staged: 0 });
  vi.mocked(commitRotation).mockImplementation((_ch, epoch) =>
    Promise.resolve({ epoch }),
  );
});

describe("stageAndCommitRotation", () => {
  it("splits a >MAX batch into ceil(N/MAX) stage chunks then one commit", async () => {
    const batch = Array.from(
      { length: MAX_STAGE_CHUNK_ENTRIES + 1 },
      (_v, i) => entry(i),
    );

    const epoch = await stageAndCommitRotation(CH, 7, batch, ID);

    expect(epoch).toEqual({ epoch: 7 });
    expect(vi.mocked(stageRotation)).toHaveBeenCalledTimes(2);

    // First chunk is a full MAX-sized slice; the remainder is 1.
    const call0 = vi.mocked(stageRotation).mock.calls[0]!;
    const call1 = vi.mocked(stageRotation).mock.calls[1]!;
    expect(call0[0]).toBe(CH);
    expect(call0[1]).toBe(7);
    expect(call0[2]).toHaveLength(MAX_STAGE_CHUNK_ENTRIES);
    expect(call1[2]).toHaveLength(1);
    // Chunks partition the batch — no overlap, no gap.
    expect(call1[2][0]).toEqual(entry(MAX_STAGE_CHUNK_ENTRIES));
    expect(call0[3]).toBe(ID);
    expect(call1[3]).toBe(ID);

    expect(vi.mocked(commitRotation)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(commitRotation)).toHaveBeenCalledWith(CH, 7, ID, undefined);
  });

  it("stages a small batch in a single chunk then commits", async () => {
    const batch = [entry(1), entry(2), entry(3)];

    await stageAndCommitRotation(CH, 4, batch, ID, "https://ch.example");

    expect(vi.mocked(stageRotation)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(stageRotation)).toHaveBeenCalledWith(
      CH,
      4,
      batch,
      ID,
      "https://ch.example",
    );
    expect(vi.mocked(commitRotation)).toHaveBeenCalledWith(
      CH,
      4,
      ID,
      "https://ch.example",
    );
  });

  it("stages nothing for an empty batch but still commits (bumps the epoch)", async () => {
    const epoch = await stageAndCommitRotation(CH, 1, [], ID);

    expect(vi.mocked(stageRotation)).not.toHaveBeenCalled();
    expect(vi.mocked(commitRotation)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(commitRotation)).toHaveBeenCalledWith(CH, 1, ID, undefined);
    expect(epoch).toEqual({ epoch: 1 });
  });

  it("commits only after every chunk has staged", async () => {
    const batch = Array.from(
      { length: MAX_STAGE_CHUNK_ENTRIES + 1 },
      (_v, i) => entry(i),
    );

    await stageAndCommitRotation(CH, 7, batch, ID);

    const stageOrders = vi.mocked(stageRotation).mock.invocationCallOrder;
    const commitOrder =
      vi.mocked(commitRotation).mock.invocationCallOrder[0]!;
    // Every stage call was invoked before the single commit.
    expect(Math.max(...stageOrders)).toBeLessThan(commitOrder);
  });

  it("aborts before commit when a stage chunk fails (no partial rotation)", async () => {
    const batch = Array.from(
      { length: MAX_STAGE_CHUNK_ENTRIES + 1 },
      (_v, i) => entry(i),
    );
    // The second chunk fails; commit must never run.
    vi.mocked(stageRotation)
      .mockResolvedValueOnce({ targetEpoch: 7, staged: MAX_STAGE_CHUNK_ENTRIES })
      .mockRejectedValueOnce(new Error("stage chunk 2 dropped"));

    await expect(stageAndCommitRotation(CH, 7, batch, ID)).rejects.toThrow(
      "stage chunk 2 dropped",
    );

    expect(vi.mocked(stageRotation)).toHaveBeenCalledTimes(2);
    expect(vi.mocked(commitRotation)).not.toHaveBeenCalled();
  });
});
