// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * G-53 - which held reach grant writes a Welcome or signs a committer
 * delegation is the core's plan (`planReachWrites`), fed per person and per
 * mailbox by `write-plan.ts`:
 *
 *   - a lower version of a key is superseded wherever it was filed, and
 *     deleted from the store;
 *   - equal grants filed in several conversations are one candidate;
 *   - `group` / `member` keys only while a conversation is shared;
 *   - the grant a search found the person through leads;
 *   - a delegation never signs with a `member` key;
 *   - the grants of a mailbox the person left are planned on their own,
 *     after the current mailbox;
 *   - the unsigned write to an open mailbox follows once every signed
 *     candidate was refused with `401`, never on another failure;
 *   - a roster shrink prunes what a higher version replaced.
 *
 * Driven through the REAL wasm core with real grants; the reach session is a
 * stand-in holding the store.
 */
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  EMPTY_RECEIVED_GRANTS,
  fileContactGrant,
  fileDelegations,
  fileFoundGrant,
  fileScopedGrant,
  type ReceivedGrants,
} from "./received-grants";

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
  reachSignerForGrant: async () => ({ free: () => undefined }),
  reachSignerForGrantWire: async (wire: string) => ({ wire, free: () => undefined }),
}));

const {
  dropSupersededEntries,
  heldEntriesForPerson,
  installSharedScopes,
  planPersonReach,
  pruneGrantsOfScope,
} = await import("./write-plan");
const { MailboxUnreachableError, writeToPerson, writeWelcome } = await import("./mailbox-write");
const { sharedScopeHexesWith } = await import("../contacts/shared-groups");

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

beforeEach(() => {
  store = EMPTY_RECEIVED_GRANTS;
  installSharedScopes(null);
});

const b64 = (bytes: Uint8Array): string => Buffer.from(bytes).toString("base64");
const hex = (bytes: Uint8Array): string => Buffer.from(bytes).toString("hex");
const person = (fill: number): wasm.Identity => wasm.Identity.fromSeed(new Uint8Array(64).fill(fill));

