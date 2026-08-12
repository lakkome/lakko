// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0042 — `push/group-routing` orchestration.
 *
 * Pins the load-bearing rules with the REAL wasm derivation (so the
 * golden-equality claim is proven, not asserted against a mock):
 *
 *   - the OWNER mints the secret exactly once, persists it, broadcasts the
 *     distribution envelope, and subscribes under the derived opaque
 *     `push_gid` — a second provision is a no-op (no re-mint / re-broadcast);
 *   - a NON-owner that receives the envelope decodes the SAME secret and
 *     derives the SAME `push_gid` (golden-equal to the owner's) — this is
 *     what makes the publisher/subscriber notify JOIN match;
 *   - subscribe routes under `push_gid`, never the DS `group_id`;
 *   - a member WITHOUT the secret DEFERS (no subscription under group_id);
 *   - the reverse index (`push_gid -> account/group`) is populated from the
 *     derived `push_gid` — the map the Sprint-4 decrypt reverse-resolves.
 *
 * The store / push-gw / device-id / device-key are mocked; the wasm core is
 * REAL (loaded through the Node `__wbindgen_init` glue), so `derivePushGid`
 * / `composePushRoutingSecret` / `decodePushRoutingSecret` are the true
 * deterministic functions.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import type {
  Group as WasmGroup,
  Identity as WasmIdentity,
} from "../../wasm/lakko_ffi_wasm";

// ---- mocks ---------------------------------------------------------------

// In-memory backing shared by the store mock (hoisted so the vi.mock factory
// can close over it). The vi.fn's read/write these maps.
// `distributed` mirrors the real store's row field: a key ABSENT from the
// map models a legacy row written before the flag existed, which the store
// reads as `distributed` (never `pending`).
const h = vi.hoisted(() => ({
  secrets: new Map<string, Uint8Array>(),
  distributed: new Map<string, boolean>(),
  routes: new Map<string, { accountId: string; groupIdB64: string }>(),
}));

vi.mock("./group-routing-store", () => ({
  putGroupRouting: vi.fn(
    async (
      args: {
        accountId: string;
        groupIdB64: string;
        secret: Uint8Array;
        pushGidHex: string;
        distributed: boolean;
      },
      _deviceKey: unknown,
    ) => {
      h.secrets.set(`${args.accountId}\0${args.groupIdB64}`, args.secret);
      h.distributed.set(
        `${args.accountId}\0${args.groupIdB64}`,
        args.distributed,
      );
      h.routes.set(args.pushGidHex, {
        accountId: args.accountId,
        groupIdB64: args.groupIdB64,
      });
    },
  ),
  getGroupRoutingSecret: vi.fn(
    async (accountId: string, groupIdB64: string, _dk: unknown) =>
      h.secrets.get(`${accountId}\0${groupIdB64}`) ?? null,
  ),
  getGroupRoutingState: vi.fn(
    async (accountId: string, groupIdB64: string) => {
      const k = `${accountId}\0${groupIdB64}`;
      if (!h.secrets.has(k)) return "absent";
      // Legacy row (no flag recorded) reads as distributed, exactly like the
      // real store's `row.distributed === false ? "pending" : "distributed"`.
      return h.distributed.get(k) === false ? "pending" : "distributed";
    },
  ),
  markGroupRoutingDistributed: vi.fn(
    async (accountId: string, groupIdB64: string) => {
      const k = `${accountId}\0${groupIdB64}`;
      if (h.secrets.has(k)) h.distributed.set(k, true);
    },
  ),
  // Both marks are no-ops on an absent row — they must never resurrect a
  // secret a concurrent `forgetDevice` wiped.
  markGroupRoutingPending: vi.fn(
    async (accountId: string, groupIdB64: string) => {
      const k = `${accountId}\0${groupIdB64}`;
      if (h.secrets.has(k)) h.distributed.set(k, false);
    },
  ),
  // ADR-0043 — the per-gid reverse-index delete. A no-op on an absent row,
  // exactly like the real IndexedDB `delete`.
  deleteRoute: vi.fn(async (pushGidHex: string) => {
    h.routes.delete(pushGidHex);
  }),
}));

// getDeviceKey returns a non-null placeholder — the store is mocked, so the
// key is never used for real crypto here.
vi.mock("../auth/key-store", () => ({
  getDeviceKey: vi.fn(async () => ({}) as unknown as CryptoKey),
}));

vi.mock("./device-id", () => ({
  getOrCreateDevicePushId: (): Uint8Array => new Uint8Array(32).fill(7),
}));

const subscribeGroup = vi.fn(
  (_device: Uint8Array, _group: Uint8Array): Promise<{ inserted: boolean }> =>
    Promise.resolve({ inserted: true }),
);
/** ADR-0043 — the first real caller of `unsubscribeGroup` (dead code until
 *  now). Records the group pseudonym so a test can prove the RETIRED gid is
 *  the one torn down. */
const unsubscribeGroup = vi.fn(
  (_device: Uint8Array, _group: Uint8Array): Promise<{ removed: boolean }> =>
    Promise.resolve({ removed: true }),
);
vi.mock("./client", () => ({
  DEFAULT_BASE_URL: "/api/push",
  PushClient: class {
    subscribeGroup = subscribeGroup;
    unsubscribeGroup = unsubscribeGroup;
  },
}));

// Return the REAL wasm namespace from `loadWasm` (initialised in beforeAll).
vi.mock("../wasm", async () => {
  const real = await import("../../wasm/lakko_ffi_wasm");
  return { loadWasm: async () => real };
});

import {
  composeSecretEnvelope,
  ensureGroupPushRouting,
  membershipRotationDecision,
  memberSignatureKeys,
  mintPushRoutingSecret,
  readPushRotationRole,
  receivePushRoutingSecret,
  receivePushRoutingSecretGated,
  redistributeSecretToGroup,
  rotatePushRoutingSecret,
  rotatePushRoutingSecretIfMembershipShrank,
  routingSecretVerdict,
  subscribeGroupUnderPushGid,
  tryDecodeSecretEnvelope,
  verdictAccepts,
} from "./group-routing";
import {
  deleteRoute,
  getGroupRoutingSecret,
  putGroupRouting,
} from "./group-routing-store";
import { base64Encode } from "../auth/account-id";

// ---- helpers -------------------------------------------------------------

function toHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** A mock group with a fixed group_id + a role the test controls. */
function mockGroup(groupId: Uint8Array, role: number | undefined): WasmGroup {
  return {
    get groupId(): Uint8Array {
      return groupId;
    },
    ownRole: (_id: WasmIdentity): number | undefined => role,
  } as unknown as WasmGroup;
}

const DUMMY_IDENTITY = {} as unknown as WasmIdentity;

/** Signature public keys for the receive-gate matrix. */
const OWNER_PK = new Uint8Array(32).fill(0xa1);
const MEMBER_PK = new Uint8Array(32).fill(0xb2);
const ADMIN_PK = new Uint8Array(32).fill(0xc3);

/** Leaf indices the mock member snapshot exposes. */
const OWNER_LEAF = 0;
const MEMBER_LEAF = 1;
const ADMIN_LEAF = 2;

/**
 * A mock group carrying an ADR-0022 role table + a `(leafIndex ->
 * signaturePublicKey)` member snapshot, i.e. exactly the three reads the
 * receive-side role gate performs (`roleTableOwner` / `roleTableAdmins` /
 * `members`).
 *
 * @param ownerKey `undefined` models a legacy / open group with NO role
 *                 table (`roleTableOwner()` returns undefined and
 *                 `roleTableAdmins()` returns an empty buffer).
 * @param opts.admins the admin set, flattened into the 32-byte-stride buffer
 *                 the real `roleTableAdmins()` returns. Defaults to
 *                 `[ADMIN_PK]`.
 * @param opts.throws makes ALL reads throw, modelling a torn group.
 */
function mockRoleGroup(
  groupId: Uint8Array,
  ownerKey: Uint8Array | undefined,
  opts: { throws?: boolean; admins?: Uint8Array[] } = {},
): WasmGroup {
  const admins = opts.admins ?? [ADMIN_PK];
  return {
    get groupId(): Uint8Array {
      return groupId;
    },
    ownRole: (_id: WasmIdentity): number | undefined => undefined,
    roleTableOwner: (): Uint8Array | undefined => {
      if (opts.throws === true) throw new Error("torn group");
      return ownerKey;
    },
    roleTableAdmins: (): Uint8Array => {
      if (opts.throws === true) throw new Error("torn group");
      // An open group (no role table) has no admin set either.
      if (ownerKey === undefined) return new Uint8Array(0);
      const flat = new Uint8Array(admins.length * 32);
      admins.forEach((a, i) => flat.set(a, i * 32));
      return flat;
    },
    members: (): unknown[] => {
      if (opts.throws === true) throw new Error("torn group");
      return [
        { leafIndex: OWNER_LEAF, signaturePublicKey: OWNER_PK },
        { leafIndex: MEMBER_LEAF, signaturePublicKey: MEMBER_PK },
        { leafIndex: ADMIN_LEAF, signaturePublicKey: ADMIN_PK },
      ];
    },
  } as unknown as WasmGroup;
}

/**
 * Yield the microtask queue enough times for an in-flight rotation to run to
 * its next real suspension point (the mocked store / client calls all settle
 * as immediately-resolved promises, so no timers are involved).
 */
async function flushMicrotasks(times = 50): Promise<void> {
  for (let i = 0; i < times; i += 1) await Promise.resolve();
}

/** Seed this device's already-held secret for `(account, group)`. */
function seedStoredSecret(
  accountId: string,
  groupIdB64: string,
  secret: Uint8Array,
): void {
  h.secrets.set(`${accountId}\0${groupIdB64}`, secret);
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(
    new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url),
  );
  await initWasm({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  h.secrets.clear();
  h.distributed.clear();
  h.routes.clear();
  subscribeGroup.mockClear();
  unsubscribeGroup.mockClear();
  vi.mocked(putGroupRouting).mockClear();
  vi.mocked(deleteRoute).mockClear();
});

describe("group-routing orchestration (ADR-0042)", () => {
  it("owner mints once, persists, broadcasts the envelope, subscribes under push_gid", async () => {
    const groupId = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
    const group = mockGroup(groupId, wasm.Role.Owner);
    const broadcasts: Uint8Array[] = [];

    await ensureGroupPushRouting(
      "acctOwner",
      group,
      DUMMY_IDENTITY,
      async (b) => {
        broadcasts.push(b);
      },
    );

    // Persisted exactly once, with a 32-byte secret.
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    const putArg = vi.mocked(putGroupRouting).mock.calls[0]![0];
    expect(putArg.secret.length).toBe(32);
    expect(putArg.accountId).toBe("acctOwner");

    // Broadcast exactly once; the envelope decodes back to the persisted secret.
    expect(broadcasts.length).toBe(1);
    const decoded = wasm.decodePushRoutingSecret(broadcasts[0]!);
    expect(Array.from(decoded)).toEqual(Array.from(putArg.secret));

    // push_gid = keyed-BLAKE3(secret) — the persisted hex matches the derivation.
    const pushGid = wasm.derivePushGid(putArg.secret);
    expect(putArg.pushGidHex).toBe(toHex(pushGid));

    // Reverse index maps push_gid -> (account, group_id).
    expect(h.routes.get(putArg.pushGidHex)).toEqual({
      accountId: "acctOwner",
      groupIdB64: base64OfGroupId(groupId),
    });

    // Subscribed under push_gid — NOT the DS group_id.
    await vi.waitFor(() => expect(subscribeGroup).toHaveBeenCalled());
    const subTarget = subscribeGroup.mock.calls[0]![1];
    expect(Array.from(subTarget)).toEqual(Array.from(pushGid));
    expect(Array.from(subTarget)).not.toEqual(Array.from(groupId));
  });

  it("a second provision of the same owned group is a no-op (no re-mint / re-broadcast)", async () => {
    const group = mockGroup(new Uint8Array([9, 9, 9, 9]), wasm.Role.Owner);
    const broadcasts: Uint8Array[] = [];
    const broadcast = async (b: Uint8Array): Promise<void> => {
      broadcasts.push(b);
    };

    await ensureGroupPushRouting("acctO", group, DUMMY_IDENTITY, broadcast);
    await ensureGroupPushRouting("acctO", group, DUMMY_IDENTITY, broadcast);

    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    expect(broadcasts.length).toBe(1);
  });

  it("a non-member (undefined role) never mints", async () => {
    const group = mockGroup(new Uint8Array([4, 4, 4, 4]), undefined);
    const broadcasts: Uint8Array[] = [];

    await ensureGroupPushRouting("acctX", group, DUMMY_IDENTITY, async (b) => {
      broadcasts.push(b);
    });

    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(broadcasts.length).toBe(0);
  });

  it("a non-owner receiving the envelope derives the SAME push_gid (golden-equal)", async () => {
    // Owner mints a secret + composes the distribution envelope.
    const secret = mintPushRoutingSecret();
    expect(secret.length).toBe(32);
    const envelope = await composeSecretEnvelope(secret);
    const ownerPushGid = wasm.derivePushGid(secret);

    // Non-owner decodes the envelope back to the exact secret ...
    const decoded = await tryDecodeSecretEnvelope(envelope);
    expect(decoded).not.toBeNull();
    expect(Array.from(decoded as Uint8Array)).toEqual(Array.from(secret));

    // ... persists it under its own account + subscribes.
    await receivePushRoutingSecret("acctPeer", "grpB64Peer", decoded as Uint8Array);

    const calls = vi.mocked(putGroupRouting).mock.calls;
    const putArg = calls[calls.length - 1]![0];
    expect(putArg.accountId).toBe("acctPeer");
    // The golden-equality: the non-owner's push_gid == the owner's push_gid.
    expect(putArg.pushGidHex).toBe(toHex(ownerPushGid));

    await vi.waitFor(() => expect(subscribeGroup).toHaveBeenCalled());
    const subTarget = subscribeGroup.mock.calls[0]![1];
    expect(Array.from(subTarget)).toEqual(Array.from(ownerPushGid));
  });

  it("a member WITHOUT the secret defers subscription (never subscribes under group_id)", async () => {
    const res = await subscribeGroupUnderPushGid("acctNoSecret", "unknownGrp");
    expect(res).toBe("deferred");
    expect(subscribeGroup).not.toHaveBeenCalled();
  });

  it("tryDecodeSecretEnvelope fail-closed rejects a non-envelope plaintext", async () => {
    // A plain chat JSON envelope is not a push-routing envelope.
    const chat = new TextEncoder().encode('{"v":1,"t":"hello"}');
    expect(await tryDecodeSecretEnvelope(chat)).toBeNull();
    // Right length, wrong first byte.
    expect(await tryDecodeSecretEnvelope(new Uint8Array(62).fill(0x7b))).toBeNull();
  });
});

describe("distribution durability — a failed broadcast must not strand the group", () => {
  const ACCT = "acctDur";
  const GROUP_ID = new Uint8Array([3, 1, 4, 1, 5, 9]);
  const GRP_B64 = base64Encode(GROUP_ID);

  /** A broadcast sink that records every attempt and can be armed to fail. */
  function recordingBroadcast(): {
    sent: Uint8Array[];
    failNext: (n: number) => void;
    fn: (b: Uint8Array) => Promise<void>;
  } {
    const sent: Uint8Array[] = [];
    let failures = 0;
    return {
      sent,
      failNext: (n: number): void => {
        failures = n;
      },
      fn: async (b: Uint8Array): Promise<void> => {
        sent.push(b);
        if (failures > 0) {
          failures -= 1;
          throw new Error("DS unreachable");
        }
      },
    };
  }

  it("a failed broadcast leaves the secret PENDING and the NEXT provision re-broadcasts it", async () => {
    const group = mockGroup(GROUP_ID, wasm.Role.Owner);
    const b = recordingBroadcast();
    b.failNext(1);

    // Pass 1 — mint + persist succeed, the broadcast throws. The failure is
    // swallowed (push is best-effort) but must NOT be forgotten.
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);

    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    const minted = vi.mocked(putGroupRouting).mock.calls[0]![0].secret;
    // Persisted as NOT distributed — this is what makes it recoverable.
    expect(vi.mocked(putGroupRouting).mock.calls[0]![0].distributed).toBe(false);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(false);
    expect(b.sent.length).toBe(1);

    // Pass 2 (a later launch / chat-open) — re-broadcasts the SAME secret,
    // recomputed from the store. No second mint: a fresh random secret would
    // diverge from the one already persisted + subscribed under.
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);

    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    expect(b.sent.length).toBe(2);
    expect(Array.from(wasm.decodePushRoutingSecret(b.sent[1]!))).toEqual(
      Array.from(minted),
    );
    // Confirmed on the wire this time -> flagged distributed.
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);

    // Pass 3 — now that it is distributed, nothing more is sent.
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(2);
  });

  it("keeps retrying while the broadcast keeps failing (not a one-shot repair)", async () => {
    const group = mockGroup(GROUP_ID, wasm.Role.Owner);
    const b = recordingBroadcast();
    b.failNext(3);

    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);

    expect(b.sent.length).toBe(3);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(false);
    // Still exactly one secret ever minted.
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
  });

  it("a SUCCESSFUL mint-broadcast is not re-broadcast on the next provision", async () => {
    const group = mockGroup(GROUP_ID, wasm.Role.Owner);
    const b = recordingBroadcast();

    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);

    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(1);
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
  });

  it("a LEGACY row carrying no flag is treated as distributed (no upgrade-day re-broadcast)", async () => {
    // Seed the secret WITHOUT a `distributed` entry — exactly the on-disk
    // shape a build predating the flag left behind. Those groups are working
    // today, so an upgrade must not re-broadcast for every one of them.
    seedStoredSecret(ACCT, GRP_B64, new Uint8Array(32).fill(0x5a));
    expect(h.distributed.has(`${ACCT}\0${GRP_B64}`)).toBe(false);

    const group = mockGroup(GROUP_ID, wasm.Role.Owner);
    const b = recordingBroadcast();
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);

    expect(b.sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });

  it("a RECEIVED secret is stored distributed (a receiver never re-broadcasts)", async () => {
    const secret = mintPushRoutingSecret();
    await receivePushRoutingSecret(ACCT, GRP_B64, secret);

    expect(vi.mocked(putGroupRouting).mock.calls[0]![0].distributed).toBe(true);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);

    // Even if this device is (or becomes) the owner, the received secret is
    // already on the wire — no re-broadcast, no re-mint.
    const b = recordingBroadcast();
    await ensureGroupPushRouting(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Owner),
      DUMMY_IDENTITY,
      b.fn,
    );
    expect(b.sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
  });

  it("a NON-owner holding a pending-looking hole never broadcasts", async () => {
    // Only the owner MINTS; a member with no secret has nothing to re-send,
    // so it defers (it never falls back to subscribing under group_id).
    const b = recordingBroadcast();
    await ensureGroupPushRouting(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Member),
      DUMMY_IDENTITY,
      b.fn,
    );
    expect(b.sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(subscribeGroup).not.toHaveBeenCalled();
  });

  // -- a failed REDISTRIBUTE must re-arm the same retry -------------------
  //
  // The `distributed` flag is per-(account, group), not per-member. Once the
  // mint broadcast lands the row is `distributed`, so a LATER redistribute
  // (post-add-member) that fails would never be retried and the NEW member —
  // whose only source of the secret is that redistribute — would defer push
  // forever.

  it("a failed REDISTRIBUTE flips the row back to pending and the next provision re-broadcasts", async () => {
    const group = mockGroup(GROUP_ID, wasm.Role.Owner);
    const b = recordingBroadcast();

    // Mint + broadcast land cleanly: the row is settled.
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(1);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);
    const minted = vi.mocked(putGroupRouting).mock.calls[0]![0].secret;

    // A member is added; the redistribute that would carry the secret to them
    // fails. The caller only logs it — the durable record is what matters.
    b.failNext(1);
    await expect(
      redistributeSecretToGroup(ACCT, group, b.fn),
    ).rejects.toThrow("DS unreachable");
    expect(b.sent.length).toBe(2);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(false);

    // Next launch / chat-open: the existing re-broadcast branch repairs it,
    // carrying the byte-identical secret (never a re-mint — a fresh random
    // secret would diverge from the one members already hold).
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(3);
    expect(Array.from(wasm.decodePushRoutingSecret(b.sent[2]!))).toEqual(
      Array.from(minted),
    );
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);

    // Settled again — no further traffic.
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(3);
  });

  it("a SUCCESSFUL redistribute leaves the row distributed and the next provision sends nothing", async () => {
    const group = mockGroup(GROUP_ID, wasm.Role.Owner);
    const b = recordingBroadcast();

    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(1);

    await redistributeSecretToGroup(ACCT, group, b.fn);
    expect(b.sent.length).toBe(2);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);

    // No spurious re-broadcast: the redistribute confirmed the wire itself.
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(2);
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
  });

  // -- the retry is reachable from ANY holder, not just the owner ----------
  //
  // MINT is owner-only (32 random bytes; a second minter diverges the group's
  // push_gid). RE-BROADCAST needs no authority at all: it re-sends the exact
  // bytes this device already holds, which `routingSecretVerdict` resolves as
  // `accept-identical` for ANY sender BEFORE the role table is consulted.
  // Gating the retry on ownership strands the admin-add case below.

  it("a NON-owner (ADMIN) whose redistribute failed DOES re-broadcast on the next provision", async () => {
    // ADR-0022 lets an ADMIN commit an add, so an admin reaches redistribute —
    // and for a member added by that admin, the admin's redistribute is the
    // new member's ONLY source of the secret. If only the owner could retry:
    // the admin's row goes pending but the admin never re-sends, the owner's
    // row still reads `distributed` so the owner never learns, and the newly
    // added member gets no push, permanently.
    const held = new Uint8Array(32).fill(0x33);
    seedStoredSecret(ACCT, GRP_B64, held);
    h.distributed.set(`${ACCT}\0${GRP_B64}`, true);
    const admin = mockGroup(GROUP_ID, wasm.Role.Admin);
    const b = recordingBroadcast();

    b.failNext(1);
    await expect(redistributeSecretToGroup(ACCT, admin, b.fn)).rejects.toThrow(
      "DS unreachable",
    );
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(false);

    // Next launch / chat-open: the admin repairs ITS OWN failed send.
    await ensureGroupPushRouting(ACCT, admin, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(2);
    expect(Array.from(wasm.decodePushRoutingSecret(b.sent[1]!))).toEqual(
      Array.from(held),
    );
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);
    // Never a mint: the retry re-sends the HELD bytes and generates nothing,
    // so a non-owner retry cannot diverge the group's push_gid.
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();

    // Settled again — no further traffic.
    await ensureGroupPushRouting(ACCT, admin, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(2);
  });

  it("a PLAIN MEMBER holding a pending row re-broadcasts too (identical bytes need no authority)", async () => {
    const held = new Uint8Array(32).fill(0x44);
    seedStoredSecret(ACCT, GRP_B64, held);
    h.distributed.set(`${ACCT}\0${GRP_B64}`, false);
    const member = mockGroup(GROUP_ID, wasm.Role.Member);
    const b = recordingBroadcast();

    await ensureGroupPushRouting(ACCT, member, DUMMY_IDENTITY, b.fn);

    expect(b.sent.length).toBe(1);
    expect(Array.from(wasm.decodePushRoutingSecret(b.sent[0]!))).toEqual(
      Array.from(held),
    );
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });

  it("a NON-owner with NO secret still never mints and never broadcasts (mint gate untouched)", async () => {
    // The half that stays owner-only. Neither an admin nor a plain member may
    // create a secret — that is what would diverge the group's push_gid.
    const b = recordingBroadcast();

    await ensureGroupPushRouting(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Admin),
      DUMMY_IDENTITY,
      b.fn,
    );
    await ensureGroupPushRouting(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Member),
      DUMMY_IDENTITY,
      b.fn,
    );

    expect(b.sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(h.secrets.has(`${ACCT}\0${GRP_B64}`)).toBe(false);
    expect(subscribeGroup).not.toHaveBeenCalled();
  });

  it("a non-owner keeps retrying while the re-broadcast keeps failing", async () => {
    const held = new Uint8Array(32).fill(0x55);
    seedStoredSecret(ACCT, GRP_B64, held);
    h.distributed.set(`${ACCT}\0${GRP_B64}`, false);
    const member = mockGroup(GROUP_ID, wasm.Role.Member);
    const b = recordingBroadcast();
    b.failNext(2);

    await ensureGroupPushRouting(ACCT, member, DUMMY_IDENTITY, b.fn);
    await ensureGroupPushRouting(ACCT, member, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(2);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(false);

    // Third attempt lands.
    await ensureGroupPushRouting(ACCT, member, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(3);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });
});

