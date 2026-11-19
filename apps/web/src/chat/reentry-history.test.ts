// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 section 7 - the gap carrier shared with iOS and Android, and the
 * re-admitter's owed send.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { base64Encode } from "../auth/account-id";
import { WebDeviceCipher } from "../auth/device-cipher";
import { bindContactsAccount } from "../contacts/local-store";
import {
  GAP_HISTORY_LABEL,
  GAP_HISTORY_LOOK_INTERVAL_MS,
  GAP_HISTORY_LOOK_RETENTION_MS,
  GAP_OWED_TTL_MS,
  afterGroupPollForGapHistory,
  gapHistoryOwed,
  gapLookStep,
  gapRendezvousOf,
  lookForGapHistory,
  noteGapHistoryOwed,
  owedGapHistoryGroups,
  recordGapLook,
  resetGapHistoryMemory,
  type FiledGapHistory,
  type GapLookPorts,
  type GapSendPorts,
} from "./reentry-history";
import { resendsPushRoutingSecret } from "./reentry-admit";
import { clearReentryRowsForAccount, clearReentryRowsForGroup, gapLookFor } from "./reentry-store";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

function hex(b: Uint8Array): string {
  return [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
}

/** The `localStorage` shim convention this repo already uses. */
class MemoryStorage {
  readonly map = new Map<string, string>();
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return [...this.map.keys()][i] ?? null;
  }
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, v);
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
}

let storage: MemoryStorage;

beforeEach(() => {
  storage = new MemoryStorage();
  (globalThis as unknown as { localStorage: unknown }).localStorage = storage;
  bindContactsAccount("acct-1");
});

