// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0064 section 7, amendment of 2026-11-14, on web: the owned channel's
 * vault head and the entry from it.
 *
 * - the writer saves the staged epoch BEFORE the commit's journal step, a
 *   failed write never stops the commit, an entry writes nothing before;
 * - the head keeps the two newest epochs, a 409 reads, merges, writes once
 *   more, an unclaimed vault is claimed first;
 * - the entry goes newest saved epoch first, falls back to the older on a
 *   lost or refused one, stops on an owed one, and waits with none;
 * - only an owner device that holds no group enters (the verdict), and the
 *   writer is an owner device;
 * - through the REAL core: an owner device saves the epoch its staged
 *   commit moves to, merges, and a second owner device with nothing but
 *   that saved GroupInfo enters and derives the same channel seed; the head
 *   round-trips through the real vault keys against an in-memory service.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm, wasmSync: () => wasm }));

import {
  enterNewestFirst,
  keepTwoNewest,
  readSavedEpochs,
  setHeadWriterForTests,
  wasmChannelHeadStore,
  writeChannelHead,
  writeCurrentHead,
  writeStagedHead,
  type ChannelHeadStore,
  type ReentryEpoch,
} from "./channel-vault-head";
import { clearInactiveHeldGroup, enterOwnedChannel, entryVerdict, type EntryPorts } from "./channel-entry";
import { afterPeerCommitsApplied, newOwnLeaves, publisherLeaves } from "./publisher-devices";
import { runPublisherCommit, type PublisherCommitEntry, type PublisherCommitPorts } from "./publisher-commit-outbox";
import { VaultClient, decodeVersioned, STORAGE_PROOF_HEADER } from "../storage/vault-client";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

afterEach(() => {
  setHeadWriterForTests(null);
});

const ep = (epoch: number, tag = epoch): ReentryEpoch => ({
  epoch: BigInt(epoch),
  dsAccessSeed: new Uint8Array(32).fill(tag),
  groupInfo: new Uint8Array([tag, tag, tag]),
});

const epochs = (list: readonly ReentryEpoch[]): number[] => list.map((r) => Number(r.epoch));

/** An in-memory head store: `conflicts` writes answer 409 after another
 *  device wrote `racer`. */
function fakeStore(opts: {
  initial?: { version: bigint; saved: ReentryEpoch[] } | "unclaimed" | null;
  conflicts?: number;
  racer?: ReentryEpoch;
  failRead?: boolean;
}) {
  let state: { version: bigint; saved: ReentryEpoch[] } | null =
    opts.initial === "unclaimed" || opts.initial === undefined ? null : opts.initial;
  let claimed = opts.initial !== "unclaimed";
  let conflicts = opts.conflicts ?? 0;
  const calls: string[] = [];
  const writes: { version: bigint; saved: ReentryEpoch[] }[] = [];
  const store: ChannelHeadStore = {
    async read() {
      calls.push("read");
      if (opts.failRead === true) throw new Error("network");
      if (!claimed) return { kind: "unclaimed" };
      if (state === null) return { kind: "none" };
      return { kind: "head", version: state.version, saved: state.saved };
    },
    async write(version, saved) {
      calls.push(`write:${version}`);
      if (!claimed) return "unclaimed";
      if (conflicts > 0) {
        conflicts -= 1;
        // Another owner device wrote first.
        const racer = opts.racer ?? ep(99);
        state = { version: (state?.version ?? 0n) + 1n, saved: keepTwoNewest(state?.saved ?? [], [racer]) };
        return "conflict";
      }
      if (version !== (state?.version ?? 0n) + 1n) return "conflict";
      state = { version, saved: [...saved] };
      writes.push({ version, saved: [...saved] });
      return "written";
    },
    async claim() {
      calls.push("claim");
      claimed = true;
    },
  };
  return { store, calls, writes, state: () => state };
}

