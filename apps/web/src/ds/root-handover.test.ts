// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0057 "What the S2 client work found", item 1 - the journal root moves
 * with an ownership transfer (`ds/root-handover.ts`).
 *
 * The sealed seed is checked end to end against the REAL core: what the
 * outgoing owner composes, the new owner (and only the new owner) opens, and
 * it is the outgoing owner's derived root seed. The network, the group and
 * the timing are fakes.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { DsClient } from "./client";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { ChatClient } from "../chat/client";
import { fakeRootAuthority, parseFakeStatement } from "./access-test-doubles";
import { receiveRegistrarSeed } from "./registrar-seeds";
import {
  HANDOVER_RESEND_MS,
  PendingRootSeedStore,
  RANK_ROOT_HANDOVER,
  decodePendingRootSeedRows,
  encodePendingRootSeedRows,
  runOwedHandover,
  type PendingRootSeedBackend,
  clearHandoverOwed,
  clearOwedHandovers,
  installOwnerTransitionHandler,
  loadOwedHandovers,
  markHandoverOwed,
  noteOwnerTransition,
  ownerMovedAwayFrom,
  receiveRootHandover,
  rootHandoverVerdict,
  sendRootHandover,
  takeOverJournal,
  type HandoverSendPorts,
  type OwedStorage,
  type OwnerTransition,
  type RootTransferSignerLike,
  type TakeOverPorts,
} from "./root-handover";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const GID = new Uint8Array(32).fill(0x51);
const OTHER_GID = new Uint8Array(32).fill(0x52);
const b64 = (b: Uint8Array): string => Buffer.from(b).toString("base64");
const acct = (n: number): Uint8Array => new Uint8Array(32).fill(n);
const hex = (b: Uint8Array): string => Buffer.from(b).toString("hex");

function identity(fill: number): WasmIdentity {
  return wasm.Identity.fromSeed(new Uint8Array(64).fill(fill));
}

describe("the sending rule: only an owner change AWAY from this account", () => {
  const own = acct(1);
  const other = acct(2);
  it("fires when this account was owner before and another account is owner after", () => {
    expect(ownerMovedAwayFrom(own, own, other)).toEqual(other);
  });
  it("does not fire on a commit that leaves the owner unchanged", () => {
    expect(ownerMovedAwayFrom(own, own, own)).toBeNull();
    expect(ownerMovedAwayFrom(own, other, other)).toBeNull();
  });
  it("does not fire when this account becomes owner", () => {
    expect(ownerMovedAwayFrom(own, other, own)).toBeNull();
  });
  it("does not fire between two other accounts", () => {
    expect(ownerMovedAwayFrom(own, other, acct(3))).toBeNull();
  });
  it("does not fire on an unreadable table or with no account", () => {
    expect(ownerMovedAwayFrom(own, own, undefined)).toBeNull();
    expect(ownerMovedAwayFrom(null, own, other)).toBeNull();
  });
});

describe("the apply seam reports an owner change and nothing else", () => {
  let uninstall: (() => void) | null = null;
  afterEach(() => {
    uninstall?.();
    uninstall = null;
  });

  function capture(): OwnerTransition[] {
    const seen: OwnerTransition[] = [];
    uninstall = installOwnerTransitionHandler((t) => seen.push(t));
    return seen;
  }

  it("noteOwnerTransition is silent when the owner did not change", () => {
    const seen = capture();
    noteOwnerTransition(GID, acct(1), acct(1));
    noteOwnerTransition(GID, undefined, undefined);
    expect(seen).toEqual([]);
    noteOwnerTransition(GID, acct(1), acct(2));
    expect(seen).toHaveLength(1);
    expect(seen[0]?.groupIdB64).toBe(b64(GID));
  });

  /** A group whose owner flips to `after` once the commit at `commitIndex`
   *  (0-based among the batch rows) is applied. */
  function group(before: Uint8Array, after: Uint8Array, commitIndex: number): WasmGroup {
    let applied = 0;
    let owner = before;
    return {
      get groupId(): Uint8Array {
        return GID;
      },
      roleTableOwner: () => owner,
      members: () => [],
      processIncomingVerifyingRoles: vi.fn(() => {
        if (applied === commitIndex) owner = after;
        applied += 1;
        return { kind: "Commit" };
      }),
    } as unknown as WasmGroup;
  }

  function dsWith(count: number): DsClient {
    return {
      fetchGroup: vi.fn(async () => ({
        messages: Array.from({ length: count }, (_, i) => ({
          id: i + 1,
          payload: new Uint8Array([i]),
          createdAt: "2026-09-28T00:00:00Z",
        })),
        hasMore: false,
      })),
    } as unknown as DsClient;
  }

  it("the poll reports the transfer at the row that carried it, once", async () => {
    const seen = capture();
    const chat = new ChatClient(dsWith(3), { identityProvider: () => ({}) as unknown as WasmIdentity });
    await chat.pollMessages(null, group(acct(1), acct(2), 1), 0);
    expect(seen).toHaveLength(1);
    expect(seen[0]?.before).toEqual(acct(1));
    expect(seen[0]?.after).toEqual(acct(2));
  });

  it("the poll reports nothing for a batch of no-op commits", async () => {
    const seen = capture();
    const chat = new ChatClient(dsWith(2), { identityProvider: () => ({}) as unknown as WasmIdentity });
    await chat.pollMessages(null, group(acct(1), acct(1), 0), 0);
    expect(seen).toEqual([]);
  });
});

