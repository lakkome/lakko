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
vi.mock("./client", () => ({
  DEFAULT_BASE_URL: "/api/push",
  PushClient: class {
    subscribeGroup = subscribeGroup;
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
  mintPushRoutingSecret,
  receivePushRoutingSecret,
  receivePushRoutingSecretGated,
  routingSecretVerdict,
  subscribeGroupUnderPushGid,
  tryDecodeSecretEnvelope,
  verdictAccepts,
} from "./group-routing";
import { getGroupRoutingSecret, putGroupRouting } from "./group-routing-store";
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
  vi.mocked(putGroupRouting).mockClear();
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
    // Only the owner mints or re-broadcasts; a member with no secret defers.
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

  it("REJECTS a changed secret from an ADMIN when the device already holds one", async () => {
    seedStoredSecret(ACCT, GRP, HELD);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      ADMIN_LEAF,
      ROTATED,
    );

    // An admin may seed a hole, never rotate — only the owner rotates.
    expect(verdict).toBe("reject-admin-overwrite");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(subscribeGroup).not.toHaveBeenCalled();
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(HELD),
    );
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

  it("REJECTS an ADMIN seed when the local store is UNREADABLE (hole unprovable)", async () => {
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

    // `storedSecret === null` here means "unknown", not "provably absent" —
    // fail-closed rather than let an admin overwrite what we may hold.
    expect(verdict).toBe("reject-admin-overwrite");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
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

  it("admin cannot overwrite a held secret", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: OWNER_PK,
        senderKey: new Uint8Array(ADMIN_PK),
        storedSecret: A,
        incomingSecret: B,
      }),
    ).toBe("reject-admin-overwrite");
  });

  it("admin cannot seed when the store is unreadable (hole unprovable)", () => {
    expect(
      routingSecretVerdict({
        ...base,
        storedUnreadable: true,
        ownerKey: OWNER_PK,
        senderKey: new Uint8Array(ADMIN_PK),
        storedSecret: null,
        incomingSecret: B,
      }),
    ).toBe("reject-admin-overwrite");
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
    expect(verdictAccepts("accept-no-owner")).toBe(true);
    expect(verdictAccepts("reject-admin-overwrite")).toBe(false);
    expect(verdictAccepts("reject-not-authorized")).toBe(false);
    expect(verdictAccepts("reject-unverifiable")).toBe(false);
  });
});

// base64 of the group_id — recomputed the same way `base64Encode` (real,
// used by the orchestration) does, to assert the reverse-map entry.
function base64OfGroupId(groupId: Uint8Array): string {
  return base64Encode(groupId);
}
