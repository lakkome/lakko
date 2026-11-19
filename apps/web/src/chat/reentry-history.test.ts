// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 section 7 - the gap carrier shared with iOS and Android, and the
 * re-admitter's owed send.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  GAP_HISTORY_LABEL,
  afterGroupPollForGapHistory,
  gapHistoryOwed,
  gapRendezvousOf,
  noteGapHistoryOwed,
  resetGapHistoryMemory,
  type GapSendPorts,
} from "./reentry-history";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

function hex(b: Uint8Array): string {
  return [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
}

afterEach(() => {
  resetGapHistoryMemory();
  vi.restoreAllMocks();
});

describe("the carrier", () => {
  it("golden: the rendezvous id of a 0x42 secret matches iOS and Android", async () => {
    const id = await gapRendezvousOf(new Uint8Array(32).fill(0x42));
    expect(hex(id)).toBe("8b4c89a03f20c038dfe71cf88eeee780a4be0af619b852cedbc63ad0dc6eb7dd");
  });
});

function fakeGroup(epochRef: { e: bigint }) {
  const exports: { label: string; context: Uint8Array }[] = [];
  const group = {
    groupId: new Uint8Array([7, 7]),
    get epoch() {
      return epochRef.e;
    },
    exportSecret: (_i: unknown, label: string, context: Uint8Array) => {
      exports.push({ label, context });
      return new Uint8Array(32).fill(0x42);
    },
  } as unknown as WasmGroup;
  return { group, exports };
}

function ports(over: Partial<GapSendPorts> = {}) {
  const published: { rendezvous: Uint8Array; sealed: Uint8Array }[] = [];
  const landed: WasmGroup[] = [];
  const built: number[] = [];
  const p: GapSendPorts = {
    now: () => 1_000,
    identity: () => ({}) as WasmIdentity,
    landed: (g) => void landed.push(g),
    build: async (_g, _s, fromMs) => {
      built.push(fromMs);
      return new Uint8Array([9]);
    },
    publish: async (rendezvous, sealed) => void published.push({ rendezvous, sealed }),
    ...over,
  };
  return { p, published, landed, built };
}

describe("the re-admitter's owed send", () => {
  it("waits below the replacement's epoch, then sends once at it under the returning leaf's key", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const epoch = { e: 4n };
    const { group, exports } = fakeGroup(epoch);
    const leaf = new Uint8Array(32).fill(3);
    noteGapHistoryOwed({ groupIdB64: "Bwc=", leafKey: leaf, epochAfter: 5n, gapFromMs: 500, nowMs: 1_000 });
    const t = ports();
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["waiting"]);
    epoch.e = 5n;
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["sent"]);
    expect(exports).toEqual([{ label: GAP_HISTORY_LABEL, context: leaf }]);
    expect(t.built).toEqual([500]);
    expect(t.landed.length).toBe(1);
    expect(hex(t.published[0]?.rendezvous ?? new Uint8Array())).toBe(
      "8b4c89a03f20c038dfe71cf88eeee780a4be0af619b852cedbc63ad0dc6eb7dd",
    );
    expect(gapHistoryOwed("Bwc=")).toBe(false);
  });

  it("drops the send when the group moved past the replacement's epoch", async () => {
    const epoch = { e: 7n };
    const { group } = fakeGroup(epoch);
    noteGapHistoryOwed({ groupIdB64: "Bwc=", leafKey: new Uint8Array(32), epochAfter: 5n, gapFromMs: 1, nowMs: 1_000 });
    const t = ports();
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["dropped"]);
    expect(t.published).toEqual([]);
  });

  it("nothing held for the gap: no publish", async () => {
    const epoch = { e: 5n };
    const { group } = fakeGroup(epoch);
    noteGapHistoryOwed({ groupIdB64: "Bwc=", leafKey: new Uint8Array(32), epochAfter: 5n, gapFromMs: 1, nowMs: 1_000 });
    const t = ports({ build: async () => null });
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["dropped"]);
    expect(t.published).toEqual([]);
    // The growth-time state is still re-sent: the replacement landed.
    expect(t.landed.length).toBe(1);
  });
});