describe("sending: the old root seed sealed to the new owner", () => {
  function ports(over: Partial<HandoverSendPorts> & { own: WasmIdentity; newOwner: WasmIdentity }): {
    ports: HandoverSendPorts;
    sent: Uint8Array[];
    seeds: Uint8Array[];
  } {
    const sent: Uint8Array[] = [];
    const seeds: Uint8Array[] = [];
    const base: HandoverSendPorts = {
      groupId: GID,
      ownAccountKey: () => over.own.signaturePublicKey,
      liveOwner: () => over.newOwner.signaturePublicKey,
      ownerX25519: async (key) =>
        key === b64(over.newOwner.signaturePublicKey) ? over.newOwner.x25519PublicKey : null,
      rootSeed: () => {
        const s = wasm.dsJournalRootSeed(over.own.ed25519Seed, GID);
        seeds.push(s);
        return s;
      },
      compose: (newOwner, x25519, rootSeed) => wasm.composeRootHandoverEnvelope(GID, newOwner, x25519, rootSeed),
      sendEnvelope: async (envelope) => {
        sent.push(envelope);
      },
    };
    return { ports: { ...base, ...over }, sent, seeds };
  }

  it("the new owner, and only the new owner, opens the outgoing owner's root seed", async () => {
    const own = identity(0x11);
    const newOwner = identity(0x12);
    const stranger = identity(0x13);
    try {
      const h = ports({ own, newOwner });
      expect(await sendRootHandover(h.ports)).toBe("sent");
      expect(h.sent).toHaveLength(1);
      const envelope = h.sent[0] as Uint8Array;

      const header = wasm.decodeRegistrarSeedEnvelope(envelope);
      try {
        expect(header.rank).toBe(RANK_ROOT_HANDOVER);
        expect(header.journalId).toEqual(GID);
        expect(header.memberAccountKey).toEqual(newOwner.signaturePublicKey);
      } finally {
        header.free();
      }
      const opened = wasm.openRegistrarSeedEnvelope(envelope, newOwner);
      expect(opened).toEqual(wasm.dsJournalRootSeed(own.ed25519Seed, GID));
      expect(() => wasm.openRegistrarSeedEnvelope(envelope, stranger)).toThrow();
      // The derived seed this device held was wiped after composing.
      expect(h.seeds[0]).toEqual(new Uint8Array(32));
    } finally {
      own.free();
      newOwner.free();
      stranger.free();
    }
  });

  it("is never sent while this account is still owner", async () => {
    const own = identity(0x21);
    const newOwner = identity(0x22);
    try {
      const h = ports({ own, newOwner, liveOwner: () => own.signaturePublicKey });
      expect(await sendRootHandover(h.ports)).toBe("cancelled");
      expect(h.sent).toEqual([]);
      expect(h.seeds).toEqual([]);
    } finally {
      own.free();
      newOwner.free();
    }
  });

  it("waits when the new owner's box key is not resolvable yet", async () => {
    const own = identity(0x31);
    const newOwner = identity(0x32);
    try {
      const h = ports({ own, newOwner, ownerX25519: async () => null });
      expect(await sendRootHandover(h.ports)).toBe("pending");
      expect(h.sent).toEqual([]);
    } finally {
      own.free();
      newOwner.free();
    }
  });

  it("reports a failed send so it is retried", async () => {
    const own = identity(0x41);
    const newOwner = identity(0x42);
    try {
      const h = ports({
        own,
        newOwner,
        sendEnvelope: async () => {
          throw new Error("offline");
        },
      });
      expect(await sendRootHandover(h.ports)).toBe("failed");
      expect(h.seeds[0]).toEqual(new Uint8Array(32));
    } finally {
      own.free();
      newOwner.free();
    }
  });
});

