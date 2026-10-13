// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 / ADR-0074 section 3.5 - every path that adds an account's
 * devices to a group goes through ONE filter (`add-member-key.ts`, applied
 * by `ContactsClient.devicesOfAccount`): the user-facing add, a new chat and
 * a successor build (`prepareGroupWith`), the ADR-0050 re-admission arm
 * (`admitDeviceToGroup`) and the membership re-request
 * (`readmitDeviceByKey`).
 *
 * The wasm core is mocked: the first byte of a fake KeyPackage is the fill
 * of the account key its binding names. The pin store is the real one over
 * an in-memory `localStorage`.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fakeAccessSigner } from "../ds/access-test-doubles";

vi.mock("../chat/commit-outbox", () => ({
  markAcked: async () => {},
  loadUnacked: async () => [],
}));

class Sentinel extends Error {}

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    keyPackageSignatureKey: (kp: Uint8Array) => new Uint8Array(32).fill(kp[1] ?? 0),
    keyPackageAccountKey: (kp: Uint8Array) => new Uint8Array(32).fill(kp[0] ?? 0),
    GroupKind: { OneToOne: 0, Group: 1 },
    // Reaching the constructor means the filter let a device through.
    Group: class {
      constructor() {
        throw new Sentinel("group built");
      }
    },
  }),
}));

import { ContactsClient } from "./client";
import { acceptReset, bindContactsAccount, setPin } from "./local-store";
import { recordVerifiedBinding } from "./verified-binding";
import { base64Encode } from "../auth/account-id";
import type { DsClient } from "../ds/client";
import type { DiscoveryClient } from "../discovery/client";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, String(v));
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return Array.from(this.map.keys())[i] ?? null;
  }
}

const ACCOUNT = new Uint8Array(16).fill(1);
const ACCOUNT_B64 = base64Encode(ACCOUNT);
const key = (fill: number): Uint8Array => new Uint8Array(32).fill(fill);
const keyB64 = (fill: number): string => base64Encode(key(fill));
/** A fake KeyPackage bound to account key `bound`, of device `device`. */
const kp = (bound: number, device: number): Uint8Array => new Uint8Array([bound, device, 0]);

const OLD = 2; // the key the leaves were enrolled under
const NEW = 3; // the key after the change
const ATTACKER = 7;

const IDENTITY = { signaturePublicKey: key(99) } as unknown as WasmIdentity;

function client(devices: readonly Uint8Array[]): ContactsClient {
  return new ContactsClient(
    {
      // Nothing published: the gate passes without touching the pin, so each
      // test states the trust it starts from.
      identityFetch: async () => null,
      fetchKeyPackages: async () =>
        devices.map((keyPackage) => ({ deviceId: key(keyPackage[1] ?? 0), keyPackage })),
    } as unknown as DiscoveryClient,
    { registerGroupEpoch: async () => {} } as unknown as DsClient,
    { identityProvider: () => IDENTITY },
  );
}

/** A group with no role table whose add records what it was handed. */
function group(added: Uint8Array[]): WasmGroup {
  return {
    get groupId(): Uint8Array {
      return new Uint8Array([4, 4, 4, 4]);
    },
    get epoch(): bigint {
      return 5n;
    },
    members: () => [],
    roleTableOwner: () => undefined,
    pendingProposals: () => [],
    dsAccessSigner: fakeAccessSigner(() => 5n),
    addMember: (_id: unknown, keyPackage: Uint8Array) => {
      added.push(keyPackage);
      throw new Sentinel("add reached");
    },
  } as unknown as WasmGroup;
}

/** The account as this device holds it after a PROVEN rotation OLD -> NEW. */
function pinProvenRotation(): void {
  setPin(ACCOUNT_B64, keyB64(OLD), 0, undefined, { keyGeneration: 1 });
  setPin(ACCOUNT_B64, keyB64(NEW), 10, undefined, { proven: true });
}

/** The account as this device holds it after an ACCEPTED RESET OLD -> NEW. */
function pinAcceptedReset(): void {
  setPin(ACCOUNT_B64, keyB64(OLD), 0, undefined, { keyGeneration: 1 });
  acceptReset({
    accountIdB64: ACCOUNT_B64,
    heldKeyB64: keyB64(OLD),
    newKeyB64: keyB64(NEW),
    keyGeneration: 2,
    signedAtMs: 1,
    nowMs: 1_000,
  });
}

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount("dmlld2VyLWFjY291bnQ=");
});

afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

describe("the user-facing add (addMemberToGroup)", () => {
  it("a rotated person is added: devices still bound to the proven predecessor pass, asked for under the NEW key", async () => {
    pinProvenRotation();
    const added: Uint8Array[] = [];
    await expect(
      client([kp(OLD, 21), kp(ATTACKER, 22)]).addMemberToGroup(null, group(added), ACCOUNT, key(NEW)),
    ).rejects.toThrow();
    expect(added).toEqual([kp(OLD, 21)]);
  });

  it("a reset-retired key is refused, even when the caller still expects it", async () => {
    pinAcceptedReset();
    const added: Uint8Array[] = [];
    await expect(
      client([kp(OLD, 21)]).addMemberToGroup(null, group(added), ACCOUNT, key(OLD)),
    ).rejects.toThrow();
    expect(added).toEqual([]);
  });

  it("after a reset only the new identity's devices are added", async () => {
    pinAcceptedReset();
    const added: Uint8Array[] = [];
    await expect(
      client([kp(OLD, 21), kp(NEW, 22)]).addMemberToGroup(null, group(added), ACCOUNT, key(NEW)),
    ).rejects.toThrow();
    expect(added).toEqual([kp(NEW, 22)]);
  });
});

