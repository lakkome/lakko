// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 section 7 - the gap carrier shared with iOS and Android (a gap key
 * per request, security review 2026-11-21), and the re-admitter's owed send.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

import { base64Encode } from "../auth/account-id";
import { WebDeviceCipher } from "../auth/device-cipher";
import { bindContactsAccount } from "../contacts/local-store";
import {
  GAP_HISTORY_LOOK_INTERVAL_MS,
  GAP_HISTORY_LOOK_RETENTION_MS,
  GAP_LOOK_PAGE_CAP,
  GAP_LOOK_PAGE_SIZE,
  GAP_OWED_TTL_MS,
  afterGroupPollForGapHistory,
  gapHistoryOwed,
  gapLookStep,
  lookForGapHistory,
  noteGapHistoryOwed,
  owedGapHistoryGroups,
  recordGapLook,
  resetGapHistoryMemory,
  sealGapSecret,
  type FiledGapHistory,
  type GapLookPage,
  type GapLookPorts,
  type GapSendPorts,
} from "./reentry-history";
import { resendsPushRoutingSecret } from "./reentry-admit";
import {
  clearReentryRowsForAccount,
  clearReentryRowsForGroup,
  gapLookFor,
  leafFirstSeenMs,
  noteLeavesSeen,
  resetLeafSeenMemory,
  type SealedGapSecret,
} from "./reentry-store";
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

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

afterEach(() => {
  resetGapHistoryMemory();
  resetLeafSeenMemory();
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
  vi.restoreAllMocks();
});

const GAP_KEY = new Uint8Array(32).fill(0x24);

/** The core's clamp, as the runtime wires it. */
function coreStart(claimed: number, first: number | undefined, last: number | undefined): number | null {
  const s = wasm.gapHistoryStart(
    BigInt(claimed),
    first === undefined ? undefined : BigInt(first),
    last === undefined ? undefined : BigInt(last),
  );
  return s === undefined ? null : Number(s);
}

describe("the core's gap start", () => {
  it("is the later of the claim and the first sighting, else the last write; none without either", () => {
    expect(coreStart(500, 800, undefined)).toBe(800);
    expect(coreStart(900, 800, undefined)).toBe(900);
    expect(coreStart(500, undefined, 700)).toBe(700);
    expect(coreStart(500, undefined, undefined)).toBeNull();
  });
});

function fakeGroup(epochRef: { e: bigint }) {
  const group = {
    groupId: new Uint8Array([7, 7]),
    get epoch() {
      return epochRef.e;
    },
  } as unknown as WasmGroup;
  return { group };
}

function ports(over: Partial<GapSendPorts> = {}) {
  const published: { rendezvous: Uint8Array; sealed: Uint8Array }[] = [];
  const landed: WasmGroup[] = [];
  const built: { fromMs: number; gapKey: Uint8Array }[] = [];
  const p: GapSendPorts = {
    now: () => 1_000,
    identity: () => ({}) as WasmIdentity,
    landed: (g) => void landed.push(g),
    leafFirstSeenMs: () => 100,
    leafLastWriteMs: () => undefined,
    gapStart: coreStart,
    build: async (_g, _i, gapKey, fromMs) => {
      built.push({ fromMs, gapKey });
      return new Uint8Array([9]);
    },
    rendezvous: (gapKey) => wasm.gapHistoryRendezvous(gapKey),
    publish: async (rendezvous, sealed) => void published.push({ rendezvous, sealed }),
    ...over,
  };
  return { p, published, landed, built };
}

function owe(over: Partial<Parameters<typeof noteGapHistoryOwed>[0]> = {}): void {
  noteGapHistoryOwed({
    groupIdB64: "Bwc=",
    leafKey: new Uint8Array(32).fill(3),
    gapKey: GAP_KEY,
    epochAfter: 5n,
    gapFromMs: 500,
    nowMs: 1_000,
    ...over,
  });
}

