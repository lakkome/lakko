// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0050 section 2.5a item 7 — a sweep's Welcome is never written unsigned
 * and never silently lost:
 *
 *   - every grant held for the addressee is tried in order, the next on `401`;
 *   - with none left (or none at all) nothing is written and the write throws,
 *     which keeps the Welcome owed in the sweep's outbox;
 *   - an owed Welcome is retried when a new grant arrives.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));

import {
  MailboxUnreachableError,
  writeThroughCandidates,
  writeWelcome,
  type ResolvedWriteAuth,
  type WriteAuthKind,
  type WriteCandidate,
} from "./mailbox-write";
import {
  EMPTY_RECEIVED_GRANTS,
  bestGrantFor,
  fileContactGrant,
  fileScopedGrant,
  grantsInOrder,
} from "./received-grants";
import { ReachSession, onReceivedGrantsChanged } from "./reach-service";
import { groupsOwingWelcomes } from "../chat/owed-welcomes";
import type { DsClient } from "../ds/client";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function candidate(name: string, released: string[]): WriteCandidate {
  return {
    kind: "kept",
    resolve: async (): Promise<ResolvedWriteAuth> => ({
      kind: "kept" as WriteAuthKind,
      auth: { signer: { name } as never, delegation: null },
      release: () => released.push(name),
    }),
  };
}

const refused = (): Error => Object.assign(new Error("HTTP 401"), { status: 401 });

describe("trying every grant", () => {
  it("a 401 moves to the next grant, and the first that works wins", async () => {
    const released: string[] = [];
    const tried: string[] = [];
    const out = await writeThroughCandidates(
      [candidate("contact", released), candidate("group", released), candidate("member", released)],
      async (auth) => {
        const name = (auth.signer as unknown as { name: string }).name;
        tried.push(name);
        if (name === "contact") throw refused();
        return 42;
      },
    );
    expect(out).toBe(42);
    expect(tried).toEqual(["contact", "group"]);
    // Every signer derived is released, the refused one included.
    expect(released).toEqual(["contact", "group"]);
  });

  it("every grant refused: throws MailboxUnreachableError, nothing more is written", async () => {
    const write = vi.fn(async () => {
      throw refused();
    });
    await expect(
      writeThroughCandidates([candidate("a", []), candidate("b", [])], write),
    ).rejects.toBeInstanceOf(MailboxUnreachableError);
    expect(write).toHaveBeenCalledTimes(2);
    // Never a third, unsigned attempt.
    for (const call of write.mock.calls as unknown as Array<[{ signer: unknown }]>) {
      expect(call[0].signer).not.toBeNull();
    }
  });

  it("a failure other than 401 is thrown as is; another key would fare no better", async () => {
    const offline = Object.assign(new Error("offline"), { status: 503 });
    const write = vi.fn(async () => {
      throw offline;
    });
    await expect(writeThroughCandidates([candidate("a", []), candidate("b", [])], write)).rejects.toBe(
      offline,
    );
    expect(write).toHaveBeenCalledTimes(1);
  });

  it("no grant at all: nothing is written (never unsigned) and the write is kept", async () => {
    // No reach session, no explicit grant: the sweep's Welcome publish.
    const publish = vi.fn(async () => 1);
    await expect(
      writeWelcome(new Uint8Array(32).fill(9), { scope: new Uint8Array(32).fill(1) }, publish),
    ).rejects.toBeInstanceOf(MailboxUnreachableError);
    expect(publish).not.toHaveBeenCalled();
  });
});

describe("the order grants are tried in", () => {
  it("contact, the scopes' group keys (this scope first), their channel keys, then this group's member key", () => {
    const g = (n: number) => ({ wireB64: `w${n}`, version: n, receivedAtMs: 0 });
    let store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, "p", "s2", "group", g(2));
    store = fileScopedGrant(store, "p", "s1", "member", g(5));
    store = fileScopedGrant(store, "p", "s1", "group", g(1));
    store = fileScopedGrant(store, "p", "s2", "channel", g(3));
    store = fileContactGrant(store, "p", g(0));
    const order = grantsInOrder(store, "p", { scopeHex: "s1", allowMember: true }).map((h) => h.wireB64);
    expect(order).toEqual(["w0", "w1", "w2", "w3", "w5"]);
    // The single-grant rule is the first of the same order.
    expect(bestGrantFor(store, "p", { scopeHex: "s1", allowMember: true })?.wireB64).toBe("w0");
    // No member key unless the write is a Welcome into that scope.
    expect(grantsInOrder(store, "p", { scopeHex: "s1" }).map((h) => h.wireB64)).toEqual([
      "w0",
      "w1",
      "w2",
      "w3",
    ]);
  });
});

