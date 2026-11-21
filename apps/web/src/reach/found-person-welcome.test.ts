// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 (amended 2026-10-29) - the Welcome reaches a person
 * added from a network search:
 *
 *   - a grant the search found them through is kept as one held for them, and
 *     only when its binding verifies under their key;
 *   - a person found in Everyone gets a committer envelope with NO delegation
 *     carrying their verified binding, recorded for this device and group;
 *   - a committer writes the Welcome unsigned only when it holds no signed way
 *     to reach the person AND such an envelope names them for this group;
 *     never for anyone else, and a person with neither stays owed.
 *
 * Driven through the REAL wasm core; the reach session is a stand-in holding
 * the store.
 */
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { EMPTY_RECEIVED_GRANTS, fileContactGrant, fileDelegations, fileFoundGrant, grantsInOrder, type ReceivedGrants } from "./received-grants";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));

const OWN_HEX = "ee".repeat(32);
let store: ReceivedGrants = EMPTY_RECEIVED_GRANTS;
const session = {
  accountKeyHex: OWN_HEX,
  receivedGrants: () => store,
  updateReceivedGrants: async (fn: (g: ReceivedGrants) => ReceivedGrants) => {
    store = fn(store);
  },
  delegateKeys: () => [],
};
vi.mock("./reach-service", () => ({
  reachSession: () => session,
  reachSignerForGrant: async () => ({ signed: true, free: () => undefined }),
  reachSignerForGrantWire: async () => ({ signed: true, free: () => undefined }),
}));

const { keepFoundGrant, keepsFoundGrantClass, openCommitterEnvelope } = await import("./found-grant");
const { MailboxUnreachableError, writeWelcome } = await import("./mailbox-write");

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

beforeEach(() => {
  store = EMPTY_RECEIVED_GRANTS;
});

const b64 = (bytes: Uint8Array): string => Buffer.from(bytes).toString("base64");
const hex = (bytes: Uint8Array): string => Buffer.from(bytes).toString("hex");
const person = (fill: number): wasm.Identity => wasm.Identity.fromSeed(new Uint8Array(64).fill(fill));

function grantWire(owner: wasm.Identity, cls: wasm.ReachClass): string {
  const g = wasm.deriveReachGrant(owner.ed25519Seed, cls, new Uint8Array(0), 0);
  try {
    return b64(wasm.encodeReachGrant(g));
  } finally {
    g.free();
  }
}

function addressOf(owner: wasm.Identity): string {
  const key = wasm.MailboxKey.fromAccountIdentity(owner);
  try {
    return hex(key.address());
  } finally {
    key.free();
  }
}

const SCOPE = new Uint8Array(32).fill(0x31);
const OTHER_SCOPE = new Uint8Array(32).fill(0x32);
const refused = (): Error => Object.assign(new Error("HTTP 401"), { status: 401 });

describe("a grant found by search is kept as held", () => {
  it("is filed for the person when its binding verifies under their key", async () => {
    const bob = person(0x61);
    try {
      const wire = grantWire(bob, wasm.ReachClass.Fof);
      expect(await keepFoundGrant(bob.signaturePublicKey, wire)).toBe(true);
      expect(store.people[hex(bob.signaturePublicKey)]?.found?.wireB64).toBe(wire);
      // And the ordinary held-grant order now offers it.
      expect(grantsInOrder(store, hex(bob.signaturePublicKey)).map((g) => g.wireB64)).toEqual([wire]);
    } finally {
      bob.free();
    }
  });

  it("is refused when its binding names someone else, or its class never comes from a search", async () => {
    const bob = person(0x62);
    const mallory = person(0x63);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      expect(await keepFoundGrant(bob.signaturePublicKey, grantWire(mallory, wasm.ReachClass.Fof))).toBe(false);
      expect(await keepFoundGrant(bob.signaturePublicKey, grantWire(bob, wasm.ReachClass.Contact))).toBe(false);
      expect(await keepFoundGrant(bob.signaturePublicKey, "AAAA")).toBe(false);
      expect(store).toBe(EMPTY_RECEIVED_GRANTS);
      expect(keepsFoundGrantClass("fof")).toBe(true);
      expect(keepsFoundGrantClass("link")).toBe(false);
    } finally {
      warn.mockRestore();
      bob.free();
      mallory.free();
    }
  });

  it("comes after the contact, group and channel grants, before this group's member key", () => {
    const h = (n: number) => ({ wireB64: `w${n}`, version: 0, receivedAtMs: n });
    let s = fileContactGrant(EMPTY_RECEIVED_GRANTS, "p", h(1));
    s = fileFoundGrant(s, "p", h(2));
    expect(fileFoundGrant(s, "p", h(2))).toBe(s);
    expect(grantsInOrder(s, "p").map((g) => g.wireB64)).toEqual(["w1", "w2"]);
  });
});