describe("the head keeps the two newest epochs", () => {
  it("merges two sets, newest first, the incoming copy winning an epoch both name", () => {
    const merged = keepTwoNewest([ep(3), ep(5)], [ep(4), ep(5, 0x55)]);
    expect(epochs(merged)).toEqual([5, 4]);
    expect(merged[0]?.groupInfo[0]).toBe(0x55);
    expect(keepTwoNewest([], [ep(1)])).toHaveLength(1);
  });

  it("writes the first head at version 1, claiming an unclaimed vault first", async () => {
    const f = fakeStore({ initial: "unclaimed" });
    expect(await writeChannelHead(f.store, ep(7))).toBe(true);
    expect(f.calls).toEqual(["read", "claim", "write:1"]);
    expect(epochs(f.state()?.saved ?? [])).toEqual([7]);
  });

  it("on 409 reads again, keeps the two newest of both, and writes once more", async () => {
    const f = fakeStore({ initial: { version: 3n, saved: [ep(4), ep(3)] }, conflicts: 1, racer: ep(6) });
    expect(await writeChannelHead(f.store, ep(5))).toBe(true);
    // The racer's 6 and this device's 5 are kept; 4 and 3 drop out.
    expect(epochs(f.state()?.saved ?? [])).toEqual([6, 5]);
    expect(f.state()?.version).toBe(5n);
    expect(f.calls).toEqual(["read", "write:4", "read", "write:5"]);
  });

  it("gives up after one retry and never throws", async () => {
    const f = fakeStore({ initial: { version: 1n, saved: [ep(1)] }, conflicts: 5, racer: ep(2) });
    expect(await writeChannelHead(f.store, ep(3))).toBe(false);
    const broken = fakeStore({ failRead: true });
    await expect(writeChannelHead(broken.store, ep(3))).resolves.toBe(false);
  });

  it("after decision A the head is REPLACED: the dead group's epochs 40 and 41 go", async () => {
    const f = fakeStore({ initial: { version: 6n, saved: [ep(41), ep(40)] } });
    // The re-founded group starts at epoch 1.
    expect(await writeChannelHead(f.store, ep(1), "replace")).toBe(true);
    expect(epochs(f.state()?.saved ?? [])).toEqual([1]);
    expect(f.state()?.version).toBe(7n);
    // A merge would have kept the dead group's GroupInfos.
    const m = fakeStore({ initial: { version: 6n, saved: [ep(41), ep(40)] } });
    expect(await writeChannelHead(m.store, ep(1))).toBe(false);
    expect(epochs(m.state()?.saved ?? [])).toEqual([41, 40]);
  });

  it("writes nothing when the head already says the same", async () => {
    const f = fakeStore({ initial: { version: 2n, saved: [ep(4), ep(3)] } });
    expect(await writeChannelHead(f.store, ep(4))).toBe(true);
    expect(f.calls).toEqual(["read"]);
  });

  it("reads the saved epochs newest first, and nothing from no head or a failed read", async () => {
    expect(epochs(await readSavedEpochs(fakeStore({ initial: { version: 1n, saved: [ep(2), ep(9)] } }).store))).toEqual([9, 2]);
    expect(await readSavedEpochs(fakeStore({ initial: null }).store)).toEqual([]);
    expect(await readSavedEpochs(fakeStore({ initial: "unclaimed" }).store)).toEqual([]);
    expect(await readSavedEpochs(fakeStore({ failRead: true }).store)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// The writer before the journal step
// ---------------------------------------------------------------------------

const ROTATION: PublisherCommitEntry = {
  id: "0b".repeat(16),
  kind: "rotation",
  channelIdHex: "11".repeat(16),
  groupIdHex: "22".repeat(32),
  targetEpoch: "5",
  serverTargetEpoch: 8,
  commitB64: btoa("rotation"),
  welcomeB64: null,
  joinerKeyB64: null,
  replyGrantB64: null,
  proofEpoch: "4",
  proofSeedB64: btoa("s".repeat(32)),
  journalLost: false,
  done: [],
};

function ports(opts: { headFails?: boolean; lost?: boolean } = {}) {
  const calls: string[] = [];
  const p: PublisherCommitPorts = {
    async commitToJournal() {
      calls.push("commit");
      if (opts.lost === true) throw new Error("409");
    },
    classifyCommitFailure: () => "lost",
    async merge() {
      calls.push("merge");
      return true;
    },
    async abandon() {
      calls.push("abandon");
    },
    async deliverWelcome() {
      calls.push("welcome");
    },
    async serverEpoch() {
      return 7;
    },
    async reseal(target) {
      calls.push(`reseal:${target}`);
    },
    async appoint() {},
    async handover() {},
    async save() {},
    async finish() {
      calls.push("finish");
    },
    async writeVaultHead() {
      calls.push("head");
      if (opts.headFails === true) throw new Error("vault down");
      return true;
    },
  };
  return { p, calls };
}

describe("the writer saves the staged epoch before the commit leaves", () => {
  it("writes the head, then sends the commit", async () => {
    const { p, calls } = ports();
    expect(await runPublisherCommit(ROTATION, p)).toBe("landed");
    expect(calls).toEqual(["head", "commit", "merge", "reseal:8", "finish"]);
  });

  it("a failed head write does not stop the commit", async () => {
    const { p, calls } = ports({ headFails: true });
    expect(await runPublisherCommit(ROTATION, p)).toBe("landed");
    expect(calls.slice(0, 3)).toEqual(["head", "commit", "merge"]);
  });

  it("an entry writes nothing before its commit (it saves the head once merged)", async () => {
    const { p, calls } = ports({ lost: true });
    expect(await runPublisherCommit({ ...ROTATION, kind: "entry" }, p)).toBe("lost");
    expect(calls).toEqual(["commit", "abandon", "finish"]);
  });
});

// ---------------------------------------------------------------------------
// The entry
// ---------------------------------------------------------------------------

function entryPorts(saved: ReentryEpoch[] | null, outcomes: Record<number, "entered" | "lost" | "refused" | "owed">) {
  const tried: number[] = [];
  let finished = 0;
  const p: EntryPorts = {
    async readSaved() {
      return saved;
    },
    async attempt(r) {
      tried.push(Number(r.epoch));
      return outcomes[Number(r.epoch)] ?? "refused";
    },
    async finish() {
      finished += 1;
    },
  };
  return { p, tried, finished: () => finished };
}

describe("an owner device enters from the saved copy", () => {
  it("tries the newest saved epoch first", async () => {
    const e = entryPorts([ep(4), ep(5)], { 5: "entered", 4: "entered" });
    expect(await enterOwnedChannel(e.p)).toBe("entered");
    expect(e.tried).toEqual([5]);
    expect(e.finished()).toBe(1);
  });

  it("falls back to the older epoch on a 409 (lost) or a refusal", async () => {
    const lost = entryPorts([ep(4), ep(5)], { 5: "lost", 4: "entered" });
    expect(await enterOwnedChannel(lost.p)).toBe("entered");
    expect(lost.tried).toEqual([5, 4]);
    const refused = entryPorts([ep(4), ep(5)], { 5: "refused", 4: "lost" });
    expect(await enterOwnedChannel(refused.p)).toBe("none");
    expect(refused.tried).toEqual([5, 4]);
    expect(refused.finished()).toBe(0);
  });

  it("waits with no saved copy, and decides nothing when the head could not be read", async () => {
    const none = entryPorts([], {});
    expect(await enterOwnedChannel(none.p)).toBe("none");
    expect(none.tried).toEqual([]);
    const failed = entryPorts(null, {});
    expect(await enterOwnedChannel(failed.p)).toBe("owed");
  });

  it("stops at an entry the DS did not answer (it is re-sent later)", async () => {
    const e = entryPorts([ep(4), ep(5)], { 5: "owed", 4: "entered" });
    expect(await enterOwnedChannel(e.p)).toBe("owed");
    expect(e.tried).toEqual([5]);
  });

  it("a throwing attempt counts as refused", async () => {
    expect(await enterNewestFirst([ep(1)], async () => Promise.reject(new Error("x")))).toBe("none");
  });

  it("is never tried by a co-publisher's device, a device holding the group, under a suspected reset, or twice", () => {
    const base = { registerRole: "owner" as const, holdsGroup: false, resetSuspected: false, entryPending: false };
    expect(entryVerdict(base)).toBe(true);
    expect(entryVerdict({ ...base, registerRole: "co-publisher" })).toBe(false);
    expect(entryVerdict({ ...base, registerRole: "subscriber" })).toBe(false);
    expect(entryVerdict({ ...base, holdsGroup: true })).toBe(false);
    expect(entryVerdict({ ...base, resetSuspected: true })).toBe(false);
    expect(entryVerdict({ ...base, entryPending: true })).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Through the real core
// ---------------------------------------------------------------------------

function deviceOf(account: wasm.Identity, fill: number): wasm.Identity {
  return wasm.Identity.fromSeedBound(new Uint8Array(64).fill(fill), account.ed25519Seed, 1_700_000_000n);
}

describe("through the real core", () => {
  it("an owner device saves the epoch its staged commit moves to; a second owner device enters from it alone", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x71));
    const first = deviceOf(owner, 0x72);
    const second = deviceOf(owner, 0x73);
    const channelId = new Uint8Array(16).fill(0x74);
    const writes: { channelId: Uint8Array; incoming: ReentryEpoch }[] = [];
    setHeadWriterForTests(async (_identity, ch, incoming) => {
      writes.push({ channelId: ch, incoming: { ...incoming, dsAccessSeed: incoming.dsAccessSeed.slice() } });
      return true;
    });
    const g = new wasm.PublisherGroup(first, owner.ed25519Seed, channelId);
    try {
      g.stageRotate(first);
      // Saved before the commit is sent: the epoch it moves to.
      expect(await writeStagedHead(g, first, owner)).toBe(true);
      expect(writes).toHaveLength(1);
      const saved = writes[0]?.incoming;
      expect(saved?.epoch).toBe(g.epoch() + 1n);
      expect(Array.from(writes[0]?.channelId ?? [])).toEqual(Array.from(channelId));
      expect(g.mergeStagedCommit(first)).toBe(true);
      expect(g.epoch()).toBe(saved?.epoch);

      // The second device holds nothing but the saved copy.
      const entry = wasm.PublisherGroup.enter(
        second,
        saved?.groupInfo ?? new Uint8Array(),
        owner.ed25519Seed,
        channelId,
        saved?.dsAccessSeed ?? new Uint8Array(),
      );
      const commit = entry.commit;
      // G-55: the row the Delivery Service stores does not carry the owner's account key.
      const ownerKey = g.ownerAccountKey();
      const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
      expect(hex(commit).includes(hex(ownerKey))).toBe(false);
      const entered = entry.takeGroup();
      entry.free();
      try {
        expect(entered.hasStagedCommit()).toBe(true);
        expect(entered.epoch()).toBe(saved?.epoch);
        // The saved access seed proves the epoch the entry leaves.
        const proof = wasm.DsProofSigner.forGroup(saved?.epoch ?? 0n, saved?.dsAccessSeed ?? new Uint8Array());
        const current = g.dsAccessSigner(first);
        expect(Array.from(proof.verifyingKey())).toEqual(Array.from(current.verifyingKey()));
        proof.free();
        current.free();
        // The DS took it: the entering device merges, the holding device applies it.
        const staged = entered.stagedChannelSeed(second);
        expect(entered.mergeStagedCommit(second)).toBe(true);
        g.processIncomingVerifyingRoles(first, commit).free();
        expect(Array.from(entered.channelSeed(second))).toEqual(Array.from(g.channelSeed(first)));
        expect(Array.from(staged ?? [])).toEqual(Array.from(g.channelSeed(first)));
        // After the merge the entered device saves the current epoch.
        expect(await writeCurrentHead(entered, second, owner)).toBe(true);
        expect(writes[1]?.incoming.epoch).toBe(entered.epoch());
      } finally {
        entered.free();
      }
    } finally {
      g.free();
    }
  });

  it("applying another device's entry runs the own-device reconcile in the same pass", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0xd1));
    const first = deviceOf(owner, 0xd2);
    const stranger = deviceOf(owner, 0xd3);
    const channelId = new Uint8Array(16).fill(0xd4);
    const modes: string[] = [];
    setHeadWriterForTests(async (_i, _c, _r, mode) => {
      modes.push(mode);
      return true;
    });
    const g = new wasm.PublisherGroup(first, owner.ed25519Seed, channelId);
    try {
      const ownerHex = Array.from(g.ownerAccountKey(), (x) => x.toString(16).padStart(2, "0")).join("");
      const before = publisherLeaves(g);
      const entry = wasm.PublisherGroup.enter(
        stranger,
        g.groupInfo(first),
        owner.ed25519Seed,
        channelId,
        g.dsAccessSeed(first),
      );
      const commit = entry.commit;
      const entered = entry.takeGroup();
      entry.free();
      expect(entered.mergeStagedCommit(stranger)).toBe(true);
      entered.free();
      g.processIncomingVerifyingRoles(first, commit).free();
      const added = newOwnLeaves(ownerHex, before, publisherLeaves(g));
      expect(added).toEqual([Array.from(stranger.signaturePublicKey, (x) => x.toString(16).padStart(2, "0")).join("")]);
      const calls: string[] = [];
      const ran = await afterPeerCommitsApplied(
        { applied: 1, ownLeavesAdded: added },
        {
          writeHead: async () => {
            calls.push("head");
            return writeCurrentHead(g, first, owner);
          },
          reconcile: async () => {
            calls.push("reconcile");
          },
        },
      );
      expect(ran).toBe(true);
      expect(calls).toEqual(["head", "reconcile"]);
      expect(modes).toEqual(["merge"]);
    } finally {
      g.free();
    }
  });

  it("an applied commit that adds no leaf of this account does not reconcile; a failing reconcile never throws", async () => {
    const calls: string[] = [];
    const ports = {
      writeHead: async () => calls.push("head"),
      reconcile: async () => {
        calls.push("reconcile");
        throw new Error("x");
      },
    };
    expect(await afterPeerCommitsApplied({ applied: 1, ownLeavesAdded: [] }, ports)).toBe(false);
    expect(await afterPeerCommitsApplied({ applied: 0, ownLeavesAdded: ["aa"] }, ports)).toBe(false);
    expect(calls).toEqual(["head"]);
    await expect(afterPeerCommitsApplied({ applied: 2, ownLeavesAdded: ["aa"] }, ports)).resolves.toBe(true);
  });

  it("a re-founded group's head write asks for a replace", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0xe1));
    const device = deviceOf(owner, 0xe2);
    const modes: string[] = [];
    setHeadWriterForTests(async (_i, _c, r, mode) => {
      modes.push(`${mode}:${r.epoch}`);
      return true;
    });
    const g = new wasm.PublisherGroup(device, owner.ed25519Seed, new Uint8Array(16).fill(0xe3));
    try {
      expect(await writeCurrentHead(g, device, owner, "replace")).toBe(true);
      expect(modes).toEqual([`replace:${g.epoch()}`]);
    } finally {
      g.free();
    }
  });

  it("a co-publisher's device never writes the head", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x81));
    const cop = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x82));
    const ownerDevice = deviceOf(owner, 0x83);
    const copDevice = deviceOf(cop, 0x84);
    const channelId = new Uint8Array(16).fill(0x85);
    let written = 0;
    setHeadWriterForTests(async () => {
      written += 1;
      return true;
    });
    const g = new wasm.PublisherGroup(ownerDevice, owner.ed25519Seed, channelId);
    try {
      const req = g.retainJoinRequest(ownerDevice, copDevice.createKeyPackage());
      const added = g.stageApproveExternalJoin(ownerDevice, req.joinerSignatureKey);
      req.free();
      expect(g.mergeStagedCommit(ownerDevice)).toBe(true);
      const copGroup = wasm.PublisherGroup.joinFromWelcome(copDevice, added.welcome, g.groupId(), channelId, owner.signaturePublicKey);
      added.free();
      try {
        expect(await writeCurrentHead(copGroup, copDevice, cop)).toBe(false);
        expect(written).toBe(0);
        expect(await writeCurrentHead(g, ownerDevice, owner)).toBe(true);
        expect(written).toBe(1);
      } finally {
        copGroup.free();
      }
    } finally {
      g.free();
    }
  });

  it("the head round-trips through the real vault keys against an in-memory service", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x91));
    const channelId = new Uint8Array(16).fill(0x92);
    const heads = new Map<string, { version: bigint; sealed: Uint8Array }>();
    const claims = new Map<string, string>();
    const seen: { method: string; url: string; proof: string | null }[] = [];
    const send = async (url: string, init: RequestInit): Promise<Response> => {
      const headers = new Headers(init.headers);
      seen.push({ method: init.method ?? "GET", url, proof: headers.get(STORAGE_PROOF_HEADER) });
      const m = /\/vault\/([0-9a-f]{64})(\/head)?$/.exec(url);
      if (m === null) return new Response(null, { status: 404 });
      const id = m[1] ?? "";
      if (m[2] === undefined) {
        const body = JSON.parse(new TextDecoder().decode(init.body as Uint8Array)) as { write_pk: string };
        claims.set(id, body.write_pk);
        return new Response(null, { status: 201 });
      }
      if (!claims.has(id)) return Response.json({ error: "vault-proof-refused" }, { status: 401 });
      if (init.method === "GET") {
        const h = heads.get(id);
        if (h === undefined) return Response.json({ error: "vault-object-not-found" }, { status: 404 });
        const out = new Uint8Array(8 + h.sealed.length);
        new DataView(out.buffer).setBigUint64(0, h.version, false);
        out.set(h.sealed, 8);
        return new Response(out, { status: 200 });
      }
      const put = decodeVersioned(init.body as Uint8Array);
      if (put === null) return new Response(null, { status: 400 });
      if (put.version !== (heads.get(id)?.version ?? 0n) + 1n) {
        return Response.json({ error: "vault-version-conflict" }, { status: 409 });
      }
      heads.set(id, put);
      return new Response(null, { status: 201 });
    };
    const tokens = { spend: async (_c: string, f: (n: string, v: string) => Promise<Response>) => f("x-lakko-anon-token", "t") };
    const client = new VaultClient("/api/storage", send, tokens as unknown as ConstructorParameters<typeof VaultClient>[2]);
    const store = wasmChannelHeadStore(wasm, owner, channelId, client);
    try {
      expect(await writeChannelHead(store, ep(3))).toBe(true);
      expect(await writeChannelHead(store, ep(4))).toBe(true);
      expect(await writeChannelHead(store, ep(5))).toBe(true);
      const saved = await readSavedEpochs(store);
      expect(epochs(saved)).toEqual([5, 4]);
      expect(Array.from(saved[0]?.groupInfo ?? [])).toEqual([5, 5, 5]);
      expect(Array.from(saved[0]?.dsAccessSeed ?? [])).toEqual(Array.from(new Uint8Array(32).fill(5)));
      // One claim, every vault request signed, the service path under the
      // storage prefix.
      expect(claims.size).toBe(1);
      expect(seen.filter((s) => s.method === "PUT" && s.url.endsWith("/head")).every((s) => s.proof !== null)).toBe(true);
      expect(seen.every((s) => s.url.startsWith("/api/storage/vault/"))).toBe(true);
      // Another account's keys cannot open it.
      const other = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x93));
      const otherStore = wasmChannelHeadStore(wasm, other, channelId, client);
      expect(await readSavedEpochs(otherStore)).toEqual([]);
      otherStore.release();
    } finally {
      store.release();
    }
  });
});

