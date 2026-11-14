// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Follow-up decision 1 (2026-11-13): the account's subscriber key from the
 * channel-subscription register is a real reader key. The publishing device
 * registers it once (a private channel with its own one-use, never-expiring
 * invite key and a sealed ticket), and every rotation it makes reseals the
 * new seed to it, above the admission mark too. The channels service is a
 * mock; the sealing is the REAL core.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const proofs = vi.hoisted((): string[] => []);
const api = vi.hoisted(() => ({
  putInviteKey: vi.fn(async () => undefined),
  subscribeWithAccess: vi.fn(
    async (
      _id: string,
      _pk: string,
      access: { inviteProof?: () => { headerName: string; value: string } },
    ): Promise<void> => {
      // The proof is signed while the request is made, as the service gets it.
      proofs.push(access.inviteProof?.().value ?? "");
    },
  ),
  listSubscriberPage: vi.fn(),
  stageRotation: vi.fn(async () => ({})),
  commitRotation: vi.fn(async (_id: string, epoch: number) => ({ epoch })),
  putPublicSeed: vi.fn(async () => undefined),
}));
vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();
  return { ...actual, ...api };
});
const admission = vi.hoisted(() => ({ admitSubscriber: vi.fn(async () => undefined) }));
vi.mock("./subscriber-admission", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./subscriber-admission")>();
  return { ...actual, admitSubscriber: admission.admitSubscriber };
});
vi.mock("../wasm", async () => {
  const m = await import("../../wasm/lakko_ffi_wasm");
  return { loadWasm: async () => m, wasmSync: () => m };
});

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { accountReaderKeyHex, ensureAccountReaderKey, readerKeyStep } from "./account-reader-key";
import {
  installChannelSubscriptionRegister,
  type ChannelSubscriptionView,
} from "./channel-subscription-register";
import { resealToSubscribers, withOwnReaderKey } from "./rotation-cron";
import { channelsActions } from "../state/channels-state";
import type { WasmIdentity } from "./composer";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const ID = "5a".repeat(16);
const KEYS = { seed: new Uint8Array(32).fill(1), signingKeySeed: new Uint8Array(32).fill(2), seedEpoch: 0 };
const identity = {} as WasmIdentity;

function keyPair(): { secret: Uint8Array; hex: string } {
  const kp = wasm.ChannelSubscriberKeypair.generate();
  try {
    const pub = kp.public();
    return { secret: kp.secret(), hex: [...pub].map((b) => b.toString(16).padStart(2, "0")).join("") };
  } finally {
    kp.free();
  }
}

function b64(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes));
}

let uninstall: (() => void) | null = null;
let account: { secret: Uint8Array; hex: string } = { secret: new Uint8Array(32), hex: "" };

beforeEach(() => {
  for (const f of Object.values(api)) f.mockClear();
  admission.admitSubscriber.mockClear();
  account = keyPair();
  const reg: ChannelSubscriptionView = {
    channelIdHex: ID,
    hlc: 1n,
    subscribed: true,
    role: "owner",
    subscriberSecret: account.secret,
    readKey: new Uint8Array(0),
    publisherGroupIdHex: "",
    ownerAccountKey: new Uint8Array(0),
    name: "",
    handle: "",
  };
  uninstall = installChannelSubscriptionRegister({
    accountIdB64: "YWNjb3VudA==",
    all: () => [reg],
    get: (id) => (id === ID ? reg : undefined),
    set: async () => false,
    leave: async () => false,
  });
  channelsActions.addChannel({ id: ID, localName: "C", localDescription: "", addedAtMs: 1 });
  channelsActions.setKeys(ID, KEYS);
});

afterEach(() => {
  uninstall?.();
  channelsActions.removeChannel(ID);
});