describe("finding 4 — the outgoing owner keeps re-sending until its probe answers 401", () => {
  it("re-sends at most every 10 minutes while the probe answers 204, and is done on 401", async () => {
    const own = identity(0x81);
    const newOwner = identity(0x82);
    try {
      let probe: "root" | "not-root" = "root";
      let now = 1_000_000;
      let lastSent: number | null = null;
      const sent: Uint8Array[] = [];
      const run = () =>
        runOwedHandover({
          groupId: GID,
          ownAccountKey: () => own.signaturePublicKey,
          liveOwner: () => newOwner.signaturePublicKey,
          ownerX25519: async () => newOwner.x25519PublicKey,
          rootSeed: () => wasm.dsJournalRootSeed(own.ed25519Seed, GID),
          compose: (n, x, r) => wasm.composeRootHandoverEnvelope(GID, n, x, r),
          sendEnvelope: async (e) => void sent.push(e),
          probeOwnRoot: async () => probe,
          lastSentAt: () => lastSent,
          noteSent: (at) => {
            lastSent = at;
          },
          now: () => now,
        });
      expect(HANDOVER_RESEND_MS).toBe(600_000);
      expect(await run()).toBe("sent");
      expect(sent).toHaveLength(1);
      now += 60_000;
      expect(await run()).toBe("throttled");
      expect(sent).toHaveLength(1);
      now += HANDOVER_RESEND_MS;
      expect(await run()).toBe("sent");
      expect(sent).toHaveLength(2);
      probe = "not-root";
      now += HANDOVER_RESEND_MS;
      expect(await run()).toBe("done");
      expect(sent).toHaveLength(2);
    } finally {
      own.free();
      newOwner.free();
    }
  });

  it("a probe that cannot run keeps the record and sends nothing", async () => {
    const sent: Uint8Array[] = [];
    const out = await runOwedHandover({
      groupId: GID,
      ownAccountKey: () => acct(1),
      liveOwner: () => acct(2),
      ownerX25519: async () => acct(3),
      rootSeed: () => new Uint8Array(32),
      compose: () => new Uint8Array(1),
      sendEnvelope: async (e) => void sent.push(e),
      probeOwnRoot: async () => {
        throw new TypeError("fetch failed");
      },
      lastSentAt: () => null,
      noteSent: () => undefined,
      now: () => 0,
    });
    expect(out).toBe("retry");
    expect(sent).toEqual([]);
  });
});

describe("finding 4 — the new owner keeps the received seed until the takeover lands", () => {
  function memBackend(): PendingRootSeedBackend & { rows: Map<string, string> } {
    const rows = new Map<string, string>();
    return {
      rows,
      load: async (account) => {
        const raw = rows.get(account);
        return raw === undefined ? null : decodePendingRootSeedRows(raw);
      },
      save: async (account, r) => void rows.set(account, encodePendingRootSeedRows(r)),
    };
  }

  it("survives a restart (a fresh store over the same backend), then is wiped after", async () => {
    const backend = memBackend();
    const before = new PendingRootSeedStore();
    await before.open("acc", backend);
    const seed = new Uint8Array(32).fill(0x5c);
    await before.set("g1", seed);
    before.close();

    const after = new PendingRootSeedStore();
    await after.open("acc", backend);
    expect(after.groups()).toEqual(["g1"]);
    const copy = after.get("g1");
    expect(copy).toEqual(new Uint8Array(32).fill(0x5c));
    await after.delete("g1");
    expect(after.get("g1")).toBeUndefined();
    const reloaded = new PendingRootSeedStore();
    await reloaded.open("acc", backend);
    expect(reloaded.groups()).toEqual([]);
  });

  it("get hands out a copy; the stored seed is not the caller's to wipe", async () => {
    const store = new PendingRootSeedStore();
    await store.open("acc", memBackend());
    await store.set("g", new Uint8Array(32).fill(9));
    store.get("g")?.fill(0);
    expect(store.get("g")).toEqual(new Uint8Array(32).fill(9));
  });

  it("the encoded rows skip junk", () => {
    expect([...decodePendingRootSeedRows('{"a":"AAAA","b":7}')]).toEqual([]);
    expect(decodePendingRootSeedRows("not json").size).toBe(0);
  });
});