describe("a person found in Everyone (the proposer)", () => {
  it("gets an envelope with no delegation and their binding, recorded for this group", async () => {
    const bob = person(0x64);
    try {
      const binding = wasm.signMailboxBinding(bob);
      const bytes = await openCommitterEnvelope(hex(SCOPE), bob.signaturePublicKey, b64(binding));
      expect(bytes).not.toBeNull();
      const env = wasm.decodeCommitterDelegationEnvelope(bytes!);
      try {
        expect(env.delegationCount()).toBe(0);
        expect(hex(env.addedMailboxBinding)).toBe(hex(binding));
        expect(hex(env.addedAccountKey)).toBe(hex(bob.signaturePublicKey));
      } finally {
        env.free();
      }
      const recorded = store.delegations[`${OWN_HEX}|${hex(bob.signaturePublicKey)}`];
      expect(recorded?.scopeHex).toBe(hex(SCOPE));
      expect(recorded?.envelopeB64).toBe(b64(bytes!));
      // A re-request with no binding in hand re-sends the recorded one, for
      // this group only.
      expect(b64((await openCommitterEnvelope(hex(SCOPE), bob.signaturePublicKey, null))!)).toBe(b64(bytes!));
      expect(await openCommitterEnvelope(hex(OTHER_SCOPE), bob.signaturePublicKey, null)).toBeNull();
    } finally {
      bob.free();
    }
  });

  it("posts and records nothing when the binding is not the person's", async () => {
    const bob = person(0x65);
    const mallory = person(0x66);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      expect(
        await openCommitterEnvelope(hex(SCOPE), bob.signaturePublicKey, b64(wasm.signMailboxBinding(mallory))),
      ).toBeNull();
      expect(await openCommitterEnvelope(hex(SCOPE), bob.signaturePublicKey, null)).toBeNull();
      expect(store).toBe(EMPTY_RECEIVED_GRANTS);
    } finally {
      warn.mockRestore();
      bob.free();
      mallory.free();
    }
  });
});