describe("the re-admitter's owed send", () => {
  it("waits below the replacement's epoch, then sends once, to the request's gap key", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const epoch = { e: 4n };
    const { group } = fakeGroup(epoch);
    owe();
    const t = ports();
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["waiting"]);
    epoch.e = 5n;
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["sent"]);
    expect(t.built.map((b) => b.fromMs)).toEqual([500]);
    expect(hex(t.built[0]?.gapKey ?? new Uint8Array())).toBe(hex(GAP_KEY));
    expect(t.landed.length).toBe(1);
    expect(hex(t.published[0]?.rendezvous ?? new Uint8Array())).toBe(hex(wasm.gapHistoryRendezvous(GAP_KEY)));
    expect(gapHistoryOwed("Bwc=")).toBe(false);
  });

  it("sends when the group already moved past the replacement's epoch (no epoch dependency)", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const { group } = fakeGroup({ e: 7n });
    owe();
    const t = ports();
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["sent"]);
    expect(t.published.length).toBe(1);
  });

  it("clamps the claim to the leaf's first sighting: an early claim cannot pull older rows", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const { group } = fakeGroup({ e: 5n });
    owe({ gapFromMs: 10 });
    const t = ports({ leafFirstSeenMs: () => 800 });
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["sent"]);
    expect(t.built.map((b) => b.fromMs)).toEqual([800]);
  });

  it("no first sighting and no last write: nothing is built or sent", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const { group } = fakeGroup({ e: 5n });
    owe();
    const t = ports({ leafFirstSeenMs: () => undefined, leafLastWriteMs: () => undefined });
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["no-start"]);
    expect(t.built).toEqual([]);
    expect(t.published).toEqual([]);
    expect(gapHistoryOwed("Bwc=")).toBe(false);
  });

  it("falls back to the last write when the leaf was never seen in the roster", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const { group } = fakeGroup({ e: 5n });
    owe({ gapFromMs: 10 });
    const t = ports({ leafFirstSeenMs: () => undefined, leafLastWriteMs: () => 600 });
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["sent"]);
    expect(t.built.map((b) => b.fromMs)).toEqual([600]);
  });

  it("an owed send without a 32-byte gap key is not recorded", () => {
    owe({ gapKey: new Uint8Array(31) });
    expect(gapHistoryOwed("Bwc=")).toBe(false);
  });

  it("nothing held for the gap: no publish", async () => {
    const epoch = { e: 5n };
    const { group } = fakeGroup(epoch);
    owe();
    const t = ports({ build: async () => null });
    expect(await afterGroupPollForGapHistory(group, t.p)).toEqual(["dropped"]);
    expect(t.published).toEqual([]);
    // The growth-time state is still re-sent: the replacement landed.
    expect(t.landed.length).toBe(1);
  });
});