describe("the owed record", () => {
  function memStorage(): OwedStorage & { data: Map<string, string> } {
    const data = new Map<string, string>();
    return {
      data,
      getItem: (k) => data.get(k) ?? null,
      setItem: (k, v) => void data.set(k, v),
      removeItem: (k) => void data.delete(k),
    };
  }
  it("marks, lists and clears per account", () => {
    const st = memStorage();
    markHandoverOwed("a", "g1", st);
    markHandoverOwed("a", "g2", st);
    markHandoverOwed("b", "g1", st);
    expect([...loadOwedHandovers("a", st)].sort()).toEqual(["g1", "g2"]);
    clearHandoverOwed("a", "g1", st);
    expect([...loadOwedHandovers("a", st)]).toEqual(["g2"]);
    clearOwedHandovers("a", st);
    expect(loadOwedHandovers("a", st).size).toBe(0);
    expect([...loadOwedHandovers("b", st)]).toEqual(["g1"]);
  });
});

describe("receiving: the gate", () => {
  const own = acct(7);
  const header = { journalId: GID, memberAccountKey: own, rank: RANK_ROOT_HANDOVER };

  it("accepts only this journal, addressed to this account, while the live table names it owner", () => {
    expect(rootHandoverVerdict({ header, groupId: GID, ownAccountKey: own, ownerAccountKey: own })).toBe("accept");
    expect(rootHandoverVerdict({ header, groupId: OTHER_GID, ownAccountKey: own, ownerAccountKey: own })).toBe(
      "wrong-journal",
    );
    expect(
      rootHandoverVerdict({ header, groupId: GID, ownAccountKey: acct(8), ownerAccountKey: acct(8) }),
    ).toBe("not-addressed");
    expect(rootHandoverVerdict({ header, groupId: GID, ownAccountKey: own, ownerAccountKey: acct(9) })).toBe(
      "not-owner",
    );
    expect(rootHandoverVerdict({ header, groupId: GID, ownAccountKey: own, ownerAccountKey: undefined })).toBe(
      "not-owner",
    );
    expect(
      rootHandoverVerdict({ header: { ...header, rank: 2 }, groupId: GID, ownAccountKey: own, ownerAccountKey: own }),
    ).toBe("malformed");
  });

  it("a refused envelope is never opened and never starts a takeover", () => {
    const openFn = vi.fn(() => new Uint8Array(32).fill(1));
    const takeOver = vi.fn();
    const verdict = receiveRootHandover(
      { open: openFn, ownAccountKey: () => own, takeOver },
      { header, groupId: GID, groupIdB64: b64(GID), plaintext: new Uint8Array(1), ownerAccountKey: acct(9) },
    );
    expect(verdict).toBe("not-owner");
    expect(openFn).not.toHaveBeenCalled();
    expect(takeOver).not.toHaveBeenCalled();
  });

  it("an accepted envelope hands the opened seed to the takeover", () => {
    const takeOver = vi.fn();
    const verdict = receiveRootHandover(
      { open: () => new Uint8Array(32).fill(0xab), ownAccountKey: () => own, takeOver },
      { header, groupId: GID, groupIdB64: b64(GID), plaintext: new Uint8Array(1), ownerAccountKey: own },
    );
    expect(verdict).toBe("accept");
    expect(takeOver).toHaveBeenCalledWith(GID, b64(GID), new Uint8Array(32).fill(0xab));
  });

  it("a rank-root envelope is never kept as a registrar seed", async () => {
    const oldOwner = identity(0x61);
    const newOwner = identity(0x62);
    try {
      const envelope = wasm.composeRootHandoverEnvelope(
        GID,
        newOwner.signaturePublicKey,
        newOwner.x25519PublicKey,
        wasm.dsJournalRootSeed(oldOwner.ed25519Seed, GID),
      );
      const keep = vi.fn(async () => undefined);
      const verdict = await receiveRegistrarSeed(
        {
          decode: (p) => {
            const d = wasm.decodeRegistrarSeedEnvelope(p);
            try {
              return { journalId: d.journalId, memberAccountKey: d.memberAccountKey, rank: d.rank };
            } finally {
              d.free();
            }
          },
          open: (p) => wasm.openRegistrarSeedEnvelope(p, newOwner),
          ownAccountKey: () => newOwner.signaturePublicKey,
          keep,
        },
        {
          groupId: GID,
          groupIdB64: b64(GID),
          plaintext: envelope,
          senderAccountKey: newOwner.signaturePublicKey,
          ownerAccountKey: newOwner.signaturePublicKey,
        },
      );
      expect(verdict).toBe("malformed");
      expect(keep).not.toHaveBeenCalled();
    } finally {
      oldOwner.free();
      newOwner.free();
    }
  });
});