describe("receive-side role gate (ADR-0042, lakko-security MEDIUM-1)", () => {
  const ACCT = "acctGate";
  const GRP = "grpGateB64";
  const GROUP_ID = new Uint8Array([7, 7, 7, 7]);

  /** A fresh secret that differs from `mintPushRoutingSecret()` output. */
  const HELD = new Uint8Array(32).fill(0x11);
  const ROTATED = new Uint8Array(32).fill(0x22);

  it("ACCEPTS a changed secret authored by the OWNER (persists + subscribes)", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      OWNER_LEAF,
      ROTATED,
    );

    expect(verdict).toBe("accept-owner");
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    // The store now holds the ROTATED secret.
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(ROTATED),
    );
    await vi.waitFor(() => expect(subscribeGroup).toHaveBeenCalled());
  });

  it("REJECTS a changed secret from a PLAIN MEMBER (store unchanged, no subscribe)", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      MEMBER_LEAF,
      ROTATED,
    );

    expect(verdict).toBe("reject-not-authorized");
    // Nothing persisted, nothing subscribed — the unilateral rotation +
    // the orphan push-gw subscription it would strand are both prevented.
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(subscribeGroup).not.toHaveBeenCalled();
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(HELD),
    );
  });

  it("ACCEPTS an IDENTICAL secret from a plain member (idempotent redistribute relay)", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      MEMBER_LEAF,
      // byte-identical to what we already hold (a fresh array, so this is a
      // value comparison, not a reference one)
      new Uint8Array(HELD),
    );

    expect(verdict).toBe("accept-identical");
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(HELD),
    );
  });

  it("ACCEPTS a changed secret in a group with NO role table (legacy / open)", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    const group = mockRoleGroup(GROUP_ID, undefined);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      MEMBER_LEAF,
      ROTATED,
    );

    // No owner exists to authorise against — keep the pre-gate behaviour
    // rather than silently breaking those groups.
    expect(verdict).toBe("accept-no-owner");
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
  });

  it("ACCEPTS a first-ever secret from the owner when the device holds none", async () => {
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      OWNER_LEAF,
      ROTATED,
    );

    expect(verdict).toBe("accept-owner");
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
  });

  it("REJECTS a first-ever secret from a PLAIN MEMBER (no stored secret to match)", async () => {
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      MEMBER_LEAF,
      ROTATED,
    );

    expect(verdict).toBe("reject-not-authorized");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(subscribeGroup).not.toHaveBeenCalled();
  });

  // ---- admin tier (the delivery regression the owner-only gate introduced) --

  it("ACCEPTS a first-ever secret from an ADMIN when the device holds none", async () => {
    // The regression this closes: ADR-0022 lets an ADMIN commit an
    // add-member, and `redistributeSecretToGroup` fires after ANY add — so
    // for a member added by a non-owner admin, that admin's redistribute is
    // the ONLY source of the secret (the owner broadcasts only at mint).
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      ADMIN_LEAF,
      ROTATED,
    );

    expect(verdict).toBe("accept-admin-seed");
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(ROTATED),
    );
    await vi.waitFor(() => expect(subscribeGroup).toHaveBeenCalled());
  });

  it("ACCEPTS a changed secret from an ADMIN over a held one (ADR-0043 option (b))", async () => {
    // The ratified widening. ADR-0022 lets an ADMIN commit the Remove that
    // makes an ADR-0043 rotation necessary; refusing the overwrite left an
    // admin able to evict a member but unable to retire that member's
    // routing id, so the eviction completed with the ex-member's wake stream
    // intact. Reported as `accept-admin-rotate`, distinct from the seed case.
    seedStoredSecret(ACCT, GRP, HELD);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      ADMIN_LEAF,
      ROTATED,
    );

    expect(verdict).toBe("accept-admin-rotate");
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(ROTATED),
    );
    await vi.waitFor(() => expect(subscribeGroup).toHaveBeenCalled());
  });

  it("ACCEPTS an OWNER rotation over an ADMIN-seeded secret (owner takes priority)", async () => {
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    // 1. Admin seeds the hole.
    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, ADMIN_LEAF, HELD),
    ).toBe("accept-admin-seed");
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(HELD),
    );

    // 2. The owner's envelope arrives AFTER and still wins — no authority
    // level was persisted alongside the admin-seeded secret, so the owner
    // branch is decided without consulting who wrote it.
    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, OWNER_LEAF, ROTATED),
    ).toBe("accept-owner");
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(ROTATED),
    );
  });

  it("an ADMIN write over an UNREADABLE store is reported as a ROTATE, not a seed", async () => {
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);
    vi.mocked(getGroupRoutingSecret).mockRejectedValueOnce(
      new Error("store unreadable"),
    );

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      ADMIN_LEAF,
      ROTATED,
    );

    // `storedSecret === null` here means "unknown", not "provably absent",
    // so it is NOT a seed. Under ADR-0043 option (b) the admin is authorised
    // either way, so the distinction only picks the verdict NAME — which
    // keeps the log honest about what was actually observed.
    expect(verdict).toBe("accept-admin-rotate");
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
  });

  it("an ADMIN key not in the role table's admin set is a plain member", async () => {
    // Same sender leaf, but the role table lists NO admins.
    const group = mockRoleGroup(GROUP_ID, OWNER_PK, { admins: [] });

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      ADMIN_LEAF,
      ROTATED,
    );

    expect(verdict).toBe("reject-not-authorized");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });

  it("fails CLOSED on a torn group: changed secret dropped, identical accepted", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    const torn = mockRoleGroup(GROUP_ID, OWNER_PK, { throws: true });

    // Changed + unverifiable -> dropped.
    expect(
      await receivePushRoutingSecretGated(ACCT, torn, GRP, OWNER_LEAF, ROTATED),
    ).toBe("reject-unverifiable");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();

    // Identical is still accepted — it changes nothing, so it needs no
    // authority.
    expect(
      await receivePushRoutingSecretGated(
        ACCT,
        torn,
        GRP,
        OWNER_LEAF,
        new Uint8Array(HELD),
      ),
    ).toBe("accept-identical");
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
  });

  it("REJECTS a changed secret carrying NO sender leaf (unverifiable authorship)", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      undefined,
      ROTATED,
    );

    expect(verdict).toBe("reject-unverifiable");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(subscribeGroup).not.toHaveBeenCalled();
  });

  it("REJECTS a changed secret from an UNKNOWN leaf (not in the member snapshot)", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      99,
      ROTATED,
    );

    expect(verdict).toBe("reject-unverifiable");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });
});