function grantWire(owner: wasm.Identity, cls: wasm.ReachClass, version = 0): string {
  const g = wasm.deriveReachGrant(owner.ed25519Seed, cls, new Uint8Array(0), version);
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

const held = (wireB64: string, version = 0) => ({ wireB64, version, receivedAtMs: 1 });
const SCOPE_A = "a1".repeat(32);
const SCOPE_B = "b2".repeat(32);
const TARGET = new Uint8Array(32).fill(0x7c);
const refused = (): Error => Object.assign(new Error("HTTP 401"), { status: 401 });

/** The signer's wire of a write, as the stand-in session builds it. */
const wireOf = (auth: { signer?: unknown }): string | null =>
  (auth.signer as { wire?: string } | null)?.wire ?? null;

describe("the plan for one person", () => {
  it("keeps the highest version of an account-wide key, wherever it was filed, and names the rest", async () => {
    const bob = person(0x71);
    try {
      const pk = hex(bob.signaturePublicKey);
      const old = grantWire(bob, wasm.ReachClass.Group, 0);
      const cur = grantWire(bob, wasm.ReachClass.Group, 1);
      store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, pk, SCOPE_A, "group", held(old, 0));
      store = fileScopedGrant(store, pk, SCOPE_B, "group", held(cur, 1));
      const plan = await planPersonReach(store, [pk], [bob.signaturePublicKey], "welcome", {
        sharedScopeHexes: [SCOPE_B],
      });
      expect(plan.candidates.map((e) => e.grant.wireB64)).toEqual([cur]);
      expect(plan.superseded.map((e) => e.grant.wireB64)).toEqual([old]);
      const next = dropSupersededEntries(store, plan.superseded);
      expect(next.people[pk]?.scopes[SCOPE_A]).toBeUndefined();
      expect(next.people[pk]?.scopes[SCOPE_B]?.group?.wireB64).toBe(cur);
    } finally {
      bob.free();
    }
  });

  it("collapses one key filed in two conversations into one candidate", async () => {
    const bob = person(0x72);
    try {
      const pk = hex(bob.signaturePublicKey);
      const g = grantWire(bob, wasm.ReachClass.Group, 2);
      store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, pk, SCOPE_A, "group", held(g, 2));
      store = fileScopedGrant(store, pk, SCOPE_B, "group", held(g, 2));
      const plan = await planPersonReach(store, [pk], [bob.signaturePublicKey], "welcome", {
        sharedScopeHexes: [SCOPE_A],
      });
      expect(plan.candidates.map((e) => e.grant.wireB64)).toEqual([g]);
      expect(plan.superseded).toEqual([]);
    } finally {
      bob.free();
    }
  });

  it("offers group and member keys only while a conversation is shared, and keeps them meanwhile", async () => {
    const bob = person(0x73);
    try {
      const pk = hex(bob.signaturePublicKey);
      const member = grantWire(bob, wasm.ReachClass.Member);
      const group = grantWire(bob, wasm.ReachClass.Group);
      const channel = grantWire(bob, wasm.ReachClass.Channel);
      store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, pk, SCOPE_A, "member", held(member));
      store = fileScopedGrant(store, pk, SCOPE_A, "group", held(group));
      store = fileScopedGrant(store, pk, SCOPE_B, "channel", held(channel));
      const alone = await planPersonReach(store, [pk], [bob.signaturePublicKey], "welcome", { sharedScopeHexes: [] });
      expect(alone.candidates.map((e) => e.grant.wireB64)).toEqual([channel]);
      expect(alone.superseded).toEqual([]);
      const shared = await planPersonReach(store, [pk], [bob.signaturePublicKey], "welcome", {
        sharedScopeHexes: [SCOPE_B],
      });
      expect(shared.candidates.map((e) => e.grant.wireB64)).toEqual([member, group, channel]);
    } finally {
      bob.free();
    }
  });

  it("leads with the grant a search found; a delegation never signs with a member key", async () => {
    const bob = person(0x74);
    try {
      const pk = hex(bob.signaturePublicKey);
      const contact = grantWire(bob, wasm.ReachClass.Contact);
      const member = grantWire(bob, wasm.ReachClass.Member);
      const fof = grantWire(bob, wasm.ReachClass.Fof);
      store = fileContactGrant(EMPTY_RECEIVED_GRANTS, pk, held(contact));
      store = fileScopedGrant(store, pk, SCOPE_A, "member", held(member));
      store = fileFoundGrant(store, pk, held(fof));
      const ctx = { sharedScopeHexes: [SCOPE_A] };
      const welcome = await planPersonReach(store, [pk], [bob.signaturePublicKey], "welcome", ctx);
      expect(welcome.candidates.map((e) => e.grant.wireB64)).toEqual([fof, contact, member]);
      const delegation = await planPersonReach(store, [pk], [bob.signaturePublicKey], "delegation", ctx);
      expect(delegation.candidates.map((e) => e.grant.wireB64)).toEqual([fof, contact]);
    } finally {
      bob.free();
    }
  });

  it("plans the mailbox the person left on its own, after the current one", async () => {
    const before = person(0x75);
    const after = person(0x76);
    try {
      const pk = hex(after.signaturePublicKey);
      const oldContact = grantWire(before, wasm.ReachClass.Contact);
      const newContact = grantWire(after, wasm.ReachClass.Contact);
      // The re-sent profile after a rotation: same version, another key; the
      // old grant is kept beside it for the mailbox that was left.
      store = fileContactGrant(EMPTY_RECEIVED_GRANTS, pk, held(oldContact), pk);
      store = fileContactGrant(store, pk, held(newContact), pk);
      expect(heldEntriesForPerson(store, [pk]).map((e) => e.previous)).toEqual([false, true]);
      const plan = await planPersonReach(
        store,
        [pk],
        [after.signaturePublicKey, before.signaturePublicKey],
        "welcome",
        { sharedScopeHexes: [] },
      );
      expect(plan.mailboxes.map((m) => m.addressHex)).toEqual([addressOf(after), addressOf(before)]);
      expect(plan.candidates.map((e) => e.grant.wireB64)).toEqual([newContact, oldContact]);
      // Same version in two mailboxes: neither replaces the other.
      expect(plan.superseded).toEqual([]);
    } finally {
      before.free();
      after.free();
    }
  });

  it("drops a grant whose binding names someone else", async () => {
    const bob = person(0x77);
    const mallory = person(0x78);
    try {
      const pk = hex(bob.signaturePublicKey);
      store = fileContactGrant(EMPTY_RECEIVED_GRANTS, pk, held(grantWire(mallory, wasm.ReachClass.Contact)));
      const plan = await planPersonReach(store, [pk], [bob.signaturePublicKey], "welcome", { sharedScopeHexes: [] });
      expect(plan.candidates).toEqual([]);
      expect(plan.mailboxes).toEqual([]);
    } finally {
      bob.free();
      mallory.free();
    }
  });
});