describe("receiving: the takeover", () => {
  const OWN_ROOT = acct(0xc1);
  // The live roster's desired set: two members, one admin.
  const DESIRED = new Map<string, number>([
    [b64(acct(0x02)), 1],
    [b64(acct(0x03)), 2],
  ]);

  /** An old-root signer whose `rootTransfer` records what it named. */
  function oldSigner(seedSeen: Uint8Array[]): (seed: Uint8Array) => RootTransferSignerLike {
    return (seed) => {
      seedSeen.push(seed.slice());
      return {
        rootTransfer: (j, newRoot) => ({
          path: `/ds/group/${hex(j)}/root`,
          bodyJson: JSON.stringify({ signer: `old:${hex(seed).slice(0, 4)}`, newRoot: hex(newRoot) }),
          free: () => undefined,
        }),
        free: () => undefined,
      };
    };
  }

  function harness(
    statuses: number[],
    probe: "root" | "not-root" = "not-root",
  ): {
    ports: TakeOverPorts;
    puts: Array<{ path: string; body: string }>;
    reset: ReturnType<typeof vi.fn>;
    reconcile: ReturnType<typeof vi.fn>;
    marked: ReturnType<typeof vi.fn>;
    seedSeen: Uint8Array[];
    sleep: ReturnType<typeof vi.fn>;
  } {
    const puts: Array<{ path: string; body: string }> = [];
    const script = [...statuses];
    const seedSeen: Uint8Array[] = [];
    const reset = vi.fn(async () => undefined);
    const reconcile = vi.fn();
    const marked = vi.fn();
    const sleep = vi.fn(async () => undefined);
    const ports: TakeOverPorts = {
      groupId: GID,
      sender: {
        putStatement: async (path, body) => {
          puts.push({ path, body });
          const status = script.shift() ?? 204;
          if (status >= 300) throw Object.assign(new Error(`HTTP ${status}`), { status });
        },
      },
      signerOf: oldSigner(seedSeen),
      ownRootPublicKey: () => OWN_ROOT,
      stillOwner: () => true,
      desiredRegistrars: () => DESIRED,
      markRootHeld: marked,
      probeOwnRoot: async () => probe,
      derive: {
        // Readable fakes: the registrar seed names the root and the member.
        registrarSeed: (root, member) => Uint8Array.from([0x5e, root[0] ?? 0, member[0] ?? 0, ...new Uint8Array(29)]),
        publicKeyOfSeed: (seed) => Uint8Array.from([0x9b, seed[1] ?? 0, seed[2] ?? 0, ...new Uint8Array(29)]),
      },
      authority: () => fakeRootAuthority("newroot"),
      resetAppointments: reset,
      reconcile,
      retryDelaysMs: [1, 1],
      sleep,
    };
    return { ports, puts, reset, reconcile, marked, seedSeen, sleep };
  }

  it("204: root names this account's own root, the marker is set, ONE registrars-set under the new root, then re-appointment", async () => {
    const h = harness([]);
    const seed = new Uint8Array(32).fill(0xa7);
    expect(await takeOverJournal(h.ports, seed)).toBe("took-over");

    expect(h.puts[0]?.path).toBe(`/ds/group/${hex(GID)}/root`);
    expect(JSON.parse(h.puts[0]?.body ?? "{}")).toEqual({ signer: "old:a7a7", newRoot: hex(OWN_ROOT) });
    expect(h.seedSeen[0]).toEqual(new Uint8Array(32).fill(0xa7));
    expect(h.marked).toHaveBeenCalledTimes(1);

    const set = parseFakeStatement(h.puts[1]?.body ?? "{}");
    expect(set["segment"]).toBe("registrars-set");
    expect(set["signer"]).toBe("newroot");
    // The roster's whole set, keys derived from the NEW root seed (the fake
    // root authority's seed is 0x07), sorted by account key; no removals.
    const expected = [0x02, 0x03]
      .map((m) => hex(Uint8Array.from([0x9b, 0x07, m, ...new Uint8Array(29)])))
      .join("");
    expect(set["keys"]).toBe(expected);
    expect(set["ranks"]).toBe("0102");
    expect(h.puts).toHaveLength(2);

    expect(h.reset).toHaveBeenCalledTimes(1);
    expect(h.reconcile).toHaveBeenCalledTimes(1);
    expect(seed).toEqual(new Uint8Array(32));
  });

  it("401: the seed is not the current root - dropped, never retried, nothing else sent", async () => {
    const h = harness([401]);
    const seed = new Uint8Array(32).fill(0xa8);
    expect(await takeOverJournal(h.ports, seed)).toBe("stale");
    expect(h.puts).toHaveLength(1);
    expect(h.sleep).not.toHaveBeenCalled();
    expect(h.reset).not.toHaveBeenCalled();
    expect(h.reconcile).not.toHaveBeenCalled();
    expect(h.marked).not.toHaveBeenCalled();
    expect(seed).toEqual(new Uint8Array(32));
  });

  it("401 while this account's own probe answers 204: another device took over; marker set", async () => {
    const h = harness([401], "root");
    const seed = new Uint8Array(32).fill(0xab);
    expect(await takeOverJournal(h.ports, seed)).toBe("held-elsewhere");
    expect(h.marked).toHaveBeenCalledTimes(1);
    expect(h.reconcile).toHaveBeenCalledTimes(1);
    expect(seed).toEqual(new Uint8Array(32));
  });

  it("a takeover that keeps failing transiently reports failed and sets no marker", async () => {
    const h = harness([503, 503, 503]);
    const seed = new Uint8Array(32).fill(0xac);
    expect(await takeOverJournal(h.ports, seed)).toBe("failed");
    expect(h.marked).not.toHaveBeenCalled();
    expect(h.puts.filter((p) => p.path.endsWith("/root"))).toHaveLength(3);
  });

  it("a transient failure is retried with the same seed in memory, then succeeds", async () => {
    const h = harness([503]);
    const seed = new Uint8Array(32).fill(0xa9);
    expect(await takeOverJournal(h.ports, seed)).toBe("took-over");
    expect(h.sleep).toHaveBeenCalledTimes(1);
    expect(h.puts.filter((p) => p.path.endsWith("/root"))).toHaveLength(2);
    expect(seed).toEqual(new Uint8Array(32));
  });

  it("no takeover once the live table no longer names this account owner", async () => {
    const h = harness([]);
    const seed = new Uint8Array(32).fill(0xaa);
    expect(await takeOverJournal({ ...h.ports, stillOwner: () => false }, seed)).toBe("not-owner");
    expect(h.puts).toEqual([]);
    expect(seed).toEqual(new Uint8Array(32));
  });

  it("the real signer puts the root statement on the root route", () => {
    const oldOwner = identity(0x71);
    const newOwner = identity(0x72);
    try {
      const seed = wasm.dsJournalRootSeed(oldOwner.ed25519Seed, GID);
      const signer = wasm.dsStatementSigner(seed);
      const ownRoot = wasm.dsJournalRootSigner(newOwner, GID);
      try {
        const req = signer.rootTransfer(GID, ownRoot.publicKey());
        try {
          expect(req.path).toBe(`/ds/group/${hex(GID)}/root`);
          expect(JSON.parse(req.bodyJson)).toHaveProperty("statement_b64");
        } finally {
          req.free();
        }
      } finally {
        signer.free();
        ownRoot.free();
      }
    } finally {
      oldOwner.free();
      newOwner.free();
    }
  });
});