describe("the own keys a committer delegation may name (ADR-0066 section 3.5a)", () => {
  it("the current key first, then every own predecessor, newest first", async () => {
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x51));
    const oldest = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x52));
    const between = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x53));
    const session = await ReachSession.open("acc", account.ed25519Seed, account.signaturePublicKey, {
      ds: {} as unknown as DsClient,
    });
    try {
      expect(session.delegateKeys().map((k) => Array.from(k.accountKey))).toEqual([
        Array.from(account.signaturePublicKey),
      ]);
      // Oldest first, as the chain is held.
      session.setPredecessors([
        { accountKey: oldest.signaturePublicKey, seed: oldest.ed25519Seed },
        { accountKey: between.signaturePublicKey, seed: between.ed25519Seed },
      ]);
      const keys = session.delegateKeys();
      expect(keys.map((k) => Array.from(k.accountKey))).toEqual([
        Array.from(account.signaturePublicKey),
        Array.from(between.signaturePublicKey),
        Array.from(oldest.signaturePublicKey),
      ]);
      // Each one signs as exactly the key it names.
      for (const k of keys) {
        const signer = await k.signer();
        expect(signer).toBeDefined();
        signer.free();
      }
      session.setPredecessors([]);
      expect(session.delegateKeys()).toHaveLength(1);
    } finally {
      session.stop();
      account.free();
      oldest.free();
      between.free();
    }
  });
});

describe("an owed Welcome is retried when a grant arrives", () => {
  it("a change to the received grants notifies; an unchanged store does not", async () => {
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x44));
    const session = await ReachSession.open("acc", account.ed25519Seed, account.signaturePublicKey, {
      ds: {} as unknown as DsClient,
    });
    const listener = vi.fn();
    const off = onReceivedGrantsChanged(listener);
    try {
      await session.updateReceivedGrants((g) => g);
      expect(listener).not.toHaveBeenCalled();
      await session.updateReceivedGrants((g) =>
        fileContactGrant(g, "peer", { wireB64: "w", version: 0, receivedAtMs: 1 }),
      );
      expect(listener).toHaveBeenCalledTimes(1);
    } finally {
      off();
      session.stop();
      account.free();
    }
  });

  it("only the groups whose outbox owes a Welcome are swept again", async () => {
    const outbox: Record<string, Array<{ sweepWelcomesOwed?: boolean }>> = {
      a: [{ sweepWelcomesOwed: true }],
      b: [{}],
      c: [],
    };
    const owing = await groupsOwingWelcomes(["a", "b", "c", "d"], async (g) => {
      if (g === "d") throw new Error("unreadable");
      return outbox[g] ?? [];
    });
    expect(owing).toEqual(["a"]);
  });
});

// ---- ADR-0065 R.4: the continuity key as a Welcome candidate ----------------

import { planWriteToPerson } from "./mailbox-write";

describe("the continuity key (ADR-0065 R.4)", () => {
  let TARGET: Uint8Array;
  beforeAll(() => {
    const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(0x42));
    TARGET = id.signaturePublicKey;
    id.free();
  });
  const CONTINUITY = { accountId: new Uint8Array(16).fill(7), keyGeneration: 2 };

  it("is a candidate for a Welcome, signed by the key the owner registered", async () => {
    const plan = await planWriteToPerson(TARGET, {
      allowMember: true,
      neverUnsigned: true,
      continuity: CONTINUITY,
    });
    expect(plan.candidates.map((c) => c.kind)).toEqual(["continuity"]);
    const resolved = await plan.candidates[0]!.resolve();
    try {
      const registered = wasm.reachVerifyingKey(wasm.continuityReachSeed(CONTINUITY.accountId, 2n));
      expect(resolved.auth.delegation).toBeNull();
      expect(Array.from((resolved.auth.signer as { verifyingKey(): Uint8Array }).verifyingKey())).toEqual(
        Array.from(registered),
      );
    } finally {
      resolved.release();
    }
  });

  it("is never offered when the caller rules it out, and never for a non-Welcome write", async () => {
    const ruledOut = await planWriteToPerson(TARGET, {
      allowMember: true,
      neverUnsigned: true,
      continuity: null,
    });
    expect(ruledOut.candidates).toEqual([]);
    // No pin names TARGET as a reset's new key, so nothing is resolved either.
    const unresolved = await planWriteToPerson(TARGET, { allowMember: true, neverUnsigned: true });
    expect(unresolved.candidates).toEqual([]);
  });
});