describe("routingSecretVerdict (pure decision core)", () => {
  const A = new Uint8Array([1, 2, 3]);
  const B = new Uint8Array([9, 9, 9]);
  const base = {
    verifyFailed: false,
    storedUnreadable: false,
    adminKeys: [ADMIN_PK],
  };

  it("identical short-circuits regardless of authorship", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: OWNER_PK,
        senderKey: MEMBER_PK,
        storedSecret: new Uint8Array(A),
        incomingSecret: new Uint8Array(A),
      }),
    ).toBe("accept-identical");
  });

  it("identical wins even when verification failed", () => {
    expect(
      routingSecretVerdict({
        ...base,
        verifyFailed: true,
        ownerKey: undefined,
        senderKey: undefined,
        storedSecret: new Uint8Array(A),
        incomingSecret: new Uint8Array(A),
      }),
    ).toBe("accept-identical");
  });

  it("owner-authored change accepted", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: OWNER_PK,
        senderKey: new Uint8Array(OWNER_PK),
        storedSecret: A,
        incomingSecret: B,
      }),
    ).toBe("accept-owner");
  });

  it("owner outranks the admin set even when also listed as an admin", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: OWNER_PK,
        adminKeys: [ADMIN_PK, OWNER_PK],
        senderKey: new Uint8Array(OWNER_PK),
        // Stored + changed: the admin branch would REJECT this, the owner
        // branch accepts. Owner is checked first, so it wins.
        storedSecret: A,
        incomingSecret: B,
      }),
    ).toBe("accept-owner");
  });

  it("admin seeds an empty store", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: OWNER_PK,
        senderKey: new Uint8Array(ADMIN_PK),
        storedSecret: null,
        incomingSecret: B,
      }),
    ).toBe("accept-admin-seed");
  });

  it("admin MAY overwrite a held secret — reported as a rotate (ADR-0043 (b))", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: OWNER_PK,
        senderKey: new Uint8Array(ADMIN_PK),
        storedSecret: A,
        incomingSecret: B,
      }),
    ).toBe("accept-admin-rotate");
  });

  it("an unreadable store makes an admin write a ROTATE, not a seed", () => {
    expect(
      routingSecretVerdict({
        ...base,
        storedUnreadable: true,
        ownerKey: OWNER_PK,
        senderKey: new Uint8Array(ADMIN_PK),
        storedSecret: null,
        incomingSecret: B,
      }),
    ).toBe("accept-admin-rotate");
  });

  it("a plain member is STILL refused, held or not (the tier boundary held)", () => {
    // The admin widening must not leak downward: only owner + admin write.
    for (const storedSecret of [A, null]) {
      expect(
        routingSecretVerdict({
          ...base,
          ownerKey: OWNER_PK,
          senderKey: new Uint8Array(MEMBER_PK),
          storedSecret,
          incomingSecret: B,
        }),
      ).toBe("reject-not-authorized");
    }
  });

  it("plain-member change rejected (held or not)", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: OWNER_PK,
        senderKey: MEMBER_PK,
        storedSecret: A,
        incomingSecret: B,
      }),
    ).toBe("reject-not-authorized");
    // A plain member gets NO hole-fill privilege — that is the admin tier.
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: OWNER_PK,
        senderKey: MEMBER_PK,
        storedSecret: null,
        incomingSecret: B,
      }),
    ).toBe("reject-not-authorized");
  });

  it("an empty admin set leaves everyone but the owner unauthorized", () => {
    expect(
      routingSecretVerdict({
        ...base,
        adminKeys: [],
        ownerKey: OWNER_PK,
        senderKey: new Uint8Array(ADMIN_PK),
        storedSecret: null,
        incomingSecret: B,
      }),
    ).toBe("reject-not-authorized");
  });

  it("no role table accepts the change", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: undefined,
        senderKey: MEMBER_PK,
        storedSecret: A,
        incomingSecret: B,
      }),
    ).toBe("accept-no-owner");
  });

  it("an unresolved sender is unverifiable, admin set notwithstanding", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: OWNER_PK,
        senderKey: undefined,
        storedSecret: null,
        incomingSecret: B,
      }),
    ).toBe("reject-unverifiable");
  });

  it("a length-mismatched stored secret is NOT identical", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: OWNER_PK,
        senderKey: MEMBER_PK,
        storedSecret: new Uint8Array([1, 2, 3, 4]),
        incomingSecret: new Uint8Array([1, 2, 3]),
      }),
    ).toBe("reject-not-authorized");
  });

  it("verdictAccepts partitions the verdict space", () => {
    expect(verdictAccepts("accept-identical")).toBe(true);
    expect(verdictAccepts("accept-owner")).toBe(true);
    expect(verdictAccepts("accept-admin-seed")).toBe(true);
    expect(verdictAccepts("accept-admin-rotate")).toBe(true);
    expect(verdictAccepts("accept-no-owner")).toBe(true);
    expect(verdictAccepts("reject-not-authorized")).toBe(false);
    expect(verdictAccepts("reject-unverifiable")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// ADR-0043 — rotation on member removal
// ---------------------------------------------------------------------------

describe("membershipRotationDecision (pure decision core, ADR-0043)", () => {
  const A = "aa".repeat(32);
  const B = "bb".repeat(32);
  const C = "cc".repeat(32);

  it("a SHRINK rotates for the OWNER", () => {
    expect(
      membershipRotationDecision({
        previousMemberKeys: [A, B, C],
        currentMemberKeys: [A, B],
        ownRole: "owner",
      }),
    ).toBe("rotate");
  });

  it("a SHRINK rotates for an ADMIN (ADR-0043 ratified option (b))", () => {
    // ADR-0022 lets an admin commit the Remove, so an admin must be able to
    // retire the evicted member's routing id — otherwise an admin-performed
    // eviction completes with the ex-member's wake stream intact.
    expect(
      membershipRotationDecision({
        previousMemberKeys: [A, B, C],
        currentMemberKeys: [A, B],
        ownRole: "admin",
      }),
    ).toBe("rotate");
  });

  it("a SHRINK does NOT rotate for a plain member", () => {
    expect(
      membershipRotationDecision({
        previousMemberKeys: [A, B, C],
        currentMemberKeys: [A, B],
        ownRole: "member",
      }),
    ).toBe("skip-not-authorized");
  });

  it("a SHRINK does NOT rotate in a group with no role table", () => {
    // No role table ⇒ no owner ⇒ nobody ever minted a secret for this group,
    // so there is nothing to rotate; and letting every member rotate would
    // have them each mint a divergent secret.
    expect(
      membershipRotationDecision({
        previousMemberKeys: [A, B, C],
        currentMemberKeys: [A, B],
        ownRole: null,
      }),
    ).toBe("skip-not-authorized");
  });

  it("GROWTH (an add) never rotates", () => {
    // Rotating on add would buy nothing (the new member is supposed to
    // route) and churn the id on the commonest membership event.
    expect(
      membershipRotationDecision({
        previousMemberKeys: [A, B],
        currentMemberKeys: [A, B, C],
        ownRole: "owner",
      }),
    ).toBe("skip-no-shrink");
  });

  it("an UNCHANGED roster never rotates (an ordinary commit / a bare poll)", () => {
    expect(
      membershipRotationDecision({
        previousMemberKeys: [A, B],
        currentMemberKeys: [A, B],
        ownRole: "owner",
      }),
    ).toBe("skip-no-shrink");
  });

  it("a remove+add in ONE batch still rotates (the count is equal, the SET is not)", () => {
    // Why the diff is over member keys, not a count: OpenMLS reuses a vacated
    // leaf for the next Add, so a batch carrying both leaves the size
    // identical while a member really was evicted.
    expect(
      membershipRotationDecision({
        previousMemberKeys: [A, B],
        currentMemberKeys: [A, C],
        ownRole: "owner",
      }),
    ).toBe("rotate");
  });

  it("an UNKNOWN snapshot never rotates (a torn members() read is not 'everyone left')", () => {
    expect(
      membershipRotationDecision({
        previousMemberKeys: null,
        currentMemberKeys: [A],
        ownRole: "owner",
      }),
    ).toBe("skip-unknown-membership");
    expect(
      membershipRotationDecision({
        previousMemberKeys: [A, B],
        currentMemberKeys: null,
        ownRole: "owner",
      }),
    ).toBe("skip-unknown-membership");
  });

  it("a FIRST observation (empty baseline) never rotates", () => {
    expect(
      membershipRotationDecision({
        previousMemberKeys: [],
        currentMemberKeys: [],
        ownRole: "owner",
      }),
    ).toBe("skip-unknown-membership");
  });

  it("memberSignatureKeys hexes the roster and returns null on a torn group", () => {
    const group = mockRoleGroup(new Uint8Array([1]), OWNER_PK);
    expect(memberSignatureKeys(group)).toEqual([
      toHex(OWNER_PK),
      toHex(MEMBER_PK),
      toHex(ADMIN_PK),
    ]);
    const torn = mockRoleGroup(new Uint8Array([1]), OWNER_PK, { throws: true });
    expect(memberSignatureKeys(torn)).toBeNull();
  });
});

describe("rotatePushRoutingSecret (ADR-0043)", () => {
  const ACCT = "acctRot";
  const GROUP_ID = new Uint8Array([2, 7, 1, 8, 2, 8]);
  const GRP_B64 = base64Encode(GROUP_ID);
  const OLD = new Uint8Array(32).fill(0x77);

  /** A broadcast sink that records every attempt and can be armed to fail. */
  function recordingBroadcast(): {
    sent: Uint8Array[];
    failNext: (n: number) => void;
    fn: (b: Uint8Array) => Promise<void>;
  } {
    const sent: Uint8Array[] = [];
    let failures = 0;
    return {
      sent,
      failNext: (n: number): void => {
        failures = n;
      },
      fn: async (b: Uint8Array): Promise<void> => {
        sent.push(b);
        if (failures > 0) {
          failures -= 1;
          throw new Error("DS unreachable");
        }
      },
    };
  }

  /** Seed a settled (already-distributed) secret for the group. */
  function seedSettled(secret: Uint8Array): void {
    seedStoredSecret(ACCT, GRP_B64, secret);
    h.distributed.set(`${ACCT}\0${GRP_B64}`, true);
    h.routes.set(toHex(wasm.derivePushGid(secret)), {
      accountId: ACCT,
      groupIdB64: GRP_B64,
    });
  }

  it("the OWNER re-keys: new secret persisted + broadcast, NEW gid subscribed + indexed, OLD gid unsubscribed + de-indexed", async () => {
    seedSettled(OLD);
    const oldGidHex = toHex(wasm.derivePushGid(OLD));
    const group = mockGroup(GROUP_ID, undefined);
    const b = recordingBroadcast();

    const outcome = await rotatePushRoutingSecret(ACCT, group, "owner", b.fn);
    expect(outcome).toBe("rotated");

    // A genuinely NEW secret replaced the old one, in place.
    const stored = h.secrets.get(`${ACCT}\0${GRP_B64}`) as Uint8Array;
    expect(stored.length).toBe(32);
    expect(Array.from(stored)).not.toEqual(Array.from(OLD));
    const newGidHex = toHex(wasm.derivePushGid(stored));
    expect(newGidHex).not.toBe(oldGidHex);

    // Redistributed to the group — the envelope carries the NEW secret.
    expect(b.sent.length).toBe(1);
    expect(Array.from(wasm.decodePushRoutingSecret(b.sent[0]!))).toEqual(
      Array.from(stored),
    );
    // Confirmed on the wire.
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);

    // Subscribed under the NEW gid.
    expect(subscribeGroup).toHaveBeenCalledTimes(1);
    expect(toHex(subscribeGroup.mock.calls[0]![1])).toBe(newGidHex);

    // Unsubscribed from the OLD gid — the first live call site of
    // `PushClient.unsubscribeGroup`, dead code before this ADR.
    expect(unsubscribeGroup).toHaveBeenCalledTimes(1);
    expect(toHex(unsubscribeGroup.mock.calls[0]![1])).toBe(oldGidHex);

    // The reverse index carries the new entry and NOT the retired one:
    // without the delete every rotation grows the map by a dead row.
    expect(h.routes.has(newGidHex)).toBe(true);
    expect(h.routes.has(oldGidHex)).toBe(false);
    expect(vi.mocked(deleteRoute)).toHaveBeenCalledWith(oldGidHex);
  });

  it("an ADMIN may rotate too (ADR-0043 ratified option (b))", async () => {
    seedSettled(OLD);
    const b = recordingBroadcast();

    expect(
      await rotatePushRoutingSecret(
        ACCT,
        mockGroup(GROUP_ID, undefined),
        "admin",
        b.fn,
      ),
    ).toBe("rotated");
    expect(b.sent.length).toBe(1);
    expect(unsubscribeGroup).toHaveBeenCalledTimes(1);
  });

  it("a PLAIN MEMBER never rotates (nothing minted, nothing sent, nothing torn down)", async () => {
    seedSettled(OLD);
    const b = recordingBroadcast();

    for (const role of ["member", null] as const) {
      expect(
        await rotatePushRoutingSecret(
          ACCT,
          mockGroup(GROUP_ID, undefined),
          role,
          b.fn,
        ),
      ).toBe("skip-not-authorized");
    }
    expect(b.sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(unsubscribeGroup).not.toHaveBeenCalled();
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP_B64}`) as Uint8Array)).toEqual(
      Array.from(OLD),
    );
  });

  it("rotation NEVER mints a first secret (minting stays owner-only in `ensure`)", async () => {
    // No secret held: an admin rotating here would become a SECOND minter and
    // diverge the group's push_gid, which is exactly what the owner-mints
    // rule exists to prevent. Option (b) widened OVERWRITE authority, not
    // MINT authority.
    const b = recordingBroadcast();

    expect(
      await rotatePushRoutingSecret(
        ACCT,
        mockGroup(GROUP_ID, undefined),
        "admin",
        b.fn,
      ),
    ).toBe("skip-no-secret");
    expect(b.sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });

  it("the OLD secret is read BEFORE the overwrite — the teardown targets the OLD gid", async () => {
    // The `secrets` row is keyed (account, group) and clobbered in place. If
    // the rotation persisted first, the id being retired could no longer be
    // named: an undeletable orphan that keeps receiving wakes — the exact
    // defect this ADR fixes. Pinned by asserting the unsubscribe target.
    seedSettled(OLD);
    const oldGidHex = toHex(wasm.derivePushGid(OLD));
    const b = recordingBroadcast();

    await rotatePushRoutingSecret(ACCT, mockGroup(GROUP_ID, undefined), "owner", b.fn);

    const newGidHex = toHex(
      wasm.derivePushGid(h.secrets.get(`${ACCT}\0${GRP_B64}`) as Uint8Array),
    );
    const torndown = toHex(unsubscribeGroup.mock.calls[0]![1]);
    expect(torndown).toBe(oldGidHex);
    expect(torndown).not.toBe(newGidHex);
    // ... and the device is armed under the new id before the old one is
    // dropped, so it is never unsubscribed from both at once.
    expect(subscribeGroup.mock.invocationCallOrder[0]!).toBeLessThan(
      unsubscribeGroup.mock.invocationCallOrder[0]!,
    );
  });

  it("a FAILED rotation broadcast leaves the row PENDING and the existing retry re-sends the NEW secret", async () => {
    // The dangerous window: the rotator has switched locally while every
    // other member is still on the old id, so nobody notifies either — the
    // whole group's wakes stop. The `distributed` flag (reused verbatim from
    // the mint path) is what makes it recoverable.
    seedSettled(OLD);
    const group = mockGroup(GROUP_ID, undefined);
    const b = recordingBroadcast();
    b.failNext(1);

    expect(await rotatePushRoutingSecret(ACCT, group, "owner", b.fn)).toBe(
      "rotated",
    );
    const rotated = h.secrets.get(`${ACCT}\0${GRP_B64}`) as Uint8Array;
    expect(b.sent.length).toBe(1);
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(false);
    // The local switch is durable regardless of the failed send.
    expect(unsubscribeGroup).toHaveBeenCalledTimes(1);

    // Next chat-open / launch: `ensureGroupPushRouting`'s re-broadcast branch
    // re-sends the STORED (i.e. the newly rotated) secret. Never a re-mint.
    await ensureGroupPushRouting(ACCT, group, DUMMY_IDENTITY, b.fn);
    expect(b.sent.length).toBe(2);
    expect(Array.from(wasm.decodePushRoutingSecret(b.sent[1]!))).toEqual(
      Array.from(rotated),
    );
    expect(h.distributed.get(`${ACCT}\0${GRP_B64}`)).toBe(true);
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP_B64}`) as Uint8Array)).toEqual(
      Array.from(rotated),
    );
  });

  it("the per-group lock SERIALISES concurrent rotations (never two mints off one old secret)", async () => {
    // The damaging interleave: this device's own `onRemoveMember` and a
    // concurrent poll-loop diff both read the SAME old secret, both mint, the
    // second persist wins — leaving the first new push_gid subscribed and
    // indexed but never notified again, i.e. exactly the orphan this ADR
    // exists to prevent. The lock must serialise them, not fold them.
    seedSettled(OLD);
    const oldGidHex = toHex(wasm.derivePushGid(OLD));
    const group = mockGroup(GROUP_ID, undefined);

    // Park the FIRST rotation inside its broadcast — that is the window (of
    // three network round-trips) in which the second removal happens.
    let release!: () => void;
    const gate = new Promise<void>((r) => {
      release = r;
    });
    const sent: Uint8Array[] = [];
    const broadcast = async (bytes: Uint8Array): Promise<void> => {
      sent.push(bytes);
      if (sent.length === 1) await gate;
    };

    const first = rotatePushRoutingSecret(ACCT, group, "owner", broadcast);
    await flushMicrotasks();
    expect(sent.length).toBe(1); // parked inside its broadcast

    // A SECOND, DIFFERENT removal is observed while the first is in flight.
    const second = rotatePushRoutingSecret(ACCT, group, "owner", broadcast);
    await flushMicrotasks();
    // Behind the lock: without it the second would ALREADY have minted off the
    // same old secret; folded into the first it would never mint at all.
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    expect(sent.length).toBe(1);

    release();
    expect(await Promise.all([first, second])).toEqual(["rotated", "rotated"]);

    // TWO real rotations: the second evictee was still a member at the epoch
    // the FIRST envelope was sealed at, so a second seal is the whole point.
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(2);
    expect(sent.length).toBe(2);
    const s1 = wasm.decodePushRoutingSecret(sent[0]!);
    const s2 = wasm.decodePushRoutingSecret(sent[1]!);
    expect(Array.from(s2)).not.toEqual(Array.from(s1));

    // Serialised, so the second rotation's "old" secret is the FIRST one's
    // output — every intermediate id is retired in turn, none stranded.
    const stored = h.secrets.get(`${ACCT}\0${GRP_B64}`) as Uint8Array;
    expect(Array.from(stored)).toEqual(Array.from(s2));
    const gid1 = toHex(wasm.derivePushGid(s1));
    const finalGid = toHex(wasm.derivePushGid(s2));
    expect(unsubscribeGroup).toHaveBeenCalledTimes(2);
    expect(unsubscribeGroup.mock.calls.map((c) => toHex(c[1]))).toEqual([
      oldGidHex,
      gid1,
    ]);

    // Exactly one live route row — the reverse index carries only the id that
    // is actually being notified.
    expect(h.routes.size).toBe(1);
    expect(h.routes.has(finalGid)).toBe(true);
    expect(h.routes.has(oldGidHex)).toBe(false);
    expect(h.routes.has(gid1)).toBe(false);
  });

  it("a rotation queued behind a FAILING one still runs (a predecessor's failure is not contagious)", async () => {
    // The removal that triggered the second rotation still needs a re-key
    // regardless of why the first one blew up.
    seedSettled(OLD);
    const group = mockGroup(GROUP_ID, undefined);

    let release!: () => void;
    const gate = new Promise<void>((r) => {
      release = r;
    });
    let calls = 0;
    const broadcast = async (): Promise<void> => {
      calls += 1;
      if (calls === 1) {
        await gate;
        throw new Error("boom");
      }
    };

    const first = rotatePushRoutingSecret(ACCT, group, "owner", broadcast);
    await flushMicrotasks();
    const second = rotatePushRoutingSecret(ACCT, group, "owner", broadcast);
    await flushMicrotasks();
    release();

    // The broadcast failure is swallowed into a PENDING row (existing
    // behaviour), so both still report `rotated` — what matters here is that
    // the second one actually ran.
    await Promise.allSettled([first, second]);
    expect(calls).toBe(2);
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(2);
  });
});

