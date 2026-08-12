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
// `mintEpochs` mirrors the ADR-0044 field the same way: a key ABSENT models a
// row written before that field existed, which the store reads as `0n`.
const h = vi.hoisted(() => ({
  secrets: new Map<string, Uint8Array>(),
  distributed: new Map<string, boolean>(),
  mintEpochs: new Map<string, bigint>(),
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
        mintEpoch: bigint;
      },
      _deviceKey: unknown,
    ) => {
      h.secrets.set(`${args.accountId}\0${args.groupIdB64}`, args.secret);
      h.distributed.set(
        `${args.accountId}\0${args.groupIdB64}`,
        args.distributed,
      );
      h.mintEpochs.set(
        `${args.accountId}\0${args.groupIdB64}`,
        args.mintEpoch,
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
  // ADR-0044 — the pair reader. Mirrors the real store: the epoch and the
  // secret come from ONE row, and a legacy row (no recorded epoch) reads 0n.
  getGroupRoutingRecord: vi.fn(
    async (accountId: string, groupIdB64: string, _dk: unknown) => {
      const k = `${accountId}\0${groupIdB64}`;
      const secret = h.secrets.get(k);
      if (secret === undefined) return null;
      return { secret, mintEpoch: h.mintEpochs.get(k) ?? 0n };
    },
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
  publishRemoveThenRotate,
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
  PUSH_ROUTING_ENVELOPE_LEN,
} from "./group-routing";
import {
  deleteRoute,
  getGroupRoutingRecord,
  putGroupRouting,
} from "./group-routing-store";
import { base64Encode } from "../auth/account-id";

// ---- helpers -------------------------------------------------------------

function toHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/**
 * The MLS epoch every mock group reports unless a test says otherwise.
 * Deliberately well above the epochs the tests stamp their fixture secrets
 * with, so an ordinary case never trips the ADR-0044 future clamp and the
 * clamp tests have to opt in.
 */
const GROUP_EPOCH = 100n;

/** ADR-0044 — the epoch the receive-gate fixtures' already-HELD secret was
 *  minted at, and the strictly-greater epoch a rotation of it carries. Both
 *  well below {@link GROUP_EPOCH}, so an ordinary gate case never trips the
 *  future clamp. */
const HELD_EPOCH = 10n;
const ROTATED_EPOCH = 11n;

/** A mock group with a fixed group_id + a role the test controls. Its
 *  `epoch` getter is the ADR-0044 stamp source (mint) and clamp ceiling
 *  (receive); `null` makes the read THROW, modelling a torn group. */
function mockGroup(
  groupId: Uint8Array,
  role: number | undefined,
  epoch: bigint | null = GROUP_EPOCH,
): WasmGroup {
  return {
    get groupId(): Uint8Array {
      return groupId;
    },
    get epoch(): bigint {
      if (epoch === null) throw new Error("torn group");
      return epoch;
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
 * @param opts.epoch the ADR-0044 clamp ceiling this group reports.
 */
function mockRoleGroup(
  groupId: Uint8Array,
  ownerKey: Uint8Array | undefined,
  opts: { throws?: boolean; admins?: Uint8Array[]; epoch?: bigint } = {},
): WasmGroup {
  const admins = opts.admins ?? [ADMIN_PK];
  return {
    get groupId(): Uint8Array {
      return groupId;
    },
    get epoch(): bigint {
      if (opts.throws === true) throw new Error("torn group");
      return opts.epoch ?? GROUP_EPOCH;
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

/**
 * Seed this device's already-held secret for `(account, group)`.
 *
 * `mintEpoch` defaults to `undefined` = NO recorded epoch, i.e. the legacy
 * on-disk shape, which the store reads as `0n`. Tests that care about the
 * ADR-0044 ordering pass one explicitly.
 */
function seedStoredSecret(
  accountId: string,
  groupIdB64: string,
  secret: Uint8Array,
  mintEpoch?: bigint,
): void {
  h.secrets.set(`${accountId}\0${groupIdB64}`, secret);
  if (mintEpoch !== undefined) {
    h.mintEpochs.set(`${accountId}\0${groupIdB64}`, mintEpoch);
  }
}

/** Decode a broadcast envelope back to its `(mintEpoch, secret)` pair. */
function decodeEnvelope(bytes: Uint8Array): {
  mintEpoch: bigint;
  secret: Uint8Array;
} {
  const d = wasm.decodePushRoutingSecret(bytes);
  return { mintEpoch: d.mintEpoch, secret: d.secret };
}

/** Just the 32-byte secret out of a broadcast envelope. */
function decodeSecret(bytes: Uint8Array): Uint8Array {
  return decodeEnvelope(bytes).secret;
}

/** Just the ADR-0044 stamp out of a broadcast envelope. */
function decodeMintEpoch(bytes: Uint8Array): bigint {
  return decodeEnvelope(bytes).mintEpoch;
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
  h.mintEpochs.clear();
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
    const decoded = decodeSecret(broadcasts[0]!);
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
    const envelope = await composeSecretEnvelope(42n, secret);
    expect(envelope.length).toBe(PUSH_ROUTING_ENVELOPE_LEN);
    const ownerPushGid = wasm.derivePushGid(secret);

    // Non-owner decodes the envelope back to the exact secret AND the
    // minter's stamp — the pair the receive gate orders on.
    const decoded = await tryDecodeSecretEnvelope(envelope);
    expect(decoded).not.toBeNull();
    expect(Array.from(decoded!.secret)).toEqual(Array.from(secret));
    expect(decoded!.mintEpoch).toBe(42n);

    // ... persists it under its own account + subscribes.
    await receivePushRoutingSecret(
      "acctPeer",
      "grpB64Peer",
      decoded!.secret,
      decoded!.mintEpoch,
    );

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
    expect(Array.from(decodeSecret(b.sent[1]!))).toEqual(
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
    await receivePushRoutingSecret(ACCT, GRP_B64, secret, 7n);

    // ADR-0044 — the MINTER's stamp is persisted, not the receiver's current
    // epoch: a re-broadcast from here must replay the minter's position in
    // the order, never invent a fresher one.
    expect(vi.mocked(putGroupRouting).mock.calls[0]![0].mintEpoch).toBe(7n);
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
    expect(Array.from(decodeSecret(b.sent[2]!))).toEqual(
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
    expect(Array.from(decodeSecret(b.sent[1]!))).toEqual(
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
    expect(Array.from(decodeSecret(b.sent[0]!))).toEqual(
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
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      OWNER_LEAF,
      ROTATED,
      ROTATED_EPOCH,
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
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      MEMBER_LEAF,
      ROTATED,
      ROTATED_EPOCH,
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
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      MEMBER_LEAF,
      // byte-identical to what we already hold (a fresh array, so this is a
      // value comparison, not a reference one)
      new Uint8Array(HELD),
      HELD_EPOCH,
    );

    expect(verdict).toBe("accept-identical");
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(HELD),
    );
  });

  it("ACCEPTS a changed secret in a group with NO role table (legacy / open)", async () => {
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    const group = mockRoleGroup(GROUP_ID, undefined);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      MEMBER_LEAF,
      ROTATED,
      ROTATED_EPOCH,
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
      ROTATED_EPOCH,
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
      ROTATED_EPOCH,
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
      ROTATED_EPOCH,
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
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      ADMIN_LEAF,
      ROTATED,
      ROTATED_EPOCH,
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
      await receivePushRoutingSecretGated(ACCT, group, GRP, ADMIN_LEAF, HELD, HELD_EPOCH),
    ).toBe("accept-admin-seed");
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(HELD),
    );

    // 2. The owner's envelope arrives AFTER and still wins — no authority
    // level was persisted alongside the admin-seeded secret, so the owner
    // branch is decided without consulting who wrote it.
    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, OWNER_LEAF, ROTATED, ROTATED_EPOCH),
    ).toBe("accept-owner");
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(ROTATED),
    );
  });

  it("an ADMIN write over an UNREADABLE store is reported as a ROTATE, not a seed", async () => {
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);
    vi.mocked(getGroupRoutingRecord).mockRejectedValueOnce(
      new Error("store unreadable"),
    );

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      ADMIN_LEAF,
      ROTATED,
      ROTATED_EPOCH,
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
      ROTATED_EPOCH,
    );

    expect(verdict).toBe("reject-not-authorized");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });

  it("fails CLOSED on a torn group: changed secret dropped, identical accepted", async () => {
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    const torn = mockRoleGroup(GROUP_ID, OWNER_PK, { throws: true });

    // Changed + unverifiable -> dropped.
    expect(
      await receivePushRoutingSecretGated(ACCT, torn, GRP, OWNER_LEAF, ROTATED, ROTATED_EPOCH),
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
        HELD_EPOCH,
      ),
    ).toBe("accept-identical");
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
  });

  it("REJECTS a changed secret carrying NO sender leaf (unverifiable authorship)", async () => {
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      undefined,
      ROTATED,
      ROTATED_EPOCH,
    );

    expect(verdict).toBe("reject-unverifiable");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(subscribeGroup).not.toHaveBeenCalled();
  });

  it("REJECTS a changed secret from an UNKNOWN leaf (not in the member snapshot)", async () => {
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    const verdict = await receivePushRoutingSecretGated(
      ACCT,
      group,
      GRP,
      99,
      ROTATED,
      ROTATED_EPOCH,
    );

    expect(verdict).toBe("reject-unverifiable");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });
});