describe("a new chat and a successor build (prepareGroupWith)", () => {
  it("a device bound to the proven predecessor founds the conversation", async () => {
    pinProvenRotation();
    await expect(client([kp(OLD, 21)]).prepareGroupWith(null, ACCOUNT, "chat")).rejects.toBeInstanceOf(
      Sentinel,
    );
  });

  it("another account's devices served under the id found nothing", async () => {
    pinProvenRotation();
    const attempt = client([kp(ATTACKER, 21)]).prepareGroupWith(null, ACCOUNT, "chat");
    await expect(attempt).rejects.toThrow();
    await expect(attempt).rejects.not.toBeInstanceOf(Sentinel);
  });

  it("a device bound to a reset-retired key founds nothing", async () => {
    pinAcceptedReset();
    const attempt = client([kp(OLD, 21)]).prepareGroupWith(null, ACCOUNT, "chat");
    await expect(attempt).rejects.toThrow();
    await expect(attempt).rejects.not.toBeInstanceOf(Sentinel);
  });
});

describe("a chat started with a person whose key the caller holds (startGroupWith)", () => {
  it("asked for under the NEW key, a device still bound to the proven predecessor founds it", async () => {
    pinProvenRotation();
    await expect(
      client([kp(OLD, 21)]).startGroupWith(null, ACCOUNT, key(NEW), "chat"),
    ).rejects.toBeInstanceOf(Sentinel);
  });

  it("a device bound to another key than the expected one founds nothing", async () => {
    const attempt = client([kp(ATTACKER, 21)]).startGroupWith(null, ACCOUNT, key(NEW), "chat");
    await expect(attempt).rejects.toThrow();
    await expect(attempt).rejects.not.toBeInstanceOf(Sentinel);
  });

  it("a reset-retired key founds nothing, even when it is the key the caller expects", async () => {
    pinAcceptedReset();
    const attempt = client([kp(OLD, 21)]).startGroupWith(null, ACCOUNT, key(OLD), "chat");
    await expect(attempt).rejects.toThrow();
    await expect(attempt).rejects.not.toBeInstanceOf(Sentinel);
  });
});

describe("the ADR-0050 re-admission arm (admitDeviceToGroup)", () => {
  it("admits a listed device bound to the proven predecessor", async () => {
    pinProvenRotation();
    const added: Uint8Array[] = [];
    await expect(
      client([]).admitDeviceToGroup(null, group(added), kp(OLD, 21), ACCOUNT),
    ).rejects.toBeInstanceOf(Error);
    expect(added).toEqual([kp(OLD, 21)]);
  });

  it("admits a device bound to the key of this device's verified binding (a member it never pinned)", async () => {
    recordVerifiedBinding(ACCOUNT_B64, { keyB64: keyB64(OLD), keyGeneration: 1, verifiedAtMs: 1 });
    const added: Uint8Array[] = [];
    await expect(
      client([]).admitDeviceToGroup(null, group(added), kp(OLD, 21), ACCOUNT),
    ).rejects.toBeInstanceOf(Error);
    expect(added).toEqual([kp(OLD, 21)]);
  });

  it("does not admit another account's device listed under the id", async () => {
    pinProvenRotation();
    const added: Uint8Array[] = [];
    await expect(client([]).admitDeviceToGroup(null, group(added), kp(ATTACKER, 21), ACCOUNT)).resolves.toBeNull();
    expect(added).toEqual([]);
  });

  it("does not admit a device bound to a reset-retired key", async () => {
    pinAcceptedReset();
    const added: Uint8Array[] = [];
    await expect(client([]).admitDeviceToGroup(null, group(added), kp(OLD, 21), ACCOUNT)).resolves.toBeNull();
    expect(added).toEqual([]);
  });

  it("admits nobody under an account id this device trusts no key for", async () => {
    const added: Uint8Array[] = [];
    await expect(client([]).admitDeviceToGroup(null, group(added), kp(OLD, 21), ACCOUNT)).resolves.toBeNull();
    expect(added).toEqual([]);
  });
});

describe("the membership re-request (readmitDeviceByKey)", () => {
  it("re-admits a device bound to the proven predecessor", async () => {
    pinProvenRotation();
    const added: Uint8Array[] = [];
    await expect(
      client([kp(OLD, 21)]).readmitDeviceByKey(null, group(added), ACCOUNT, key(21)),
    ).rejects.toBeInstanceOf(Error);
    expect(added).toEqual([kp(OLD, 21)]);
  });

  it("does not re-admit a device bound to a reset-retired key", async () => {
    pinAcceptedReset();
    const added: Uint8Array[] = [];
    await expect(
      client([kp(OLD, 21)]).readmitDeviceByKey(null, group(added), ACCOUNT, key(21)),
    ).resolves.toBeNull();
    expect(added).toEqual([]);
  });

  it("never first-sights an account: with no pin and no binding nothing is fetched", async () => {
    const added: Uint8Array[] = [];
    await expect(
      client([kp(OLD, 21)]).readmitDeviceByKey(null, group(added), ACCOUNT, key(21)),
    ).rejects.toThrow("no trusted key");
    expect(added).toEqual([]);
  });
});