describe("readPushRotationRole — the LIVE role read (lakko-security F2)", () => {
  /** A group whose role the test can flip between reads, modelling a poll
   *  batch that lands a demotion. */
  function mutableRoleGroup(): {
    group: WasmGroup;
    setRole: (r: number | undefined) => void;
  } {
    let role: number | undefined = wasm.Role.Admin;
    return {
      group: {
        get groupId(): Uint8Array {
          return new Uint8Array([9, 9]);
        },
        ownRole: (_id: WasmIdentity): number | undefined => role,
      } as unknown as WasmGroup,
      setRole: (r): void => {
        role = r;
      },
    };
  }

  it("maps the ADR-0022 role table onto the rotation role", async () => {
    const m = mutableRoleGroup();
    for (const [wasmRole, expected] of [
      [wasm.Role.Owner, "owner"],
      [wasm.Role.Admin, "admin"],
      [wasm.Role.Member, "member"],
      [undefined, null], // open / legacy group with no role table
    ] as const) {
      m.setRole(wasmRole);
      expect(await readPushRotationRole(m.group, DUMMY_IDENTITY)).toBe(expected);
    }
  });

  it("re-reads the group EVERY call, so a demotion is observed immediately", async () => {
    // The F2 defect: the chat view holds `ownRole` in a signal refreshed by an
    // async effect, so at a poll-path rotation call site it still carries the
    // PRE-poll role. A batch carrying a demotion AND a removal would then have
    // a just-demoted admin rotate — every peer refuses the envelope on the
    // receive-side role gate and the rotator strands itself on a secret nobody
    // else holds. A live read observes the demotion the moment it lands.
    const m = mutableRoleGroup();
    expect(await readPushRotationRole(m.group, DUMMY_IDENTITY)).toBe("admin");

    m.setRole(wasm.Role.Member); // the demotion lands in this poll batch
    expect(await readPushRotationRole(m.group, DUMMY_IDENTITY)).toBe("member");

    // ... and a demoted device does not rotate, so nothing is stranded.
    seedStoredSecret("acctF2", base64Encode(new Uint8Array([9, 9])), OLD_F2);
    expect(
      await rotatePushRoutingSecretIfMembershipShrank({
        accountId: "acctF2",
        group: m.group,
        ownRole: await readPushRotationRole(m.group, DUMMY_IDENTITY),
        previousMemberKeys: ["aa".repeat(32), "bb".repeat(32)],
        currentMemberKeys: ["aa".repeat(32)],
        broadcast: async () => {
          throw new Error("a demoted device must not broadcast");
        },
      }),
    ).toBe("skip-not-authorized");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });

  it("a torn group reads as `null` — fail-closed, never rotates", async () => {
    const torn = {
      get groupId(): Uint8Array {
        return new Uint8Array([9, 9]);
      },
      ownRole: (): number | undefined => {
        throw new Error("torn group");
      },
    } as unknown as WasmGroup;
    expect(await readPushRotationRole(torn, DUMMY_IDENTITY)).toBeNull();
  });
});