describe("the account's reader key on the publishing device", () => {
  it("is registered once on a private channel, with its own one-use invite key and a sealed ticket", async () => {
    channelsActions.updateAccess(ID, (r) => ({
      ...r,
      visibility: "private",
      admissionSeeds: [{ version: 3, seedB64: b64(wasm.generateChannelAdmissionSeed()) }],
    }));
    expect(await accountReaderKeyHex(ID)).toBe(account.hex);
    await ensureAccountReaderKey(identity, ID);
    expect(api.putInviteKey).toHaveBeenCalledTimes(1);
    // Never expiring, one use: the row's ticket names it for good.
    expect(api.putInviteKey.mock.calls[0]?.slice(0, 4)).toEqual([ID, expect.stringMatching(/^[0-9a-f]{64}$/), 0, 1]);
    expect(api.subscribeWithAccess).toHaveBeenCalledTimes(1);
    const [, pubHex, access] = api.subscribeWithAccess.mock.calls[0] as unknown as [
      string,
      string,
      { sealedTicketB64Url?: string; inviteProof?: () => { headerName: string; value: string } },
    ];
    expect(pubHex).toBe(account.hex);
    expect(access.sealedTicketB64Url).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(proofs.at(-1)?.length ?? 0).toBeGreaterThan(0);
    expect(channelsActions.accessOf(ID).readerKeyRegisteredHex).toBe(account.hex);
    expect(admission.admitSubscriber).toHaveBeenCalledWith(ID, account.hex, identity);

    // Once: a later pass registers nothing.
    await ensureAccountReaderKey(identity, ID);
    expect(api.subscribeWithAccess).toHaveBeenCalledTimes(1);
    expect(api.putInviteKey).toHaveBeenCalledTimes(1);
  });

  it("is registered on a public channel without an invite", async () => {
    channelsActions.updateAccess(ID, (r) => ({ ...r, visibility: "public" }));
    await ensureAccountReaderKey(identity, ID);
    expect(api.putInviteKey).not.toHaveBeenCalled();
    expect(api.subscribeWithAccess).toHaveBeenCalledWith(ID, account.hex, {});
  });

  it("only the publishing device registers, and a failure waits before the next try", () => {
    const base = { keyHex: "aa", registeredHex: null, lastFailureMs: undefined, nowMs: 1_000_000 };
    expect(readerKeyStep({ ...base, publishing: true })).toBe("register");
    expect(readerKeyStep({ ...base, publishing: false })).toBe("nothing");
    expect(readerKeyStep({ ...base, publishing: true, registeredHex: "aa" })).toBe("nothing");
    expect(readerKeyStep({ ...base, publishing: true, registeredHex: "bb" })).toBe("register");
    expect(readerKeyStep({ ...base, publishing: true, lastFailureMs: 1_000_000 - 1000 })).toBe("nothing");
    expect(readerKeyStep({ ...base, publishing: true, lastFailureMs: 1_000_000 - 11 * 60 * 1000 })).toBe("register");
  });

  it("a private channel's rotation reseals to it even above the admission mark", async () => {
    const below = keyPair();
    const above = keyPair();
    channelsActions.updateAccess(ID, (r) => ({
      ...r,
      visibility: "private",
      admittedThroughSeq: 1,
      admissionSeeds: [{ version: 1, seedB64: b64(new Uint8Array(32).fill(4)) }],
      readerKeyRegisteredHex: account.hex,
    }));
    api.listSubscriberPage.mockResolvedValue({
      entries: [
        { seq: 1, subscriberId: below.hex, sealedTicket: null },
        { seq: 2, subscriberId: above.hex, sealedTicket: null },
        { seq: 3, subscriberId: account.hex, sealedTicket: null },
      ],
      nextAfter: 3,
    });
    const group = {
      channelSeed: () => new Uint8Array(32).fill(8),
      channelId: () => new Uint8Array(16).fill(0x5a),
    } as unknown as Parameters<typeof resealToSubscribers>[0];
    await resealToSubscribers(
      group,
      {} as Parameters<typeof resealToSubscribers>[1],
      {} as Parameters<typeof resealToSubscribers>[2],
      ID,
      7,
    );
    const staged = api.stageRotation.mock.calls.flatMap(
      (c) => (c as unknown as [string, number, { subscriber_id: string }[]])[2],
    );
    expect(staged.map((e) => e.subscriber_id)).toEqual([below.hex, account.hex]);
    expect(api.commitRotation).toHaveBeenCalledTimes(1);
  });

  it("the reseal set takes the account's key once", () => {
    expect(withOwnReaderKey(["aa"], "bb")).toEqual(["aa", "bb"]);
    expect(withOwnReaderKey(["aa", "BB"], "bb")).toEqual(["aa", "BB"]);
    expect(withOwnReaderKey([], null)).toEqual([]);
  });
});