afterEach(() => {
  resetGapHistoryMemory();
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
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

describe("the owed send is durable (live walk 2026-11-19)", () => {
  it("is kept in storage, so it survives a restart, and goes with the group", () => {
    noteGapHistoryOwed({ groupIdB64: "Bwc=", leafKey: new Uint8Array(32).fill(1), epochAfter: 5n, gapFromMs: 1, nowMs: 1_000 });
    resetGapHistoryMemory();
    expect(owedGapHistoryGroups()).toEqual([{ groupIdB64: "Bwc=", epochAfter: 5n }]);
    clearReentryRowsForGroup("Bwc=");
    expect(gapHistoryOwed("Bwc=")).toBe(false);
  });

  it("waits up to 30 days, then is dropped", async () => {
    expect(GAP_OWED_TTL_MS).toBe(30 * 24 * 60 * 60_000);
    const { group } = fakeGroup({ e: 4n });
    noteGapHistoryOwed({ groupIdB64: "Bwc=", leafKey: new Uint8Array(32), epochAfter: 5n, gapFromMs: 1, nowMs: 0 });
    const day29 = ports({ now: () => 29 * 24 * 60 * 60_000 });
    expect(await afterGroupPollForGapHistory(group, day29.p)).toEqual(["waiting"]);
    const day31 = ports({ now: () => 31 * 24 * 60 * 60_000 });
    expect(await afterGroupPollForGapHistory(group, day31.p)).toEqual(["dropped"]);
    expect(gapHistoryOwed("Bwc=")).toBe(false);
  });

  it("two poll paths at once send one bundle", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const { group } = fakeGroup({ e: 5n });
    noteGapHistoryOwed({ groupIdB64: "Bwc=", leafKey: new Uint8Array(32), epochAfter: 5n, gapFromMs: 1, nowMs: 1_000 });
    const t = ports();
    const [a, b] = await Promise.all([afterGroupPollForGapHistory(group, t.p), afterGroupPollForGapHistory(group, t.p)]);
    expect([...(a ?? []), ...(b ?? [])]).toEqual(["sent"]);
    expect(t.published.length).toBe(1);
  });
});

describe("the push routing secret after a replacement", () => {
  it("is re-sent by the owner or an admin only", () => {
    expect(resendsPushRoutingSecret("owner")).toBe(true);
    expect(resendsPushRoutingSecret("admin")).toBe(true);
    expect(resendsPushRoutingSecret("member")).toBe(false);
    expect(resendsPushRoutingSecret(null)).toBe(false);
  });
});

describe("the device that came back keeps looking (live walk 2026-11-19)", () => {
  it("the step: a look, then a minute's wait, dropped after 30 days", () => {
    expect(GAP_HISTORY_LOOK_INTERVAL_MS).toBe(60_000);
    expect(GAP_HISTORY_LOOK_RETENTION_MS).toBe(30 * 24 * 60 * 60_000);
    const to = 1_000_000;
    expect(gapLookStep({ gapToMs: to, lastLookMs: 0 }, to)).toBe("look");
    expect(gapLookStep({ gapToMs: to, lastLookMs: to }, to + 59_999)).toBe("wait");
    expect(gapLookStep({ gapToMs: to, lastLookMs: to }, to + 60_000)).toBe("look");
    // A clock that moved back is due.
    expect(gapLookStep({ gapToMs: to, lastLookMs: to + 5_000 }, to + 1_000)).toBe("look");
    expect(gapLookStep({ gapToMs: to, lastLookMs: 0 }, to + GAP_HISTORY_LOOK_RETENTION_MS + 1)).toBe("drop");
  });

  async function cipher(): Promise<WebDeviceCipher> {
    const key = await globalThis.crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
    return new WebDeviceCipher(key);
  }

  const secret = new Uint8Array(32).fill(0x42);
  const filed: FiledGapHistory = { applied: 2, complete: true, earliestMs: 10, rows: [] };

  function lookPorts(deviceKey: WebDeviceCipher, over: Partial<GapLookPorts> = {}) {
    const reads: string[] = [];
    const received: { group: string; toMs: number }[] = [];
    let now = 2_000_000;
    const p: GapLookPorts = {
      now: () => now,
      deviceKey: async () => deviceKey,
      read: async (rendezvous) => {
        reads.push(hex(rendezvous));
        return [];
      },
      apply: async () => null,
      received: (group, toMs) => void received.push({ group, toMs }),
      ...over,
    };
    return { p, reads, received, advance: (ms: number) => (now += ms) };
  }

  it("stores the secret sealed, never in the clear", async () => {
    await recordGapLook({ groupIdB64: "Bwc=", secret, gapFromMs: 1, gapToMs: 2_000_000 }, await cipher());
    const stored = [...storage.map.values()].join("|");
    expect(stored.includes(base64Encode(secret))).toBe(false);
    expect(stored.includes(hex(secret))).toBe(false);
    expect(gapLookFor("Bwc=")).toBeDefined();
  });

  it("finds nothing, waits a minute, keeps looking; the bundle ends the look", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const key = await cipher();
    await recordGapLook({ groupIdB64: "Bwc=", secret, gapFromMs: 1, gapToMs: 2_000_000 }, key);
    let rows: Uint8Array[] = [];
    const t = lookPorts(key, {
      read: async () => rows,
      apply: async (s, sealed) => (hex(s) === hex(secret) && sealed[0] === 9 ? filed : null),
    });
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("none");
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("wait");
    t.advance(GAP_HISTORY_LOOK_INTERVAL_MS);
    // A row that does not open keeps the look.
    rows = [new Uint8Array([1])];
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("none");
    expect(gapLookFor("Bwc=")).toBeDefined();
    t.advance(GAP_HISTORY_LOOK_INTERVAL_MS);
    rows = [new Uint8Array([1]), new Uint8Array([9])];
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("received");
    expect(t.received).toEqual([{ group: "Bwc=", toMs: 2_000_000 }]);
    expect(gapLookFor("Bwc=")).toBeUndefined();
  });

  it("reads the rendezvous the carrier names", async () => {
    const key = await cipher();
    await recordGapLook({ groupIdB64: "Bwc=", secret, gapFromMs: 1, gapToMs: 2_000_000 }, key);
    const t = lookPorts(key);
    await lookForGapHistory("Bwc=", t.p);
    expect(t.reads).toEqual(["8b4c89a03f20c038dfe71cf88eeee780a4be0af619b852cedbc63ad0dc6eb7dd"]);
  });

  it("a failed read keeps the look", async () => {
    const key = await cipher();
    await recordGapLook({ groupIdB64: "Bwc=", secret, gapFromMs: 1, gapToMs: 2_000_000 }, key);
    const t = lookPorts(key, { read: async () => null });
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("none");
    expect(gapLookFor("Bwc=")).toBeDefined();
  });

  it("past 30 days the look goes without a read; the gap line stays", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const key = await cipher();
    await recordGapLook({ groupIdB64: "Bwc=", secret, gapFromMs: 1, gapToMs: 0 }, key);
    const t = lookPorts(key, { now: () => GAP_HISTORY_LOOK_RETENTION_MS + 1 });
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("dropped");
    expect(t.reads).toEqual([]);
    expect(gapLookFor("Bwc=")).toBeUndefined();
  });

  it("a secret sealed under another device key is dropped", async () => {
    await recordGapLook({ groupIdB64: "Bwc=", secret, gapFromMs: 1, gapToMs: 2_000_000 }, await cipher());
    const t = lookPorts(await cipher());
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("dropped");
    expect(t.reads).toEqual([]);
  });

  it("goes on Forget device", async () => {
    await recordGapLook({ groupIdB64: "Bwc=", secret, gapFromMs: 1, gapToMs: 2_000_000 }, await cipher());
    noteGapHistoryOwed({ groupIdB64: "Bwc=", leafKey: new Uint8Array(32), epochAfter: 5n, gapFromMs: 1, nowMs: 1_000 });
    clearReentryRowsForAccount("acct-1");
    expect(storage.map.size).toBe(0);
  });
});