const OLD_F2 = new Uint8Array(32).fill(0x5a);

describe("rotation trigger — the membership-shrink entry point (ADR-0043)", () => {
  const ACCT = "acctTrig";
  const GROUP_ID = new Uint8Array([5, 5, 5, 5]);
  const GRP_B64 = base64Encode(GROUP_ID);
  const HELD = new Uint8Array(32).fill(0x66);
  const A = toHex(OWNER_PK);
  const B = toHex(MEMBER_PK);
  const C = toHex(ADMIN_PK);

  function seedSettled(): void {
    seedStoredSecret(ACCT, GRP_B64, HELD);
    h.distributed.set(`${ACCT}\0${GRP_B64}`, true);
    h.routes.set(toHex(wasm.derivePushGid(HELD)), {
      accountId: ACCT,
      groupIdB64: GRP_B64,
    });
  }

  async function trigger(
    ownRole: "owner" | "admin" | "member" | null,
    previous: readonly string[] | null,
    current: readonly string[] | null,
    sent: Uint8Array[],
  ): Promise<string> {
    return rotatePushRoutingSecretIfMembershipShrank({
      accountId: ACCT,
      group: mockGroup(GROUP_ID, undefined),
      ownRole,
      previousMemberKeys: previous,
      currentMemberKeys: current,
      broadcast: async (bytes) => {
        sent.push(bytes);
      },
    });
  }

  it("an observed SHRINK rotates on the OWNER", async () => {
    seedSettled();
    const sent: Uint8Array[] = [];
    expect(await trigger("owner", [A, B, C], [A, B], sent)).toBe("rotated");
    expect(sent.length).toBe(1);
    expect(unsubscribeGroup).toHaveBeenCalledTimes(1);
    expect(toHex(unsubscribeGroup.mock.calls[0]![1])).toBe(
      toHex(wasm.derivePushGid(HELD)),
    );
  });

  it("an observed SHRINK rotates on an ADMIN", async () => {
    seedSettled();
    const sent: Uint8Array[] = [];
    expect(await trigger("admin", [A, B, C], [A, B], sent)).toBe("rotated");
    expect(sent.length).toBe(1);
  });

  it("an observed SHRINK does NOT rotate on a plain member", async () => {
    seedSettled();
    const sent: Uint8Array[] = [];
    expect(await trigger("member", [A, B, C], [A, B], sent)).toBe(
      "skip-not-authorized",
    );
    expect(sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(unsubscribeGroup).not.toHaveBeenCalled();
    // The plain member still ends up on the new id — via the gated receive
    // path, when the rotator's envelope arrives.
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP_B64}`) as Uint8Array)).toEqual(
      Array.from(HELD),
    );
  });

  it("an ADD never rotates (the routing id stays stable across ordinary commits)", async () => {
    seedSettled();
    const sent: Uint8Array[] = [];
    expect(await trigger("owner", [A, B], [A, B, C], sent)).toBe(
      "skip-no-shrink",
    );
    expect(sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(unsubscribeGroup).not.toHaveBeenCalled();
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP_B64}`) as Uint8Array)).toEqual(
      Array.from(HELD),
    );
  });

  it("a poll that changed nothing never rotates", async () => {
    seedSettled();
    const sent: Uint8Array[] = [];
    expect(await trigger("owner", [A, B], [A, B], sent)).toBe("skip-no-shrink");
    expect(sent.length).toBe(0);
  });
});