describe("a Welcome through the plan", () => {
  function noDelegationEnvelope(added: wasm.Identity): string {
    const env = new wasm.CommitterDelegationEnvelope(added.signaturePublicKey, wasm.signMailboxBinding(added), []);
    try {
      return b64(wasm.composeCommitterDelegationEnvelope(env));
    } finally {
      env.free();
    }
  }

  it("deletes the superseded grant and never tries it", async () => {
    const bob = person(0x79);
    try {
      const pk = hex(bob.signaturePublicKey);
      const old = grantWire(bob, wasm.ReachClass.Group, 0);
      const cur = grantWire(bob, wasm.ReachClass.Group, 3);
      store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, pk, SCOPE_A, "group", held(old, 0));
      store = fileScopedGrant(store, pk, SCOPE_B, "group", held(cur, 3));
      installSharedScopes(() => [SCOPE_B]);
      const tried: (string | null)[] = [];
      const out = await writeWelcome(bob.signaturePublicKey, { scope: TARGET, recheck: null }, async (_t, auth) => {
        tried.push(wireOf(auth));
        return "ok";
      });
      expect(out).toBe("ok");
      expect(tried).toEqual([cur]);
      expect(store.people[pk]?.scopes[SCOPE_A]).toBeUndefined();
    } finally {
      bob.free();
    }
  });

  it("holding only a group key from a conversation no longer shared, goes straight to the open mailbox", async () => {
    const bob = person(0x7a);
    try {
      const pk = hex(bob.signaturePublicKey);
      store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, pk, SCOPE_A, "group", held(grantWire(bob, wasm.ReachClass.Group)));
      store = fileDelegations(store, "aa".repeat(32), pk, {
        envelopeB64: noDelegationEnvelope(bob),
        receivedAtMs: 1,
        scopeHex: hex(TARGET),
      });
      const kinds: string[] = [];
      const out = await writeWelcome(bob.signaturePublicKey, { scope: TARGET, recheck: null }, async (t, _a, kind) => {
        kinds.push(kind);
        return hex(t.address);
      });
      expect(out).toBe(addressOf(bob));
      expect(kinds).toEqual(["none"]);
    } finally {
      bob.free();
    }
  });

  it("an open mailbox is not tried after a failure other than 401", async () => {
    const bob = person(0x7b);
    try {
      const pk = hex(bob.signaturePublicKey);
      store = fileContactGrant(EMPTY_RECEIVED_GRANTS, pk, held(grantWire(bob, wasm.ReachClass.Contact)));
      store = fileDelegations(store, "aa".repeat(32), pk, {
        envelopeB64: noDelegationEnvelope(bob),
        receivedAtMs: 1,
        scopeHex: hex(TARGET),
      });
      const offline = Object.assign(new Error("offline"), { status: 503 });
      const kinds: string[] = [];
      await expect(
        writeWelcome(bob.signaturePublicKey, { scope: TARGET, recheck: null }, async (_t, _a, kind) => {
          kinds.push(kind);
          throw offline;
        }),
      ).rejects.toBe(offline);
      expect(kinds).toEqual(["kept"]);
    } finally {
      bob.free();
    }
  });

  it("with no open record, every refusal keeps the Welcome owed", async () => {
    const bob = person(0x7c);
    try {
      const pk = hex(bob.signaturePublicKey);
      store = fileContactGrant(EMPTY_RECEIVED_GRANTS, pk, held(grantWire(bob, wasm.ReachClass.Contact)));
      const kinds: string[] = [];
      await expect(
        writeWelcome(bob.signaturePublicKey, { scope: TARGET, recheck: null }, async (_t, _a, kind) => {
          kinds.push(kind);
          throw refused();
        }),
      ).rejects.toBeInstanceOf(MailboxUnreachableError);
      expect(kinds).toEqual(["kept"]);
    } finally {
      bob.free();
    }
  });
});