describe("routingSecretVerdict (pure decision core)", () => {
  // Real 32-byte secrets. They have to be: ADR-0044's order is fail-closed on
  // a wrong-length secret (a malformed value must never displace a stored
  // one), so a 3-byte fixture would land every stored-secret case on
  // `reject-stale` and mask the AUTHORITY question these cases are about.
  const A = new Uint8Array(32).fill(0x0a);
  const B = new Uint8Array(32).fill(0x0b);
  // The base makes the incoming envelope strictly newer, so each case below
  // isolates authority; the ordering itself has its own describe. `isNewer`
  // is the REAL wasm export, never a stub — routing every decision through
  // the one audited comparison is the entire point of that FFI surface.
  const base = {
    verifyFailed: false,
    storedUnreadable: false,
    adminKeys: [ADMIN_PK],
    storedMintEpoch: 1n,
    incomingMintEpoch: 2n,
    ownEpoch: GROUP_EPOCH,
    isNewer: wasm.pushRoutingSecretIsNewer,
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
    expect(Array.from(decodeSecret(b.sent[0]!))).toEqual(
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
    expect(Array.from(decodeSecret(b.sent[1]!))).toEqual(
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
    const s1 = decodeSecret(sent[0]!);
    const s2 = decodeSecret(sent[1]!);
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
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    h.routes.set(toHex(wasm.derivePushGid(HELD)), {
      accountId: ACCT,
      groupIdB64: GRP,
    });
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, OWNER_LEAF, ROTATED, ROTATED_EPOCH),
    ).toBe("accept-owner");

    const oldGidHex = toHex(wasm.derivePushGid(HELD));
    const newGidHex = toHex(wasm.derivePushGid(ROTATED));
    await vi.waitFor(() => expect(unsubscribeGroup).toHaveBeenCalled());
    expect(toHex(unsubscribeGroup.mock.calls[0]![1])).toBe(oldGidHex);
    expect(h.routes.has(oldGidHex)).toBe(false);
    expect(h.routes.has(newGidHex)).toBe(true);
  });

  it("an ADMIN-authored rotation is applied and torn down the same way", async () => {
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    h.routes.set(toHex(wasm.derivePushGid(HELD)), {
      accountId: ACCT,
      groupIdB64: GRP,
    });
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, ADMIN_LEAF, ROTATED, ROTATED_EPOCH),
    ).toBe("accept-admin-rotate");
    await vi.waitFor(() => expect(unsubscribeGroup).toHaveBeenCalled());
    expect(toHex(unsubscribeGroup.mock.calls[0]![1])).toBe(
      toHex(wasm.derivePushGid(HELD)),
    );
  });

  it("an IDENTICAL re-broadcast retires nothing (the id did not change)", async () => {
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(
        ACCT,
        group,
        GRP,
        MEMBER_LEAF,
        new Uint8Array(HELD),
        HELD_EPOCH,
      ),
    ).toBe("accept-identical");
    expect(unsubscribeGroup).not.toHaveBeenCalled();
    expect(vi.mocked(deleteRoute)).not.toHaveBeenCalled();
  });

  it("a FIRST-EVER secret retires nothing (there is no old id)", async () => {
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, ADMIN_LEAF, ROTATED, ROTATED_EPOCH),
    ).toBe("accept-admin-seed");
    expect(unsubscribeGroup).not.toHaveBeenCalled();
    expect(vi.mocked(deleteRoute)).not.toHaveBeenCalled();
  });

  it("a REFUSED secret retires nothing (the device keeps its id)", async () => {
    seedStoredSecret(ACCT, GRP, HELD, HELD_EPOCH);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, MEMBER_LEAF, ROTATED, ROTATED_EPOCH),
    ).toBe("reject-not-authorized");
    expect(unsubscribeGroup).not.toHaveBeenCalled();
    expect(vi.mocked(deleteRoute)).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// ADR-0044 — mint-epoch recency ordering (G-36 residual 9 / lakko-security F-A)