describe("an evicted owner group no longer blocks the entry (walk of 2026-11-14)", () => {
  it("through the real core: the inactive group is deleted right before the entry, an active one never", () => {
    vi.spyOn(console, "info").mockImplementation(() => undefined);
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0xb1));
    const a = deviceOf(owner, 0xb2);
    const b = deviceOf(owner, 0xb3);
    const channelId = new Uint8Array(16).fill(0xb4);
    const ga = new wasm.PublisherGroup(a, owner.ed25519Seed, channelId);
    const groupId = ga.groupId();
    const c1 = ga.stageCommit(a, [b.createKeyPackage()], new Uint8Array(), false);
    const w1 = c1.welcome ?? new Uint8Array();
    c1.free();
    ga.mergeStagedCommit(a);
    const gb = wasm.PublisherGroup.joinFromWelcome(b, w1, groupId, channelId, wasm.publisherOwnerAccountKey(owner.ed25519Seed));
    try {
      // An ACTIVE held group on B: never deleted, no entry.
      expect(clearInactiveHeldGroup(wasm, b, owner.ed25519Seed, channelId)).toBeNull();
      expect(wasm.PublisherGroup.load(b, owner.ed25519Seed, channelId)?.isActive()).toBe(true);
      // B retires A; A applies it and is evicted.
      const retire = gb.stageRetireDevices(b, a.signaturePublicKey);
      gb.mergeStagedCommit(b);
      ga.processIncomingVerifyingRoles(a, retire).free();
      expect(ga.isActive()).toBe(false);
      ga.free();
      const savedInfo = gb.groupInfo(b);
      const savedSeed = gb.dsAccessSeed(b);
      const enter = (): wasm.PublisherGroupEntry =>
        wasm.PublisherGroup.enter(a, savedInfo, owner.ed25519Seed, channelId, savedSeed);
      // While the evicted group is stored the core refuses the entry.
      expect(enter).toThrow();
      const cleared = clearInactiveHeldGroup(wasm, a, owner.ed25519Seed, channelId);
      expect(cleared).not.toBeNull();
      expect(wasm.PublisherGroup.load(a, owner.ed25519Seed, channelId)).toBeUndefined();
      const entry = enter();
      const entered = entry.takeGroup();
      const commit = entry.commit;
      entry.free();
      try {
        expect(entered.mergeStagedCommit(a)).toBe(true);
        gb.processIncomingVerifyingRoles(b, commit).free();
        expect(Array.from(entered.channelSeed(a))).toEqual(Array.from(gb.channelSeed(b)));
      } finally {
        entered.free();
      }
    } finally {
      gb.free();
    }
  });

  it("nothing stored: a no-op handle; a failed build puts the evicted group back", () => {
    vi.spyOn(console, "info").mockImplementation(() => undefined);
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0xc1));
    const a = deviceOf(owner, 0xc2);
    const b = deviceOf(owner, 0xc3);
    const channelId = new Uint8Array(16).fill(0xc4);
    expect(clearInactiveHeldGroup(wasm, a, owner.ed25519Seed, new Uint8Array(16).fill(0xc5))).not.toBeNull();
    const ga = new wasm.PublisherGroup(a, owner.ed25519Seed, channelId);
    const c1 = ga.stageCommit(a, [b.createKeyPackage()], new Uint8Array(), false);
    const w1 = c1.welcome ?? new Uint8Array();
    c1.free();
    ga.mergeStagedCommit(a);
    const gb = wasm.PublisherGroup.joinFromWelcome(b, w1, ga.groupId(), channelId, wasm.publisherOwnerAccountKey(owner.ed25519Seed));
    try {
      const retire = gb.stageRetireDevices(b, a.signaturePublicKey);
      gb.mergeStagedCommit(b);
      ga.processIncomingVerifyingRoles(a, retire).free();
      const epochBefore = ga.epoch();
      ga.free();
      const cleared = clearInactiveHeldGroup(wasm, a, owner.ed25519Seed, channelId);
      expect(cleared).not.toBeNull();
      cleared?.restore();
      const back = wasm.PublisherGroup.load(a, owner.ed25519Seed, channelId);
      expect(back?.isActive()).toBe(false);
      expect(back?.epoch()).toBe(epochBefore);
      back?.free();
    } finally {
      gb.free();
    }
  });
});