describe("a contact request through the plan", () => {
  const request = (key: Uint8Array, write: Parameters<typeof writeToPerson>[2], explicit: string | null = null) =>
    writeToPerson(key, { explicit, exactKey: true, contactRequest: true, recheck: null }, write);

  it("holding only a group key of a conversation no longer shared, writes nothing and is unreachable", async () => {
    const bob = person(0x7e);
    try {
      const pk = hex(bob.signaturePublicKey);
      store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, pk, SCOPE_A, "group", held(grantWire(bob, wasm.ReachClass.Group)));
      const write = vi.fn(async () => "ok");
      await expect(request(bob.signaturePublicKey, write)).rejects.toBeInstanceOf(MailboxUnreachableError);
      expect(write).not.toHaveBeenCalled();
    } finally {
      bob.free();
    }
  });

  it("uses the group key while a conversation is shared, after the grant a search found", async () => {
    const bob = person(0x7f);
    try {
      const pk = hex(bob.signaturePublicKey);
      const group = grantWire(bob, wasm.ReachClass.Group);
      const fof = grantWire(bob, wasm.ReachClass.Fof);
      store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, pk, SCOPE_A, "group", held(group));
      store = fileFoundGrant(store, pk, held(fof));
      installSharedScopes(() => [SCOPE_A]);
      const tried: (string | null)[] = [];
      const out = await request(bob.signaturePublicKey, async (_t, auth) => {
        tried.push(wireOf(auth));
        if (wireOf(auth) === fof) throw refused();
        return "ok";
      });
      expect(out).toBe("ok");
      expect(tried).toEqual([fof, group]);
    } finally {
      bob.free();
    }
  });

  it("every candidate refused is unreachable, the last refusal kept as its cause", async () => {
    const bob = person(0x80);
    try {
      const pk = hex(bob.signaturePublicKey);
      store = fileContactGrant(EMPTY_RECEIVED_GRANTS, pk, held(grantWire(bob, wasm.ReachClass.Contact)));
      const refusal = Object.assign(refused(), { clockSkewed: true });
      const err = await request(bob.signaturePublicKey, async () => {
        throw refusal;
      }).catch((e: unknown) => e);
      expect(err).toBeInstanceOf(MailboxUnreachableError);
      expect((err as { cause?: unknown }).cause).toBe(refusal);
    } finally {
      bob.free();
    }
  });
});

describe("a roster shrink", () => {
  it("prunes the grants of that conversation a higher version replaced, and keeps the highest", async () => {
    const bob = person(0x7d);
    try {
      const pk = hex(bob.signaturePublicKey);
      const old = grantWire(bob, wasm.ReachClass.Member, 0);
      const cur = grantWire(bob, wasm.ReachClass.Member, 1);
      const group = grantWire(bob, wasm.ReachClass.Group, 0);
      store = fileScopedGrant(EMPTY_RECEIVED_GRANTS, pk, SCOPE_A, "member", held(old, 0));
      store = fileScopedGrant(store, pk, SCOPE_A, "group", held(group, 0));
      store = fileScopedGrant(store, pk, SCOPE_B, "member", held(cur, 1));
      await pruneGrantsOfScope(SCOPE_A);
      expect(store.people[pk]?.scopes[SCOPE_A]?.member).toBeUndefined();
      expect(store.people[pk]?.scopes[SCOPE_A]?.group?.wireB64).toBe(group);
      expect(store.people[pk]?.scopes[SCOPE_B]?.member?.wireB64).toBe(cur);
    } finally {
      bob.free();
    }
  });
});

describe("the shared scopes", () => {
  it("are the active conversations holding this account and one of the person's keys", () => {
    const groups = [
      { id: "01", people: ["own", "p1"], active: true },
      { id: "02", people: ["own", "p0"], active: true },
      { id: "03", people: ["own", "p1"], active: false },
      { id: "04", people: ["p1"], active: true },
      { id: "05", people: ["own", "other"], active: true },
    ];
    expect(
      sharedScopeHexesWith({
        groups,
        personKeysHex: ["p1", "p0"],
        ownKeyHex: "own",
        isActive: (g) => g.active,
        peopleHexOf: (g) => g.people,
        groupIdHexOf: (g) => g.id,
      }),
    ).toEqual(["01", "02"]);
    expect(
      sharedScopeHexesWith({
        groups,
        personKeysHex: ["p1"],
        ownKeyHex: null,
        isActive: () => true,
        peopleHexOf: (g) => g.people,
        groupIdHexOf: (g) => g.id,
      }),
    ).toEqual([]);
  });
});