// ---------------------------------------------------------------------------

describe("routingSecretVerdict — the ADR-0044 ordering rules", () => {
  // 32-byte fixtures chosen so the STALE secret sorts STRICTLY HIGHER than
  // the rotation that replaced it. A gate that compared only bytes, or that
  // tie-broke before consulting the epoch, would accept the stale one — so
  // these fixtures are what make the epoch's primacy observable.
  const STALE = new Uint8Array(32).fill(0xee);
  const LIVE = new Uint8Array(32).fill(0x11);

  const base = {
    ownerKey: OWNER_PK,
    adminKeys: [ADMIN_PK],
    senderKey: OWNER_PK,
    storedUnreadable: false,
    verifyFailed: false,
    ownEpoch: GROUP_EPOCH,
    isNewer: wasm.pushRoutingSecretIsNewer,
  };

  it("REJECTS a stamp ahead of this device's own epoch (reject-future)", () => {
    expect(
      routingSecretVerdict({
        ...base,
        storedSecret: LIVE,
        storedMintEpoch: 10n,
        incomingSecret: STALE,
        incomingMintEpoch: GROUP_EPOCH + 1n,
      }),
    ).toBe("reject-future");
  });

  it("the future clamp is evaluated BEFORE authority — even the OWNER is refused", () => {
    // Without this precedence an authorised writer could stamp u64::MAX and
    // FREEZE the group's routing id: nothing could ever exceed it, so no
    // later owner rotation could win. That trades G-36 residual 3's
    // recoverable churn for an unrecoverable denial.
    for (const senderKey of [OWNER_PK, ADMIN_PK]) {
      expect(
        routingSecretVerdict({
          ...base,
          senderKey,
          storedSecret: LIVE,
          storedMintEpoch: 10n,
          incomingSecret: STALE,
          incomingMintEpoch: 2n ** 64n - 1n,
        }),
      ).toBe("reject-future");
    }
  });

  it("accepts a stamp EQUAL to this device's own epoch (the clamp is not off-by-one)", () => {
    // `stamp <= seal <= receiver's epoch at dispatch`, so equality is the
    // ordinary case for a freshly minted rotation applied by a peer that has
    // processed the same commit. Refusing it would break every rotation.
    expect(
      routingSecretVerdict({
        ...base,
        storedSecret: LIVE,
        storedMintEpoch: 10n,
        incomingSecret: STALE,
        incomingMintEpoch: GROUP_EPOCH,
      }),
    ).toBe("accept-owner");
  });

  it("REJECTS an AUTHORISED envelope that is older than what is held (reject-stale)", () => {
    // THE finding. The sender clears authority — it is the owner — and the
    // stale secret even sorts higher bytewise. Only the epoch refuses it.
    expect(
      routingSecretVerdict({
        ...base,
        storedSecret: LIVE,
        storedMintEpoch: 11n,
        incomingSecret: STALE,
        incomingMintEpoch: 10n,
      }),
    ).toBe("reject-stale");
  });

  it("an ADMIN's stale re-broadcast is refused the same way", () => {
    // The reachable shape: ADR-0043 option (b) lets an admin rotate, so an
    // admin relaying a superseded secret clears authority as `accept-admin-
    // rotate` and would previously have been applied.
    expect(
      routingSecretVerdict({
        ...base,
        senderKey: ADMIN_PK,
        storedSecret: LIVE,
        storedMintEpoch: 11n,
        incomingSecret: STALE,
        incomingMintEpoch: 10n,
      }),
    ).toBe("reject-stale");
  });

  it("a legacy no-role-table group is ordered too (accept-no-owner is not exempt)", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownerKey: undefined,
        adminKeys: [],
        senderKey: MEMBER_PK,
        storedSecret: LIVE,
        storedMintEpoch: 11n,
        incomingSecret: STALE,
        incomingMintEpoch: 10n,
      }),
    ).toBe("reject-stale");
  });

  it("accepts a strictly NEWER envelope even when its secret sorts LOWER", () => {
    // The epoch is the primary key: `LIVE` < `STALE` bytewise, and it still
    // wins on a greater epoch.
    expect(
      routingSecretVerdict({
        ...base,
        storedSecret: STALE,
        storedMintEpoch: 10n,
        incomingSecret: LIVE,
        incomingMintEpoch: 11n,
      }),
    ).toBe("accept-owner");
  });

  it("at an EXACT epoch tie the bytes decide — and the owner can LOSE", () => {
    // The documented semantic narrowing. Under ADR-0043 option (b) one
    // removal produces N+1 same-epoch mints, so this is the common path, not
    // a corner case. Asserted so it cannot be "fixed" by accident.
    const ownerMint = new Uint8Array(32).fill(0x10);
    const adminMint = new Uint8Array(32).fill(0x90);
    // The admin's higher-sorting mint is already stored; the owner's
    // same-epoch envelope arrives and is refused.
    expect(
      routingSecretVerdict({
        ...base,
        senderKey: OWNER_PK,
        storedSecret: adminMint,
        storedMintEpoch: 42n,
        incomingSecret: ownerMint,
        incomingMintEpoch: 42n,
      }),
    ).toBe("reject-stale");
    // ... and the converse converges on the same winner, whichever order the
    // two envelopes arrive in. That arrival-order independence is what
    // owner-priority used to provide for the owner alone.
    expect(
      routingSecretVerdict({
        ...base,
        senderKey: ADMIN_PK,
        storedSecret: ownerMint,
        storedMintEpoch: 42n,
        incomingSecret: adminMint,
        incomingMintEpoch: 42n,
      }),
    ).toBe("accept-admin-rotate");
  });

  it("a SEED (no stored secret) is never ordered — any stamp is accepted", () => {
    // Nothing to compare against; refusing here would leave a member that
    // holds none deferring push forever.
    expect(
      routingSecretVerdict({
        ...base,
        senderKey: ADMIN_PK,
        storedSecret: null,
        storedMintEpoch: 0n,
        incomingSecret: STALE,
        incomingMintEpoch: 0n,
      }),
    ).toBe("accept-admin-seed");
  });

  it("an unreadable own epoch fails CLOSED (no ceiling, no accept)", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownEpoch: undefined,
        storedSecret: LIVE,
        storedMintEpoch: 10n,
        incomingSecret: STALE,
        incomingMintEpoch: 11n,
      }),
    ).toBe("reject-unverifiable");
  });

  // -- the accept-identical fast path must bypass BOTH new rules -----------
  //
  // This is what preserves retry semantics. The any-holder re-broadcast, the
  // post-add redistribute and any relay all re-emit the identical pair, so
  // they must never be read as an update NOR as a stale replay.

  it("accept-identical short-circuits the future clamp AND the order", () => {
    for (const incomingMintEpoch of [0n, 10n, GROUP_EPOCH + 999n]) {
      expect(
        routingSecretVerdict({
          ...base,
          senderKey: MEMBER_PK,
          storedSecret: LIVE,
          storedMintEpoch: 10n,
          incomingSecret: new Uint8Array(LIVE),
          incomingMintEpoch,
        }),
      ).toBe("accept-identical");
    }
  });

  it("accept-identical still wins with an unreadable own epoch (a torn group)", () => {
    expect(
      routingSecretVerdict({
        ...base,
        ownEpoch: undefined,
        verifyFailed: true,
        storedSecret: LIVE,
        storedMintEpoch: 10n,
        incomingSecret: new Uint8Array(LIVE),
        incomingMintEpoch: 10n,
      }),
    ).toBe("accept-identical");
  });

  it("a wrong-length incoming secret can never displace a stored one", () => {
    // The core's fail-closed rule, surfaced through the gate: a malformed
    // value is not "newer", whatever epoch it claims.
    expect(
      routingSecretVerdict({
        ...base,
        storedSecret: LIVE,
        storedMintEpoch: 10n,
        incomingSecret: new Uint8Array(31).fill(0xff),
        incomingMintEpoch: 99n,
      }),
    ).toBe("reject-stale");
  });

  it("verdictAccepts partitions the two NEW verdicts as rejections", () => {
    expect(verdictAccepts("reject-future")).toBe(false);
    expect(verdictAccepts("reject-stale")).toBe(false);
  });
});

