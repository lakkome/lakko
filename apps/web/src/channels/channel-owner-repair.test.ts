// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * 2026-11-20 — an owner device whose own channel refuses its owner proof
 * moves the owner key to its current identity's, signed by a predecessor.
 */
import { beforeEach, describe, expect, it } from "vitest";

import {
  repairChannelOwner,
  repairChannelOwnerIfRefused,
  resetChannelOwnerRepairs,
  type PutOwner,
} from "./channel-owner-repair";
import type { Wasm } from "../wasm";

type Identity = InstanceType<Wasm["Identity"]>;

function identity(n: number): Identity {
  return { signaturePublicKey: new Uint8Array(32).fill(n) } as unknown as Identity;
}

const ID = "ab".repeat(16);

/** A service that holds `owner` and hands it over on a proof by it. */
function service(owner: Identity): { put: PutOwner; owner: () => number; calls: () => number } {
  let held = owner.signaturePublicKey[0] ?? 0;
  let calls = 0;
  return {
    put: async (_id, signing, naming) => {
      calls += 1;
      if ((signing.signaturePublicKey[0] ?? -1) !== held) return 401;
      held = naming.signaturePublicKey[0] ?? 0;
      return 204;
    },
    owner: () => held,
    calls: () => calls,
  };
}

beforeEach(() => resetChannelOwnerRepairs());

describe("repairChannelOwner", () => {
  it("hands the channel from the predecessor that owns it to the current key", async () => {
    const svc = service(identity(1));
    expect(await repairChannelOwner(ID, [identity(2), identity(1)], identity(3), svc.put)).toBe("owned");
    expect(svc.owner()).toBe(3);
  });

  it("is owned when the current key already owns the channel", async () => {
    const svc = service(identity(3));
    expect(await repairChannelOwner(ID, [identity(1)], identity(3), svc.put)).toBe("owned");
  });

  it("is not the owner when no key of the account owns it, and retries with no answer", async () => {
    expect(await repairChannelOwner(ID, [identity(1)], identity(3), service(identity(7)).put)).toBe("not-owner");
    expect(await repairChannelOwner(ID, [identity(1)], identity(3), async () => null)).toBe("retry");
  });
});

describe("repairChannelOwnerIfRefused", () => {
  it("runs only for a refused channel, once a session", async () => {
    const svc = service(identity(1));
    let owned = 0;
    const input = {
      channelIdHex: ID,
      predecessors: [identity(1)],
      current: identity(3),
      onOwned: () => {
        owned += 1;
      },
      put: svc.put,
    };
    expect(await repairChannelOwnerIfRefused({ ...input, refused: false })).toBeNull();
    expect(svc.calls()).toBe(0);
    expect(await repairChannelOwnerIfRefused({ ...input, refused: true })).toBe("owned");
    expect(owned).toBe(1);
    expect(await repairChannelOwnerIfRefused({ ...input, refused: true })).toBeNull();
    expect(svc.calls()).toBe(1);
  });
});