describe("the owed send is durable (live walk 2026-11-19)", () => {
  it("is kept in storage, so it survives a restart, and goes with the group", () => {
    owe({ leafKey: new Uint8Array(32).fill(1), gapFromMs: 1 });
    resetGapHistoryMemory();
    expect(owedGapHistoryGroups()).toEqual([{ groupIdB64: "Bwc=", epochAfter: 5n }]);
    clearReentryRowsForGroup("Bwc=");
    expect(gapHistoryOwed("Bwc=")).toBe(false);
  });

  it("waits up to 30 days, then is dropped", async () => {
    expect(GAP_OWED_TTL_MS).toBe(30 * 24 * 60 * 60_000);
    const { group } = fakeGroup({ e: 4n });
    owe({ gapFromMs: 1, nowMs: 0 });
    const day29 = ports({ now: () => 29 * 24 * 60 * 60_000 });
    expect(await afterGroupPollForGapHistory(group, day29.p)).toEqual(["waiting"]);
    const day31 = ports({ now: () => 31 * 24 * 60 * 60_000 });
    expect(await afterGroupPollForGapHistory(group, day31.p)).toEqual(["dropped"]);
    expect(gapHistoryOwed("Bwc=")).toBe(false);
  });

  it("two poll paths at once send one bundle", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const { group } = fakeGroup({ e: 5n });
    owe({ gapFromMs: 1 });
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

  async function record(groupIdB64: string, key: WebDeviceCipher, gapToMs = 2_000_000, secrets: Uint8Array[] = [secret]) {
    const sealed: SealedGapSecret[] = [];
    for (const s of secrets) sealed.push(await sealGapSecret(s, key));
    return recordGapLook({ groupIdB64, secrets: sealed, gapFromMs: 1, gapToMs });
  }

  /** The rendezvous the returning device reads for a gap secret. */
  function rendezvousOf(s: Uint8Array): Uint8Array {
    return wasm.gapHistoryRendezvous(wasm.gapPublicKey(s));
  }

  /** One page holding `rows`, numbered from 1, with nothing after it. */
  function onePage(rows: readonly Uint8Array[]): GapLookPage {
    return { rows: rows.map((payload, i) => ({ id: i + 1, payload })), hasMore: false };
  }

  /** A rendezvous journal of `rows` (ids 1..n), served page by page as the
   *  DS serves it; records every `(after, limit)` asked. */
  function journal(rows: readonly Uint8Array[]) {
    const asked: { after: number; limit: number }[] = [];
    const readPage = async (_r: Uint8Array, after: number, limit: number): Promise<GapLookPage> => {
      asked.push({ after, limit });
      const slice = rows.slice(after, after + limit);
      return {
        rows: slice.map((payload, i) => ({ id: after + i + 1, payload })),
        hasMore: after + limit < rows.length,
      };
    };
    return { readPage, asked };
  }

  function lookPorts(deviceKey: WebDeviceCipher, over: Partial<GapLookPorts> = {}) {
    const reads: string[] = [];
    const received: { group: string; toMs: number }[] = [];
    let now = 2_000_000;
    const p: GapLookPorts = {
      now: () => now,
      deviceKey: async () => deviceKey,
      rendezvousOf,
      readPage: async (rendezvous) => {
        reads.push(hex(rendezvous));
        return onePage([]);
      },
      apply: async () => null,
      received: (group, toMs) => void received.push({ group, toMs }),
      ...over,
    };
    return { p, reads, received, advance: (ms: number) => (now += ms) };
  }

  it("stores the secret sealed, never in the clear", async () => {
    await record("Bwc=", await cipher());
    const stored = [...storage.map.values()].join("|");
    expect(stored.includes(base64Encode(secret))).toBe(false);
    expect(stored.includes(hex(secret))).toBe(false);
    expect(gapLookFor("Bwc=")).toBeDefined();
  });

  it("finds nothing, waits a minute, keeps looking; the bundle ends the look", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const key = await cipher();
    await record("Bwc=", key);
    let rows: Uint8Array[] = [];
    const t = lookPorts(key, {
      readPage: async () => onePage(rows),
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

  it("reads the rendezvous of the gap key's public half, newest request first", async () => {
    const key = await cipher();
    const older = new Uint8Array(32).fill(0x11);
    await record("Bwc=", key, 2_000_000, [older, secret]);
    const t = lookPorts(key);
    await lookForGapHistory("Bwc=", t.p);
    expect(t.reads).toEqual([hex(rendezvousOf(secret)), hex(rendezvousOf(older))]);
  });

  it("a bundle whose signer is not accepted is dropped and the look goes on", async () => {
    const key = await cipher();
    await record("Bwc=", key);
    // `apply` answers null for a row whose signer the joined group refuses.
    const t = lookPorts(key, { readPage: async () => onePage([new Uint8Array([7])]), apply: async () => null });
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("none");
    expect(t.received).toEqual([]);
    expect(gapLookFor("Bwc=")).toBeDefined();
  });

  it("a failed read keeps the look", async () => {
    const key = await cipher();
    await record("Bwc=", key);
    const t = lookPorts(key, { readPage: async () => null });
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("none");
    expect(gapLookFor("Bwc=")).toBeDefined();
  });

  it("junk rows on the first pages do not hide the bundle on a later page", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const key = await cipher();
    await record("Bwc=", key);
    // 250 junk rows a member who saw the request posted, the bundle at row
    // 120, more junk after it.
    const rows: Uint8Array[] = [];
    for (let i = 0; i < 250; i += 1) rows.push(new Uint8Array([1, i & 0xff]));
    rows[119] = new Uint8Array([9]);
    const j = journal(rows);
    const tried: number[] = [];
    const t = lookPorts(key, {
      readPage: j.readPage,
      apply: async (_s, sealed) => {
        tried.push(sealed[0] ?? 0);
        return sealed[0] === 9 ? filed : null;
      },
    });
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("received");
    expect(j.asked).toEqual([
      { after: 0, limit: GAP_LOOK_PAGE_SIZE },
      { after: 100, limit: GAP_LOOK_PAGE_SIZE },
      { after: 200, limit: GAP_LOOK_PAGE_SIZE },
    ]);
    // Newest first: the 130 rows after the bundle were passed over first.
    expect(tried).toHaveLength(131);
    expect(gapLookFor("Bwc=")).toBeUndefined();
  });

  it("stops after the page cap; a bundle past it is not read", async () => {
    const key = await cipher();
    await record("Bwc=", key);
    expect(GAP_LOOK_PAGE_SIZE).toBe(100);
    expect(GAP_LOOK_PAGE_CAP).toBe(10);
    const rows: Uint8Array[] = [];
    for (let i = 0; i < GAP_LOOK_PAGE_SIZE * GAP_LOOK_PAGE_CAP + 50; i += 1) rows.push(new Uint8Array([1]));
    rows[rows.length - 1] = new Uint8Array([9]);
    const j = journal(rows);
    let tried = 0;
    const t = lookPorts(key, {
      readPage: j.readPage,
      apply: async (_s, sealed) => {
        tried += 1;
        return sealed[0] === 9 ? filed : null;
      },
    });
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("none");
    expect(j.asked).toHaveLength(GAP_LOOK_PAGE_CAP);
    expect(tried).toBe(GAP_LOOK_PAGE_SIZE * GAP_LOOK_PAGE_CAP);
    expect(gapLookFor("Bwc=")).toBeDefined();
  });

  it("a later page that fails still tries the rows already read", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const key = await cipher();
    await record("Bwc=", key);
    const rows: Uint8Array[] = [];
    for (let i = 0; i < 150; i += 1) rows.push(new Uint8Array([1]));
    rows[10] = new Uint8Array([9]);
    const j = journal(rows);
    const t = lookPorts(key, {
      readPage: async (r, after, limit) => (after === 0 ? j.readPage(r, after, limit) : null),
      apply: async (_s, sealed) => (sealed[0] === 9 ? filed : null),
    });
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("received");
  });

  it("past 30 days the look goes without a read; the gap line stays", async () => {
    vi.spyOn(console, "info").mockImplementation(() => {});
    const key = await cipher();
    await record("Bwc=", key, 0);
    const t = lookPorts(key, { now: () => GAP_HISTORY_LOOK_RETENTION_MS + 1 });
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("dropped");
    expect(t.reads).toEqual([]);
    expect(gapLookFor("Bwc=")).toBeUndefined();
  });

  it("a secret sealed under another device key is dropped", async () => {
    await record("Bwc=", await cipher());
    const t = lookPorts(await cipher());
    expect(await lookForGapHistory("Bwc=", t.p)).toBe("dropped");
    expect(t.reads).toEqual([]);
  });

  it("goes on Forget device", async () => {
    await record("Bwc=", await cipher());
    owe({ gapFromMs: 1 });
    clearReentryRowsForAccount("acct-1");
    expect(storage.map.size).toBe(0);
  });
});

describe("the first sighting of a leaf (section 7)", () => {
  it("keeps only the first observation, durably, and goes with the group", () => {
    noteLeavesSeen("Bwc=", ["leafA", "leafB"], 1_000);
    noteLeavesSeen("Bwc=", ["leafA", "leafC"], 5_000);
    resetLeafSeenMemory();
    noteLeavesSeen("Bwc=", ["leafA"], 9_000);
    expect(leafFirstSeenMs("Bwc=", "leafA")).toBe(1_000);
    expect(leafFirstSeenMs("Bwc=", "leafB")).toBe(1_000);
    expect(leafFirstSeenMs("Bwc=", "leafC")).toBe(5_000);
    expect(leafFirstSeenMs("Bwc=", "leafD")).toBeUndefined();
    clearReentryRowsForGroup("Bwc=");
    expect(leafFirstSeenMs("Bwc=", "leafA")).toBeUndefined();
  });
});
