// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0053 on the receive path (`ChatClient.pollMessages`):
 *
 *   - contract point 4, heal on apply: when a polled batch moved the epoch,
 *     the new epoch's key is registered proven under the key captured BEFORE
 *     the batch; when it did not, nothing is sent;
 *   - contract point 7, members read knocks: an owner / admin also reads the
 *     group's knock queue and feeds each row to the same receive path, which
 *     turns an external-join proposal into the `externalJoinRequest` event
 *     the members panel lists; a plain member does not read it.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChatClient } from "./client";
import type { DsClient } from "../ds/client";
import type { ProofSigner } from "../ds/access";
import type { PushClient } from "../push/client";
import { fakeAccessSigner } from "../ds/access-test-doubles";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

const IDENTITY_TOKEN = {} as unknown as WasmIdentity;

let nextGid = 0;
function freshGid(): Uint8Array {
  nextGid += 1;
  return new Uint8Array([0xd5, nextGid]);
}

/** Payload byte 0 says what a row is: 0xc0 a commit (moves the epoch), 0x1b a
 *  knock proposal, 0xa7 an attribution (not an MLS message). */
function fakeGroup(rank: number | undefined): { group: WasmGroup; epoch: () => bigint } {
  let epoch = 5n;
  const gid = freshGid();
  const group = {
    get groupId(): Uint8Array {
      return gid;
    },
    get epoch(): bigint {
      return epoch;
    },
    dsAccessSigner: fakeAccessSigner(() => epoch),
    ownCommitterRank: () => rank,
    members: () => [],
    processIncomingVerifyingRoles: vi.fn((_id: unknown, payload: Uint8Array) => {
      if (payload[0] === 0xc0) {
        epoch += 1n;
        return { kind: "Commit" };
      }
      if (payload[0] === 0x1b) {
        return {
          kind: "ExternalJoinProposal",
          externalJoinSignatureKey: new Uint8Array(32).fill(payload[1] ?? 0),
          externalJoinRetained: true,
        };
      }
      throw new Error("not an MLS message");
    }),
    // A faithful double of the core's knock gate: anything that is not a
    // stranger's external-join proposal is refused BEFORE processing (so a
    // commit here does NOT move the epoch); a proposal is processed like the
    // journal path would.
    processKnock: vi.fn((_id: unknown, payload: Uint8Array) => {
      if (payload[0] !== 0x1b) throw new Error("not a stranger's external-join proposal");
      return {
        kind: "ExternalJoinProposal",
        externalJoinSignatureKey: new Uint8Array(32).fill(payload[1] ?? 0),
        externalJoinRetained: true,
      };
    }),
  } as unknown as WasmGroup;
  return { group, epoch: () => epoch };
}

function row(id: number, bytes: number[]): { id: number; payload: Uint8Array; createdAt: string } {
  return { id, payload: new Uint8Array(bytes), createdAt: "t" };
}

function fakeDs(
  journal: ReturnType<typeof row>[],
  knocks: ReturnType<typeof row>[],
): { ds: DsClient; registrations: string[]; knockReads: number[] } {
  const registrations: string[] = [];
  const knockReads: number[] = [];
  const ds = {
    fetchGroup: vi.fn(async () => ({ messages: journal, hasMore: false })),
    fetchKnocks: vi.fn(async (_g: Uint8Array, opts: { after?: number }) => {
      knockReads.push(opts.after ?? 0);
      return { messages: knocks, hasMore: false };
    }),
    registerGroupEpoch: vi.fn(async (_g: Uint8Array, next: ProofSigner, by: ProofSigner | null) => {
      registrations.push(`${next.epoch}<-${by?.epoch ?? "unsigned"}`);
    }),
  } as unknown as DsClient;
  return { ds, registrations, knockReads };
}

function chat(ds: DsClient): ChatClient {
  return new ChatClient(
    ds,
    { identityProvider: () => IDENTITY_TOKEN },
    { notifyGroup: vi.fn(async () => {}) } as unknown as PushClient,
  );
}

/** Let the fire-and-forget heal settle. */
const settle = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

afterEach(() => {
  vi.clearAllMocks();
});

describe("ADR-0053 contract point 4 — heal on apply", () => {
  it("a batch that moved the epoch registers the new key under the pre-batch key", async () => {
    const g = fakeGroup(undefined);
    const { ds, registrations } = fakeDs([row(1, [0xc0]), row(2, [0xc0])], []);

    await chat(ds).pollMessages(null, g.group, 0);
    await settle();

    expect(g.epoch()).toBe(7n);
    expect(registrations).toEqual(["7<-5"]);
  });

  it("a batch that did not move the epoch registers nothing", async () => {
    const g = fakeGroup(undefined);
    const { ds, registrations } = fakeDs([row(1, [0xa7])], []);

    await chat(ds).pollMessages(null, g.group, 0);
    await settle();

    expect(registrations).toEqual([]);
  });
});

describe("ADR-0053 contract point 7 — an owner or admin reads the knock queue", () => {
  it("a knocked external-join proposal surfaces as an `externalJoinRequest` event; the attribution beside it is skipped", async () => {
    const g = fakeGroup(0);
    const { ds, knockReads } = fakeDs([], [row(1, [0xa7]), row(2, [0x1b, 0x42])]);

    const result = await chat(ds).pollMessages(null, g.group, 0);

    expect(knockReads).toHaveLength(1);
    const requests = result.events.filter((e) => e.kind === "externalJoinRequest");
    expect(requests).toHaveLength(1);
    expect(requests[0]).toMatchObject({
      kind: "externalJoinRequest",
      joinerSignatureKey: new Uint8Array(32).fill(0x42),
    });
  });

  it("a member's COMMIT posted as a knock goes through the knock gate, is refused and never applied", async () => {
    const g = fakeGroup(0);
    const { ds } = fakeDs([], [row(1, [0xc0]), row(2, [0x1b, 0x42])]);

    const result = await chat(ds).pollMessages(null, g.group, 0);

    // The commit did not move this owner's epoch: it was never processed.
    expect(g.epoch()).toBe(5n);
    const journalApply = (g.group as unknown as { processIncomingVerifyingRoles: ReturnType<typeof vi.fn> })
      .processIncomingVerifyingRoles;
    expect(journalApply).not.toHaveBeenCalled();
    const knockApply = (g.group as unknown as { processKnock: ReturnType<typeof vi.fn> }).processKnock;
    expect(knockApply).toHaveBeenCalledTimes(2);
    // The proposal beside it still surfaces.
    expect(result.events.filter((e) => e.kind === "externalJoinRequest")).toHaveLength(1);
  });

  it("a plain member does not read knocks", async () => {
    const g = fakeGroup(undefined);
    const { ds, knockReads } = fakeDs([], [row(1, [0x1b, 0x42])]);

    const result = await chat(ds).pollMessages(null, g.group, 0);

    expect(knockReads).toEqual([]);
    expect(result.events).toEqual([]);
  });
});