describe("the F-A replay, end to end (G-36 residual 9)", () => {
  const ACCT = "acctFA";
  const GRP = "grpFAB64";
  const GROUP_ID = new Uint8Array([0xfa, 0xfa, 0xfa, 0xfa]);
  // Again: the STALE secret sorts HIGHER, so nothing but the epoch saves us.
  const S_OLD = new Uint8Array(32).fill(0xee);
  const S1 = new Uint8Array(32).fill(0x11);

  it("a stale re-broadcast is refused and does NOT retire the live gid", async () => {
    // The scenario, exactly as `lakko-security` F-A describes it:
    //
    //   1. the group is on S_OLD, minted at epoch 10;
    //   2. an admin removes a member and rotates to S1 at epoch 11 — this
    //      device applies it, retiring gid(S_OLD);
    //   3. a device whose earlier broadcast FAILED still holds S_OLD in a
    //      pending row and re-broadcasts it. It is relayed by an authorised
    //      sender, so the authority gate alone would accept it.
    //
    // Before ADR-0044 step 3 was applied by every member, retiring gid(S1)
    // and migrating the whole group BACK onto the routing id the evicted
    // member still derives — permanently, since no further shrink occurs.
    seedStoredSecret(ACCT, GRP, S_OLD, 10n);
    h.routes.set(toHex(wasm.derivePushGid(S_OLD)), {
      accountId: ACCT,
      groupIdB64: GRP,
    });
    const group = mockRoleGroup(GROUP_ID, OWNER_PK, { epoch: 11n });

    // (2) the rotation lands.
    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, ADMIN_LEAF, S1, 11n),
    ).toBe("accept-admin-rotate");
    const gidOld = toHex(wasm.derivePushGid(S_OLD));
    const gidLive = toHex(wasm.derivePushGid(S1));
    await vi.waitFor(() => expect(unsubscribeGroup).toHaveBeenCalledTimes(1));
    expect(toHex(unsubscribeGroup.mock.calls[0]![1])).toBe(gidOld);
    expect(h.routes.has(gidLive)).toBe(true);

    // (3) the stale re-broadcast — from the OWNER, the highest authority
    // there is, so this cannot be refused on authorship.
    expect(
      await receivePushRoutingSecretGated(
        ACCT,
        group,
        GRP,
        OWNER_LEAF,
        S_OLD,
        10n,
      ),
    ).toBe("reject-stale");

    // Nothing moved: the store still holds S1 ...
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(S1),
    );
    // ... gid(S1) is STILL the live route, not retired ...
    expect(h.routes.has(gidLive)).toBe(true);
    // ... no second teardown fired (the retire is what made F-A permanent) ...
    expect(unsubscribeGroup).toHaveBeenCalledTimes(1);
    expect(vi.mocked(deleteRoute)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(deleteRoute)).not.toHaveBeenCalledWith(gidLive);
    // ... and nothing was persisted for the refused envelope.
    expect(vi.mocked(putGroupRouting)).toHaveBeenCalledTimes(1);
  });

  it("the same stale replay from an ADMIN is refused too", async () => {
    seedStoredSecret(ACCT, GRP, S1, 11n);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK, { epoch: 11n });

    expect(
      await receivePushRoutingSecretGated(
        ACCT,
        group,
        GRP,
        ADMIN_LEAF,
        S_OLD,
        10n,
      ),
    ).toBe("reject-stale");
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    expect(unsubscribeGroup).not.toHaveBeenCalled();
  });

  it("a LEGACY stored row (no recorded epoch) reads as 0n and is superseded", async () => {
    // A row written before ADR-0044 carries no stamp. Reading it as 0n is the
    // direction that converges: an authorised stamped mint supersedes it,
    // rather than an unstamped legacy secret out-ranking every rotation.
    seedStoredSecret(ACCT, GRP, S_OLD); // NO epoch recorded
    expect(h.mintEpochs.has(`${ACCT}\0${GRP}`)).toBe(false);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, OWNER_LEAF, S1, 1n),
    ).toBe("accept-owner");
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(S1),
    );
    expect(h.mintEpochs.get(`${ACCT}\0${GRP}`)).toBe(1n);
  });

  it("an identical re-broadcast of a legacy secret is still idempotent", async () => {
    // The upgrade-day case: a peer still on the legacy row re-emits the same
    // bytes. `accept-identical` matches on the SECRET, before any epoch is
    // consulted, so a stamp mismatch cannot turn a benign retry into churn.
    seedStoredSecret(ACCT, GRP, S_OLD, 10n);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(
        ACCT,
        group,
        GRP,
        MEMBER_LEAF,
        new Uint8Array(S_OLD),
        0n,
      ),
    ).toBe("accept-identical");
    expect(unsubscribeGroup).not.toHaveBeenCalled();
    // The stored epoch is left UNTOUCHED — see the inflation test below.
    expect(h.mintEpochs.get(`${ACCT}\0${GRP}`)).toBe(10n);
  });

  it("accept-identical cannot INFLATE the stored epoch (a plain member freeze)", async () => {
    // `accept-identical` clears NEITHER the authority gate nor the future
    // clamp — it short-circuits both by design, so a retry stays idempotent
    // for any sender. That makes the re-persist an epoch-inflation channel if
    // it writes the INCOMING stamp: a plain member relays the group's current
    // secret bytes stamped u64::MAX, the stored epoch jumps to the ceiling,
    // and every later rotation — the OWNER's included — is refused
    // `reject-stale` forever. That is precisely the permanent freeze the
    // clamp exists to prevent, reached around it. So the stored epoch must
    // survive the write.
    seedStoredSecret(ACCT, GRP, S_OLD, 10n);
    const group = mockRoleGroup(GROUP_ID, OWNER_PK);

    expect(
      await receivePushRoutingSecretGated(
        ACCT,
        group,
        GRP,
        MEMBER_LEAF,
        new Uint8Array(S_OLD),
        2n ** 64n - 1n,
      ),
    ).toBe("accept-identical");
    expect(h.mintEpochs.get(`${ACCT}\0${GRP}`)).toBe(10n);

    // ... and a legitimate owner rotation at a modest epoch still lands.
    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, OWNER_LEAF, S1, 11n),
    ).toBe("accept-owner");
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP}`) as Uint8Array)).toEqual(
      Array.from(S1),
    );
    expect(h.mintEpochs.get(`${ACCT}\0${GRP}`)).toBe(11n);
  });

  it("an accepted DIFFERENT secret clears a stale PENDING re-broadcast", async () => {
    // ADR-0044's complementary fix. The order rule stops a stale value from
    // SPREADING; this stops the device still holding one from re-emitting it.
    // On web both fall out of the same atomic row write: applying an accepted
    // secret replaces the bytes AND the pending flag in one
    // `putGroupRouting`, so after any accept there is nothing stale left to
    // re-broadcast. Pinned here rather than left incidental.
    seedStoredSecret(ACCT, GRP, S_OLD, 10n);
    h.distributed.set(`${ACCT}\0${GRP}`, false); // its broadcast failed
    const group = mockRoleGroup(GROUP_ID, OWNER_PK, { epoch: 11n });

    expect(
      await receivePushRoutingSecretGated(ACCT, group, GRP, OWNER_LEAF, S1, 11n),
    ).toBe("accept-owner");

    // The pending flag is gone ...
    expect(h.distributed.get(`${ACCT}\0${GRP}`)).toBe(true);
    // ... so the next provisioning pass re-broadcasts NOTHING.
    const sent: Uint8Array[] = [];
    await ensureGroupPushRouting(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Member, 11n),
      DUMMY_IDENTITY,
      async (b) => {
        sent.push(b);
      },
    );
    expect(sent.length).toBe(0);
  });
});

describe("the mint epoch is stamped once and REPLAYED verbatim (ADR-0044)", () => {
  const ACCT = "acctStamp";
  const GROUP_ID = new Uint8Array([0x5a, 0x5a]);
  const GRP_B64 = base64Encode(GROUP_ID);

  it("the OWNER's MINT stamps the group's CURRENT epoch", async () => {
    const sent: Uint8Array[] = [];
    await ensureGroupPushRouting(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Owner, 33n),
      DUMMY_IDENTITY,
      async (b) => {
        sent.push(b);
      },
    );

    expect(sent.length).toBe(1);
    expect(decodeMintEpoch(sent[0]!)).toBe(33n);
    // ... and the same value is persisted, so a later re-broadcast can
    // replay it.
    expect(vi.mocked(putGroupRouting).mock.calls[0]![0].mintEpoch).toBe(33n);
  });

  it("a MINT is REFUSED when the group epoch cannot be read (fail-closed)", async () => {
    // An unstamped secret has no position in the total order, which is the
    // one thing v2 exists to eliminate — so no secret at all beats an
    // unordered one.
    const sent: Uint8Array[] = [];
    await ensureGroupPushRouting(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Owner, null),
      DUMMY_IDENTITY,
      async (b) => {
        sent.push(b);
      },
    );

    expect(sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });

  it("the PENDING re-broadcast replays the STORED epoch, not the current one", async () => {
    // THE site the F-A replay travels through. The group has advanced to
    // epoch 100 since this secret was minted at 5; re-stamping 100 here would
    // hand the stale secret a newer position than the rotation that
    // superseded it and reproduce the finding with the gate fully armed.
    seedStoredSecret(ACCT, GRP_B64, new Uint8Array(32).fill(0x42), 5n);
    h.distributed.set(`${ACCT}\0${GRP_B64}`, false);
    const sent: Uint8Array[] = [];

    await ensureGroupPushRouting(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Member, 100n),
      DUMMY_IDENTITY,
      async (b) => {
        sent.push(b);
      },
    );

    expect(sent.length).toBe(1);
    expect(decodeMintEpoch(sent[0]!)).toBe(5n);
    expect(decodeMintEpoch(sent[0]!)).not.toBe(100n);
  });

  it("the post-add REDISTRIBUTE replays the STORED epoch too", async () => {
    // The secret did not change, only its audience did. Re-stamping would let
    // a routine add-member out-rank a rotation minted at a higher epoch.
    seedStoredSecret(ACCT, GRP_B64, new Uint8Array(32).fill(0x43), 5n);
    const sent: Uint8Array[] = [];

    await redistributeSecretToGroup(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Owner, 100n),
      async (b) => {
        sent.push(b);
      },
    );

    expect(sent.length).toBe(1);
    expect(decodeMintEpoch(sent[0]!)).toBe(5n);
  });

  it("a LEGACY stored row re-broadcasts at 0n rather than inventing a stamp", async () => {
    seedStoredSecret(ACCT, GRP_B64, new Uint8Array(32).fill(0x44)); // no epoch
    h.distributed.set(`${ACCT}\0${GRP_B64}`, false);
    const sent: Uint8Array[] = [];

    await ensureGroupPushRouting(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Member, 100n),
      DUMMY_IDENTITY,
      async (b) => {
        sent.push(b);
      },
    );

    expect(sent.length).toBe(1);
    expect(decodeMintEpoch(sent[0]!)).toBe(0n);
  });

  it("a ROTATION stamps the current epoch, and its RETRY replays that stamp", async () => {
    seedStoredSecret(ACCT, GRP_B64, new Uint8Array(32).fill(0x45), 5n);
    h.distributed.set(`${ACCT}\0${GRP_B64}`, true);
    const sent: Uint8Array[] = [];
    let failNext = true;
    const broadcast = async (b: Uint8Array): Promise<void> => {
      sent.push(b);
      if (failNext) {
        failNext = false;
        throw new Error("DS unreachable");
      }
    };

    // Rotate at epoch 11; the broadcast fails, leaving the row pending.
    expect(
      await rotatePushRoutingSecret(
        ACCT,
        mockGroup(GROUP_ID, undefined, 11n),
        "owner",
        broadcast,
      ),
    ).toBe("rotated");
    expect(decodeMintEpoch(sent[0]!)).toBe(11n);

    // The retry fires much later, at epoch 50 — and must STILL carry 11.
    await ensureGroupPushRouting(
      ACCT,
      mockGroup(GROUP_ID, wasm.Role.Owner, 50n),
      DUMMY_IDENTITY,
      broadcast,
    );
    expect(sent.length).toBe(2);
    expect(decodeMintEpoch(sent[1]!)).toBe(11n);
    expect(Array.from(decodeSecret(sent[1]!))).toEqual(
      Array.from(decodeSecret(sent[0]!)),
    );
  });

  it("a ROTATION is abandoned when the epoch cannot be read (fail-closed)", async () => {
    const held = new Uint8Array(32).fill(0x46);
    seedStoredSecret(ACCT, GRP_B64, held, 5n);
    const sent: Uint8Array[] = [];

    expect(
      await rotatePushRoutingSecret(
        ACCT,
        mockGroup(GROUP_ID, undefined, null),
        "owner",
        async (b) => {
          sent.push(b);
        },
      ),
    ).toBe("skip-unknown-epoch");
    expect(sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
    // The old secret survives, so a later trigger can still rotate.
    expect(Array.from(h.secrets.get(`${ACCT}\0${GRP_B64}`) as Uint8Array))
      .toEqual(Array.from(held));
    expect(unsubscribeGroup).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// C-61 — the rotation broadcast STRICTLY FOLLOWS the Remove commit
// ---------------------------------------------------------------------------

describe("C-61 — publish the Remove commit, THEN rotate", () => {
  const ACCT = "acctC61";
  const GROUP_ID = new Uint8Array([0xc6, 0x10]);
  const GRP_B64 = base64Encode(GROUP_ID);
  /** The secret the evicted member is holding, minted pre-removal. */
  const S_OLD = new Uint8Array(32).fill(0xee);
  const PRE_REMOVAL_EPOCH = 10n;
  const POST_REMOVAL_EPOCH = 11n;

  /** A group whose epoch the test advances, exactly as applying a Remove
   *  commit locally does. */
  function epochGroup(): { group: WasmGroup; advance: () => void } {
    let epoch = PRE_REMOVAL_EPOCH;
    return {
      group: {
        get groupId(): Uint8Array {
          return GROUP_ID;
        },
        get epoch(): bigint {
          return epoch;
        },
        ownRole: (): number | undefined => undefined,
      } as unknown as WasmGroup,
      advance: (): void => {
        epoch = POST_REMOVAL_EPOCH;
      },
    };
  }

  function seedSettled(): void {
    seedStoredSecret(ACCT, GRP_B64, S_OLD, PRE_REMOVAL_EPOCH);
    h.distributed.set(`${ACCT}\0${GRP_B64}`, true);
  }

  it("stamps the rotation at the POST-removal epoch, so it out-ranks the secret it replaces", async () => {
    // How this fails under a SWAP: the assertions below read the epoch the
    // emitted envelope was STAMPED with. `rotatePushRoutingSecret` reads
    // `group.epoch` at mint time, and `advance()` — the local application of
    // the Remove commit — is what moves that value from 10 to 11. Start the
    // rotation before the removal and the stamp is 10, so
    // `decodeMintEpoch(...) === POST_REMOVAL_EPOCH` fails outright, and the
    // `isNewer` assertion below flips from "the rotation always wins" to a
    // same-epoch tie decided by the 32 secret bytes. The companion test after
    // this one runs exactly that swap and records both consequences.
    seedSettled();
    const { group, advance } = epochGroup();
    const sent: Uint8Array[] = [];
    let rotation!: Promise<Awaited<ReturnType<typeof rotatePushRoutingSecret>>>;

    const commitId = await publishRemoveThenRotate({
      publishRemove: async () => {
        // Applying + publishing the Remove commit advances the local epoch.
        advance();
        return 4242;
      },
      rotate: () => {
        rotation = rotatePushRoutingSecret(ACCT, group, "owner", async (b) => {
          sent.push(b);
        });
        return rotation;
      },
    });
    expect(commitId).toBe(4242);
    expect(await rotation).toBe("rotated");

    // The rotation was sealed AND stamped after the removal.
    expect(sent.length).toBe(1);
    expect(decodeMintEpoch(sent[0]!)).toBe(POST_REMOVAL_EPOCH);

    // ... which is what makes the ordering a GUARANTEE rather than a coin
    // flip: the evicted member's secret sorts HIGHER bytewise, so only the
    // strictly greater epoch puts the rotation on top.
    const rotated = decodeSecret(sent[0]!);
    expect(
      wasm.pushRoutingSecretIsNewer(
        POST_REMOVAL_EPOCH,
        rotated,
        PRE_REMOVAL_EPOCH,
        S_OLD,
      ),
    ).toBe(true);
    expect(
      wasm.pushRoutingSecretIsNewer(
        PRE_REMOVAL_EPOCH,
        S_OLD,
        POST_REMOVAL_EPOCH,
        rotated,
      ),
    ).toBe(false);
  });

  it("the INVERTED order would stamp the pre-removal epoch and lose the guarantee", async () => {
    // Not a test of production code — a demonstration that the assertion
    // above actually discriminates. Rotating BEFORE the Remove commit lands
    // stamps epoch 10, tying with the secret the evicted member holds, so the
    // outcome falls to the byte comparison. With these fixtures the stale
    // secret sorts higher and the rotation LOSES outright.
    seedSettled();
    const { group, advance } = epochGroup();
    const sent: Uint8Array[] = [];

    await rotatePushRoutingSecret(ACCT, group, "owner", async (b) => {
      sent.push(b);
    });
    advance(); // the Remove commit lands afterwards — too late

    expect(decodeMintEpoch(sent[0]!)).toBe(PRE_REMOVAL_EPOCH);
    const rotated = decodeSecret(sent[0]!);
    // Tied on epoch, so the bytes decide — and here the stale secret wins.
    expect(
      wasm.pushRoutingSecretIsNewer(
        PRE_REMOVAL_EPOCH,
        rotated,
        PRE_REMOVAL_EPOCH,
        S_OLD,
      ),
    ).toBe(false);
    expect(
      wasm.pushRoutingSecretIsNewer(
        PRE_REMOVAL_EPOCH,
        S_OLD,
        PRE_REMOVAL_EPOCH,
        rotated,
      ),
    ).toBe(true);
  });

  it("the rotation is not STARTED until the removal resolves", async () => {
    // The ordering is about start time, not completion: the rotation stays
    // fire-and-forget so a push failure never unwinds a membership change.
    seedSettled();
    const { group, advance } = epochGroup();
    const order: string[] = [];
    let releaseRemove!: () => void;
    const removeGate = new Promise<void>((r) => {
      releaseRemove = r;
    });
    let rotation!: Promise<Awaited<ReturnType<typeof rotatePushRoutingSecret>>>;

    const pending = publishRemoveThenRotate({
      publishRemove: async () => {
        order.push("remove-started");
        await removeGate;
        advance();
        order.push("remove-published");
        return 1;
      },
      rotate: () => {
        order.push("rotate-started");
        rotation = rotatePushRoutingSecret(ACCT, group, "owner", async (b) => {
          order.push(`rotate-broadcast@${decodeMintEpoch(b)}`);
        });
        return rotation;
      },
    });

    await flushMicrotasks();
    // The removal is parked; nothing about the rotation has happened.
    expect(order).toEqual(["remove-started"]);

    releaseRemove();
    await pending;
    await rotation;

    expect(order).toEqual([
      "remove-started",
      "remove-published",
      "rotate-started",
      `rotate-broadcast@${POST_REMOVAL_EPOCH}`,
    ]);
  });

  it("a FAILED removal never rotates", async () => {
    // A rotation after a removal that did not land would re-key the group for
    // nothing and, worse, seal at the pre-removal epoch.
    seedSettled();
    const { group } = epochGroup();
    const sent: Uint8Array[] = [];

    await expect(
      publishRemoveThenRotate({
        publishRemove: async () => {
          throw new Error("commit rejected");
        },
        rotate: () =>
          rotatePushRoutingSecret(ACCT, group, "owner", async (b) => {
            sent.push(b);
          }),
      }),
    ).rejects.toThrow("commit rejected");

    await flushMicrotasks();
    expect(sent.length).toBe(0);
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });

  it("a rotation failure is reported, not thrown at the removal's caller", async () => {
    seedSettled();
    const { group, advance } = epochGroup();
    const errors: unknown[] = [];

    const commitId = await publishRemoveThenRotate({
      publishRemove: async () => {
        advance();
        return 7;
      },
      rotate: () => Promise.reject(new Error("rotation blew up")),
      onRotationError: (err) => errors.push(err),
    });

    expect(commitId).toBe(7);
    await flushMicrotasks();
    expect(errors).toHaveLength(1);
    expect((errors[0] as Error).message).toBe("rotation blew up");
    // Nothing about the group's own epoch was disturbed.
    expect(group.epoch).toBe(POST_REMOVAL_EPOCH);
  });

  it("no session means no rotation, and the removal still lands", async () => {
    seedSettled();
    expect(
      await publishRemoveThenRotate({
        publishRemove: async () => 9,
      }),
    ).toBe(9);
    await flushMicrotasks();
    expect(vi.mocked(putGroupRouting)).not.toHaveBeenCalled();
  });
});

// base64 of the group_id — recomputed the same way `base64Encode` (real,
// used by the orchestration) does, to assert the reverse-map entry.
function base64OfGroupId(groupId: Uint8Array): string {
  return base64Encode(groupId);
}