describe("receive-side teardown of a superseded push_gid (ADR-0043)", () => {
  const ACCT = "acctRecv";
  const GRP = "grpRecvB64";
  const GROUP_ID = new Uint8Array([8, 8, 8, 8]);
  const HELD = new Uint8Array(32).fill(0x11);
  const ROTATED = new Uint8Array(32).fill(0x22);

  it("a NON-rotator applying a CHANGED secret retires its OWN old gid", async () => {
    // Every member must tear its own row down: `group_unsubscribe` deletes
    // exactly the (device, group) row named in the request and a client knows
    // only its own device pseudonym, so the rotator cannot do it for anyone
    // else. Skip this and each rotation leaves one dead row PER MEMBER — the
    // versioned-cluster archive ADR-0043 calls worse than not rotating.
    seedStoredSecret(ACCT, GRP, HELD);
    h.routes.set(toHex(wasm.derivePushGid(HELD)), {
      accountId: ACCT,
      groupIdB64: GRP,
    });
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, OWNER_LEAF, ROTATED),
    ).toBe("accept-owner");

    const oldGidHex = toHex(wasm.derivePushGid(HELD));
    const newGidHex = toHex(wasm.derivePushGid(ROTATED));
    await vi.waitFor(() => expect(unsubscribeGroup).toHaveBeenCalled());
    expect(toHex(unsubscribeGroup.mock.calls[0]![1])).toBe(oldGidHex);
    expect(h.routes.has(oldGidHex)).toBe(false);
    expect(h.routes.has(newGidHex)).toBe(true);
  });

  it("an ADMIN-authored rotation is applied and torn down the same way", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    h.routes.set(toHex(wasm.derivePushGid(HELD)), {
      accountId: ACCT,
      groupIdB64: GRP,
    });
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, ADMIN_LEAF, ROTATED),
    ).toBe("accept-admin-rotate");
    await vi.waitFor(() => expect(unsubscribeGroup).toHaveBeenCalled());
    expect(toHex(unsubscribeGroup.mock.calls[0]![1])).toBe(
      toHex(wasm.derivePushGid(HELD)),
    );
  });

  it("an IDENTICAL re-broadcast retires nothing (the id did not change)", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(
        ACCT,
        group,
        GRP,
        MEMBER_LEAF,
        new Uint8Array(HELD),
      ),
    ).toBe("accept-identical");
    expect(unsubscribeGroup).not.toHaveBeenCalled();
    expect(vi.mocked(deleteRoute)).not.toHaveBeenCalled();
  });

  it("a FIRST-EVER secret retires nothing (there is no old id)", async () => {
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, ADMIN_LEAF, ROTATED),
    ).toBe("accept-admin-seed");
    expect(unsubscribeGroup).not.toHaveBeenCalled();
    expect(vi.mocked(deleteRoute)).not.toHaveBeenCalled();
  });

  it("a REFUSED secret retires nothing (the device keeps its id)", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, MEMBER_LEAF, ROTATED),
    ).toBe("reject-not-authorized");
    expect(unsubscribeGroup).not.toHaveBeenCalled();
    expect(vi.mocked(deleteRoute)).not.toHaveBeenCalled();
  });
});

// base64 of the group_id — recomputed the same way `base64Encode` (real,
// used by the orchestration) does, to assert the reverse-map entry.
function base64OfGroupId(groupId: Uint8Array): string {
  return base64Encode(groupId);
}