describe("the committer's Welcome", () => {
  function noDelegationEnvelope(added: wasm.Identity, binding: Uint8Array): string {
    const env = new wasm.CommitterDelegationEnvelope(added.signaturePublicKey, binding, []);
    try {
      return b64(wasm.composeCommitterDelegationEnvelope(env));
    } finally {
      env.free();
    }
  }

  it("goes unsigned to the binding of a kept no-delegation envelope for this group, spending no proof", async () => {
    const bob = person(0x67);
    try {
      store = fileDelegations(EMPTY_RECEIVED_GRANTS, "aa".repeat(32), hex(bob.signaturePublicKey), {
        envelopeB64: noDelegationEnvelope(bob, wasm.signMailboxBinding(bob)),
        receivedAtMs: 1,
        scopeHex: hex(SCOPE),
      });
      const write = vi.fn(async (target: { address: Uint8Array }, auth: { signer?: unknown }, kind: string) => {
        expect(auth.signer ?? null).toBeNull();
        expect(kind).toBe("none");
        return hex(target.address);
      });
      expect(await writeWelcome(bob.signaturePublicKey, { scope: SCOPE, recheck: null }, write)).toBe(addressOf(bob));
      expect(write).toHaveBeenCalledTimes(1);
    } finally {
      bob.free();
    }
  });

  it("is not written when the kept envelope's binding is not the added person's", async () => {
    const bob = person(0x6d);
    const mallory = person(0x6e);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      let envelopeB64: string;
      try {
        envelopeB64 = noDelegationEnvelope(bob, wasm.signMailboxBinding(mallory));
      } catch {
        // The core refuses to even build it: nothing could ever be kept.
        return;
      }
      store = fileDelegations(EMPTY_RECEIVED_GRANTS, "aa".repeat(32), hex(bob.signaturePublicKey), {
        envelopeB64,
        receivedAtMs: 1,
        scopeHex: hex(SCOPE),
      });
      const write = vi.fn(async () => 1);
      await expect(writeWelcome(bob.signaturePublicKey, { scope: SCOPE, recheck: null }, write)).rejects.toBeInstanceOf(
        MailboxUnreachableError,
      );
      expect(write).not.toHaveBeenCalled();
    } finally {
      warn.mockRestore();
      bob.free();
      mallory.free();
    }
  });

  it("this device's own record serves its own sweep", async () => {
    const bob = person(0x68);
    try {
      await openCommitterEnvelope(hex(SCOPE), bob.signaturePublicKey, b64(wasm.signMailboxBinding(bob)));
      const write = vi.fn(async (target: { address: Uint8Array }) => hex(target.address));
      expect(await writeWelcome(bob.signaturePublicKey, { scope: SCOPE, recheck: null }, write)).toBe(addressOf(bob));
    } finally {
      bob.free();
    }
  });

  it("is never unsigned for another group, an envelope with no group, or another person", async () => {
    const bob = person(0x69);
    const carol = person(0x6a);
    try {
      store = fileDelegations(EMPTY_RECEIVED_GRANTS, "aa".repeat(32), hex(bob.signaturePublicKey), {
        envelopeB64: noDelegationEnvelope(bob, wasm.signMailboxBinding(bob)),
        receivedAtMs: 1,
        scopeHex: hex(OTHER_SCOPE),
      });
      // Recorded before the group was recorded with it: authorises nothing.
      store = fileDelegations(store, "bb".repeat(32), hex(bob.signaturePublicKey), {
        envelopeB64: noDelegationEnvelope(bob, wasm.signMailboxBinding(bob)),
        receivedAtMs: 2,
      });
      const write = vi.fn(async () => 1);
      await expect(writeWelcome(bob.signaturePublicKey, { scope: SCOPE, recheck: null }, write)).rejects.toBeInstanceOf(
        MailboxUnreachableError,
      );
      // Carol has nothing; Bob's envelope does not reach her.
      await expect(writeWelcome(carol.signaturePublicKey, { scope: OTHER_SCOPE, recheck: null }, write)).rejects.toBeInstanceOf(
        MailboxUnreachableError,
      );
      expect(write).not.toHaveBeenCalled();
    } finally {
      bob.free();
      carol.free();
    }
  });

  it("goes through a held grant first; with that grant refused it is written unsigned (G-53)", async () => {
    const bob = person(0x6b);
    try {
      const contact = grantWire(bob, wasm.ReachClass.Contact);
      store = fileContactGrant(EMPTY_RECEIVED_GRANTS, hex(bob.signaturePublicKey), {
        wireB64: contact,
        version: 0,
        receivedAtMs: 1,
      });
      store = fileDelegations(store, "aa".repeat(32), hex(bob.signaturePublicKey), {
        envelopeB64: noDelegationEnvelope(bob, wasm.signMailboxBinding(bob)),
        receivedAtMs: 1,
        scopeHex: hex(SCOPE),
      });
      const kinds: string[] = [];
      const write = vi.fn(async (target: { address: Uint8Array }, auth: { signer?: unknown }, kind: string) => {
        kinds.push(kind);
        if (kind !== "none") throw refused();
        expect(auth.signer ?? null).toBeNull();
        return hex(target.address);
      });
      expect(await writeWelcome(bob.signaturePublicKey, { scope: SCOPE, recheck: null }, write)).toBe(addressOf(bob));
      expect(kinds).toEqual(["kept", "none"]);
    } finally {
      bob.free();
    }
  });

  it("a person with neither a grant nor such an envelope stays owed", async () => {
    const bob = person(0x6c);
    try {
      const write = vi.fn(async () => 1);
      await expect(writeWelcome(bob.signaturePublicKey, { scope: SCOPE, recheck: null }, write)).rejects.toBeInstanceOf(
        MailboxUnreachableError,
      );
      expect(write).not.toHaveBeenCalled();
    } finally {
      bob.free();
    }
  });
});
