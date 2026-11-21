// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 6 - a proposer re-sends its committer envelope while its Add
 * waits. Driven through the REAL wasm core: the rule that says "due" is the
 * core's, and the envelopes are real ones, read back with the core's own check.
 * The reach session is a stand-in holding the store, the group a double
 * (promoting an admin on a real one would need a whole second member).
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import type { Group as WasmGroup } from "../../wasm/lakko_ffi_wasm";
import type { IdentitySecrets } from "../auth/AuthContext";
import type { DiscoveryClient } from "../discovery/client";
import type { DsClient } from "../ds/client";
import { withCommitRoute } from "../ds/access-test-doubles";
import {
  forgetOwnRequest,
  loadOwnRequests,
  noteCommitterEnvelopeSent,
  ownAddIsPending,
  recordOwnRequest,
  type IntentSnapshot,
  type OwnRequest,
  type OwnRequestRecord,
} from "../chat/membership-retry";
import {
  EMPTY_RECEIVED_GRANTS,
  fileContactGrant,
  fileDelegations,
  fileFoundGrant,
  fileScopedGrant,
  type ReceivedGrants,
} from "./received-grants";

const posted: { bytes: Uint8Array }[] = [];

vi.mock("../wasm", () => ({
  loadWasm: async () => ({
    ...wasm,
    // The add path's KeyPackage reads, over this suite's fake KeyPackages.
    keyPackageAccountKey: (kp: Uint8Array) => kp.slice(0, 32),
    keyPackageSignatureKey: (kp: Uint8Array) => kp.slice(32, 64),
  }),
}));

const OWN_HEX = "ee".repeat(32);
const OWN_ID_B64 = "b3duLWFjY291bnQ=";
let store: ReceivedGrants = EMPTY_RECEIVED_GRANTS;
const session = {
  accountIdB64: OWN_ID_B64,
  accountKeyHex: OWN_HEX,
  receivedGrants: () => store,
  updateReceivedGrants: async (fn: (g: ReceivedGrants) => ReceivedGrants) => {
    store = fn(store);
  },
};
vi.mock("./reach-service", () => ({
  reachSession: () => session,
  reachSignerForGrant: async () => ({ signed: true, free: () => undefined }),
  reachSignerForGrantWire: async () => ({ signed: true, free: () => undefined }),
}));
vi.mock("../chat/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../chat/client")>()),
  ChatClient: class {
    async broadcastControl(_identity: unknown, _group: unknown, bytes: Uint8Array) {
      posted.push({ bytes });
      return { messageId: 1 };
    }
  },
}));

const {
  committerCandidateHexes,
  committerEnvelopeDueNow,
  runCommitterEnvelopeResends,
  sendCommitterEnvelope,
} = await import("./committer-envelope");
const { ContactsClient } = await import("../contacts/client");
const { bindContactsAccount } = await import("../contacts/local-store");

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

const b64 = (bytes: Uint8Array): string => Buffer.from(bytes).toString("base64");
const hex = (bytes: Uint8Array): string => Buffer.from(bytes).toString("hex");
const person = (fill: number): wasm.Identity => wasm.Identity.fromSeed(new Uint8Array(64).fill(fill));

const HOUR_MS = 60 * 60 * 1000;
const START_MS = 1_800_000_000_000;
const GROUP_ID = new Uint8Array(32).fill(0x31);
const GROUP_B64 = b64(GROUP_ID);
const OWNER_KEY = new Uint8Array(32).fill(0x0a);
const ADMIN_KEY = new Uint8Array(32).fill(0x0b);
const NEW_ADMIN_KEY = new Uint8Array(32).fill(0x0c);
const DEVICE_KEY_B64 = b64(new Uint8Array(32).fill(0x77));

interface Harness {
  now: number;
  admins: Uint8Array[];
  roster: { signaturePublicKey: Uint8Array; leafIndex: number }[];
  pendingAdd: boolean;
  publishFails: boolean;
  bytes: Uint8Array[];
  sends: number;
}

let h: Harness;
let bob: wasm.Identity;
let bobKeyB64: string;
let nowSpy: ReturnType<typeof vi.spyOn>;

function groupDouble(): WasmGroup {
  return {
    groupId: GROUP_ID,
    roleTableOwner: () => OWNER_KEY,
    roleTableAdmins: () => {
      const out = new Uint8Array(h.admins.length * 32);
      h.admins.forEach((k, i) => out.set(k, i * 32));
      return out;
    },
    members: () => h.roster,
    committerRank: () => undefined,
  } as unknown as WasmGroup;
}

function snapshot(over: Partial<IntentSnapshot> = {}): IntentSnapshot {
  return {
    roster: h.roster.map((m) => ({
      leafIndex: m.leafIndex,
      signatureKeyB64: b64(m.signaturePublicKey),
    })),
    pendingKeys: h.pendingAdd ? [`add:${DEVICE_KEY_B64}`] : [],
    pendingCount: h.pendingAdd ? 1 : 0,
    roleTable: null,
    membershipEnded: false,
    retainedJoinKeys: [],
    epoch: 5n,
    ...over,
  };
}

function addRequest(): OwnRequest {
  return { kind: "add", subjectKeyB64: bobKeyB64, subjectAccountIdB64: "Ym9iLWFjY291bnQ=" };
}

function record(): OwnRequestRecord | undefined {
  return loadOwnRequests(OWN_ID_B64, GROUP_B64).get(`add:${bobKeyB64}`);
}

function holdGrantForBob(): void {
  const g = wasm.deriveReachGrant(bob.ed25519Seed, wasm.ReachClass.Contact, new Uint8Array(0), 0);
  store = fileContactGrant(store, hex(bob.signaturePublicKey), {
    wireB64: b64(wasm.encodeReachGrant(g)),
    version: 0,
    receivedAtMs: START_MS,
  });
  g.free();
}

function holdOpenEnvelopeForBob(): void {
  const env = new wasm.CommitterDelegationEnvelope(bob.signaturePublicKey, wasm.signMailboxBinding(bob), []);
  store = fileDelegations(store, OWN_HEX, hex(bob.signaturePublicKey), {
    envelopeB64: b64(wasm.composeCommitterDelegationEnvelope(env)),
    receivedAtMs: START_MS,
    scopeHex: hex(GROUP_ID),
  });
  env.free();
}

function send(): ReturnType<typeof sendCommitterEnvelope> {
  return sendCommitterEnvelope({
    group: groupDouble(),
    addedAccountKey: bob.signaturePublicKey,
    openBindingB64: null,
    broadcast: async (bytes) => {
      if (h.publishFails) throw new Error("DS down");
      h.bytes.push(bytes);
    },
    onSent: (s) => noteCommitterEnvelopeSent(OWN_ID_B64, GROUP_B64, bobKeyB64, s),
  });
}

function pass(over: Partial<IntentSnapshot> = {}): ReturnType<typeof runCommitterEnvelopeResends> {
  return runCommitterEnvelopeResends({
    accountIdB64: OWN_ID_B64,
    groupIdB64: GROUP_B64,
    snapshot: () => snapshot(over),
    candidatesHex: () => committerCandidateHexes(groupDouble()),
    due: async (last, candidates, nowMs) => committerEnvelopeDueNow(wasm, last, candidates, nowMs),
    send: async () => {
      h.sends += 1;
      return send();
    },
    grantsToken: () => store,
    nowMs: () => h.now,
  });
}

/** The delegates a posted envelope names, and when they expire. */
function delegatesOf(bytes: Uint8Array): { delegates: string[]; expiresAtMs: bigint[] } {
  const env = wasm.decodeCommitterDelegationEnvelope(bytes);
  const list = wasm.checkCommitterDelegations(env);
  const out = {
    delegates: list.map((d) => hex(d.delegatePk)),
    expiresAtMs: list.map((d) => d.expiresAtMs),
  };
  list.forEach((d) => d.free());
  env.free();
  return out;
}

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage =
    new MemoryStorage() as unknown as Storage;
  bindContactsAccount(OWN_ID_B64);
  // A value of its own per test: the "nothing to send" memo is keyed on it.
  store = { ...EMPTY_RECEIVED_GRANTS };
  posted.length = 0;
  h = {
    now: START_MS,
    admins: [],
    roster: [],
    pendingAdd: true,
    publishFails: false,
    bytes: [],
    sends: 0,
  };
  bob = person(0x61);
  bobKeyB64 = b64(bob.signaturePublicKey);
  nowSpy = vi.spyOn(Date, "now").mockImplementation(() => h.now);
  recordOwnRequest(OWN_ID_B64, GROUP_B64, addRequest(), 5n, [DEVICE_KEY_B64]);
});

afterEach(() => {
  nowSpy.mockRestore();
  bob.free();
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
});

describe("the first send", () => {
  it("is noted once published: the time, signed, and the candidates it covered", async () => {
    holdGrantForBob();
    h.admins = [ADMIN_KEY];
    expect(await send()).toBe("sent");
    expect(h.bytes).toHaveLength(1);
    expect(record()?.envelope).toEqual({
      sentAtMs: START_MS,
      signed: true,
      coveredHex: [hex(OWNER_KEY), hex(ADMIN_KEY)],
    });
    expect(delegatesOf(h.bytes[0] as Uint8Array).delegates).toEqual([hex(OWNER_KEY), hex(ADMIN_KEY)]);
  });

  it("is not noted when the publish failed, so the next poll tail sends it", async () => {
    holdGrantForBob();
    h.publishFails = true;
    expect(await send()).toBe("failed");
    expect(record()?.envelope).toBeUndefined();

    h.publishFails = false;
    expect(await pass()).toEqual({ sent: 1, failed: 0, nothingToSend: 0 });
    expect(record()?.envelope?.sentAtMs).toBe(START_MS);
  });

  it("survives a re-request: the send is about the person, not one proposal", async () => {
    holdGrantForBob();
    await send();
    recordOwnRequest(OWN_ID_B64, GROUP_B64, addRequest(), 6n, [DEVICE_KEY_B64]);
    expect(record()?.epoch).toBe("6");
    expect(record()?.envelope?.sentAtMs).toBe(START_MS);
  });

  it("a stored send that does not parse reads as nothing sent", async () => {
    holdGrantForBob();
    await send();
    const key = `lakko.own-requests.v1.${OWN_ID_B64}.${GROUP_B64}`;
    const raw = JSON.parse(globalThis.localStorage.getItem(key) ?? "{}") as Record<
      string,
      { envelope: { coveredHex: string[] } }
    >;
    (raw[`add:${bobKeyB64}`] as { envelope: { coveredHex: string[] } }).envelope.coveredHex = ["zz"];
    globalThis.localStorage.setItem(key, JSON.stringify(raw));
    expect(record()?.envelope).toBeUndefined();
    expect(record()?.deviceKeysB64).toEqual([DEVICE_KEY_B64]);
  });
});

describe("which grant signs (G-53, the core's plan)", () => {
  function wireOf(cls: wasm.ReachClass, version = 0): { wireB64: string; issuerHex: string } {
    const g = wasm.deriveReachGrant(bob.ed25519Seed, cls, new Uint8Array(0), version);
    try {
      return { wireB64: b64(wasm.encodeReachGrant(g)), issuerHex: hex(g.verifyingKey()) };
    } finally {
      g.free();
    }
  }
  function issuersOf(bytes: Uint8Array): string[] {
    const env = wasm.decodeCommitterDelegationEnvelope(bytes);
    const list = wasm.checkCommitterDelegations(env);
    const out = list.map((d) => hex(d.issuerPk));
    list.forEach((d) => d.free());
    env.free();
    return out;
  }
  const OTHER_SCOPE_HEX = "5a".repeat(32);

  it("the grant a search found leads; a group key of a conversation no longer shared is not used", async () => {
    const group = wireOf(wasm.ReachClass.Group, 4);
    const fof = wireOf(wasm.ReachClass.Fof);
    store = fileScopedGrant(store, hex(bob.signaturePublicKey), OTHER_SCOPE_HEX, "group", {
      wireB64: group.wireB64,
      version: 4,
      receivedAtMs: START_MS,
    });
    store = fileFoundGrant(store, hex(bob.signaturePublicKey), { wireB64: fof.wireB64, version: 0, receivedAtMs: START_MS });
    expect(await send()).toBe("sent");
    expect(issuersOf(h.bytes[0] as Uint8Array)).toEqual([fof.issuerHex]);
  });

  it("an add from search with an open record posts the open envelope whatever grant is held, and re-sends it", async () => {
    holdGrantForBob();
    const binding = wasm.signMailboxBinding(bob);
    const r = await sendCommitterEnvelope({
      group: groupDouble(),
      addedAccountKey: bob.signaturePublicKey,
      openBindingB64: b64(binding),
      broadcast: async (bytes) => {
        h.bytes.push(bytes);
      },
      onSent: (sent) => noteCommitterEnvelopeSent(OWN_ID_B64, GROUP_B64, bobKeyB64, sent),
    });
    expect(r).toBe("sent");
    expect(record()?.envelope?.signed).toBe(false);
    const env = wasm.decodeCommitterDelegationEnvelope(h.bytes[0] as Uint8Array);
    try {
      expect(env.delegationCount()).toBe(0);
      expect(hex(env.addedMailboxBinding)).toBe(hex(binding));
    } finally {
      env.free();
    }
    // A re-send carries no binding: the recorded open envelope goes again,
    // not one signed with the held grant.
    expect(await send()).toBe("sent");
    expect(b64(h.bytes[1] as Uint8Array)).toBe(b64(h.bytes[0] as Uint8Array));
  });

  it("an open binding that is not the person's falls back to the signed envelope", async () => {
    holdGrantForBob();
    const mallory = person(0x64);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      const r = await sendCommitterEnvelope({
        group: groupDouble(),
        addedAccountKey: bob.signaturePublicKey,
        openBindingB64: b64(wasm.signMailboxBinding(mallory)),
        broadcast: async (bytes) => {
          h.bytes.push(bytes);
        },
        onSent: () => undefined,
      });
      expect(r).toBe("sent");
      expect(issuersOf(h.bytes[0] as Uint8Array)).toHaveLength(1);
    } finally {
      warn.mockRestore();
      mallory.free();
    }
  });

  it("never signs with a member key, and deletes a lower version it replaced", async () => {
    const member = wireOf(wasm.ReachClass.Member);
    const oldContact = wireOf(wasm.ReachClass.Contact, 0);
    const contact = wireOf(wasm.ReachClass.Contact, 1);
    const pk = hex(bob.signaturePublicKey);
    store = fileScopedGrant(store, pk, OTHER_SCOPE_HEX, "member", { wireB64: member.wireB64, version: 0, receivedAtMs: 1 });
    // A contact grant of a lower version, still filed as found: the same key
    // family as the held contact grant.
    store = fileFoundGrant(store, pk, { wireB64: oldContact.wireB64, version: 0, receivedAtMs: 1 });
    store = fileContactGrant(store, pk, { wireB64: contact.wireB64, version: 1, receivedAtMs: 1 });
    expect(await send()).toBe("sent");
    expect(issuersOf(h.bytes[0] as Uint8Array)).toEqual([contact.issuerHex]);
    expect(store.people[pk]?.found).toBeUndefined();
    expect(store.people[pk]?.scopes[OTHER_SCOPE_HEX]?.member?.wireB64).toBe(member.wireB64);
  });
});

describe("the re-send of a signed envelope", () => {
  beforeEach(async () => {
    holdGrantForBob();
    await send();
    h.bytes.length = 0;
  });

  it("is not due while young, however many polls run", async () => {
    h.now = START_MS + 12 * HOUR_MS - 1;
    for (let i = 0; i < 5; i++) await pass();
    expect(h.sends).toBe(0);
    expect(record()?.envelope?.sentAtMs).toBe(START_MS);
  });

  it("is due after 12 hours: fresh delegations, and the record moves", async () => {
    h.now = START_MS + 12 * HOUR_MS;
    expect((await pass()).sent).toBe(1);
    expect(h.bytes).toHaveLength(1);
    const { delegates, expiresAtMs } = delegatesOf(h.bytes[0] as Uint8Array);
    expect(delegates).toEqual([hex(OWNER_KEY)]);
    // Fresh: 23 hours from now, not from the first send.
    expect(expiresAtMs).toEqual([BigInt(h.now + 23 * HOUR_MS)]);
    expect(record()?.envelope?.sentAtMs).toBe(h.now);

    // And the next one waits another 12 hours.
    await pass();
    h.now += 12 * HOUR_MS - 1;
    await pass();
    expect(h.sends).toBe(1);
    h.now += 1;
    await pass();
    expect(h.sends).toBe(2);
  });

  it("a clock that went backwards re-sends rather than letting delegations lapse", async () => {
    h.now = START_MS - 1;
    expect((await pass()).sent).toBe(1);
  });

  it("a new admin makes it due at once, and the fresh envelope covers them", async () => {
    h.admins = [NEW_ADMIN_KEY];
    h.now = START_MS + HOUR_MS;
    expect((await pass()).sent).toBe(1);
    expect(delegatesOf(h.bytes[0] as Uint8Array).delegates).toEqual([
      hex(OWNER_KEY),
      hex(NEW_ADMIN_KEY),
    ]);
    expect(record()?.envelope?.coveredHex).toEqual([hex(OWNER_KEY), hex(NEW_ADMIN_KEY)]);
    await pass();
    expect(h.sends).toBe(1);
  });

  it("an admin who left needs nothing", async () => {
    h.admins = [ADMIN_KEY];
    h.now = START_MS + 12 * HOUR_MS;
    await pass();
    h.admins = [];
    h.now += HOUR_MS;
    await pass();
    expect(h.sends).toBe(1);
  });

  it("two passes racing on one poll tail send one envelope", async () => {
    h.now = START_MS + 12 * HOUR_MS;
    await Promise.all([pass(), pass()]);
    expect(h.sends).toBe(1);
  });
});

describe("an open envelope (no delegation, the person is in Everyone)", () => {
  beforeEach(async () => {
    holdOpenEnvelopeForBob();
    await send();
    h.bytes.length = 0;
  });

  it("was noted as unsigned and never ages", async () => {
    expect(record()?.envelope).toEqual({
      sentAtMs: START_MS,
      signed: false,
      coveredHex: [hex(OWNER_KEY)],
    });
    h.now = START_MS + 1000 * HOUR_MS;
    await pass();
    expect(h.sends).toBe(0);
  });

  it("is re-sent only for a new candidate, still with no delegation", async () => {
    h.admins = [NEW_ADMIN_KEY];
    h.now = START_MS + HOUR_MS;
    expect((await pass()).sent).toBe(1);
    expect(delegatesOf(h.bytes[0] as Uint8Array).delegates).toEqual([]);
    expect(record()?.envelope).toEqual({
      sentAtMs: h.now,
      signed: false,
      coveredHex: [hex(OWNER_KEY), hex(NEW_ADMIN_KEY)],
    });
  });
});

describe("what never re-sends", () => {
  beforeEach(async () => {
    holdGrantForBob();
    await send();
    h.bytes.length = 0;
    h.now = START_MS + 13 * HOUR_MS;
  });

  it("an Add that landed", async () => {
    h.pendingAdd = false;
    h.roster = [{ signaturePublicKey: new Uint8Array(32).fill(0x77), leafIndex: 3 }];
    expect(await pass()).toEqual({ sent: 0, failed: 0, nothingToSend: 0 });
    expect(h.sends).toBe(0);
  });

  it("an Add a commit dropped: the re-request's to answer", async () => {
    h.pendingAdd = false;
    await pass();
    expect(h.sends).toBe(0);
  });

  it("a group this device left, or a torn read", async () => {
    await pass({ membershipEnded: true });
    await pass({ roster: null });
    expect(h.sends).toBe(0);
  });

  it("another device's Add: this device holds no record of it", async () => {
    forgetOwnRequest(OWN_ID_B64, GROUP_B64, `add:${bobKeyB64}`);
    expect(await pass()).toEqual({ sent: 0, failed: 0, nothingToSend: 0 });
    expect(h.sends).toBe(0);
  });

  it("a forgotten record takes the send with it, and a late note makes none", async () => {
    forgetOwnRequest(OWN_ID_B64, GROUP_B64, `add:${bobKeyB64}`);
    expect(loadOwnRequests(OWN_ID_B64, GROUP_B64).size).toBe(0);
    noteCommitterEnvelopeSent(OWN_ID_B64, GROUP_B64, bobKeyB64, {
      sentAtMs: h.now,
      signed: true,
      coveredHex: [],
    });
    expect(loadOwnRequests(OWN_ID_B64, GROUP_B64).size).toBe(0);
  });

  it("no other kind of request", () => {
    expect(
      ownAddIsPending({ request: { kind: "remove", subjectKeyB64: bobKeyB64 }, epoch: "5" }, snapshot()),
    ).toBe(false);
  });
});

describe("nothing to send", () => {
  it("is asked once, not on every poll, and again when a grant arrives", async () => {
    // Due (nothing sent yet), and no grant and no open envelope to send.
    expect(await pass()).toEqual({ sent: 0, failed: 0, nothingToSend: 1 });
    for (let i = 0; i < 5; i++) await pass();
    expect(h.sends).toBe(1);
    expect(h.bytes).toEqual([]);

    holdGrantForBob();
    expect((await pass()).sent).toBe(1);
    expect(h.sends).toBe(2);
  });

  it("a held grant whose binding names someone else does not loop either", async () => {
    const mallory = person(0x63);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      const g = wasm.deriveReachGrant(mallory.ed25519Seed, wasm.ReachClass.Contact, new Uint8Array(0), 0);
      store = fileContactGrant(store, hex(bob.signaturePublicKey), {
        wireB64: b64(wasm.encodeReachGrant(g)),
        version: 0,
        receivedAtMs: START_MS,
      });
      g.free();
      await pass();
      await pass();
      expect(h.sends).toBe(1);
      expect(h.bytes).toEqual([]);
    } finally {
      warn.mockRestore();
      mallory.free();
    }
  });

  it("a new candidate asks once more", async () => {
    await pass();
    h.admins = [NEW_ADMIN_KEY];
    await pass();
    await pass();
    expect(h.sends).toBe(2);
  });

  it("no reach session decides nothing", async () => {
    const summary = await runCommitterEnvelopeResends({
      accountIdB64: OWN_ID_B64,
      groupIdB64: GROUP_B64,
      snapshot: () => snapshot(),
      candidatesHex: async () => [],
      due: async () => true,
      send: async () => {
        h.sends += 1;
        return "sent";
      },
      grantsToken: () => null,
      nowMs: () => h.now,
    });
    expect(summary.sent).toBe(0);
    expect(h.sends).toBe(0);
  });
});

describe("through the contacts client", () => {
  const BOB_ACCOUNT = new Uint8Array(16).fill(0x22);
  const IDENTITY = { accountId: new Uint8Array(16).fill(0x11) } as unknown as IdentitySecrets;
  const ACCOUNT_B64 = b64(IDENTITY.accountId);

  function memberGroup(pending: () => boolean): WasmGroup {
    return {
      ...groupDouble(),
      groupId: GROUP_ID,
      epoch: 5n,
      proposeAddMember: () => new Uint8Array([1]),
      pendingProposals: () =>
        pending()
          ? [{ kind: "add", stored: true, addSignatureKey: new Uint8Array(32).fill(0x77) }]
          : [],
      pendingExternalJoins: () => new Uint8Array(0),
    } as unknown as WasmGroup;
  }

  function client(): InstanceType<typeof ContactsClient> {
    const keyPackage = new Uint8Array(64);
    keyPackage.set(bob.signaturePublicKey, 0);
    keyPackage.fill(0x77, 32);
    return new ContactsClient(
      {
        identityFetch: async () => null,
        fetchKeyPackages: async () => [{ deviceId: new Uint8Array(32).fill(0x77), keyPackage }],
      } as unknown as DiscoveryClient,
      withCommitRoute({ publishGroup: async () => ({ id: 7, createdAt: "2027-01-15T00:00:00Z" }) }) as unknown as DsClient,
      {
        identityProvider: () => ({ signaturePublicKey: new Uint8Array(32) }) as unknown as wasm.Identity,
        onPersist: async () => undefined,
        onPersistWithCommit: async () => undefined,
      } as unknown as ConstructorParameters<typeof ContactsClient>[2],
    );
  }

  it("the add records the first send, a poll tail keeps quiet, and 12 hours later it re-sends", async () => {
    holdGrantForBob();
    session.accountIdB64 = ACCOUNT_B64;
    try {
      let pending = true;
      const group = memberGroup(() => pending);
      const contacts = client();
      const outcome = await contacts.addMemberToGroup(
        IDENTITY,
        group,
        BOB_ACCOUNT,
        bob.signaturePublicKey,
      );
      expect(outcome.kind).toBe("proposed");
      expect(posted).toHaveLength(1);
      const records = loadOwnRequests(ACCOUNT_B64, GROUP_B64);
      expect(records.get(`add:${bobKeyB64}`)?.envelope).toEqual({
        sentAtMs: START_MS,
        signed: true,
        coveredHex: [hex(OWNER_KEY)],
      });

      await contacts.resendCommitterEnvelopes(IDENTITY, group, false);
      expect(posted).toHaveLength(1);

      h.now = START_MS + 12 * HOUR_MS;
      await contacts.resendCommitterEnvelopes(IDENTITY, group, false);
      expect(posted).toHaveLength(2);
      expect(loadOwnRequests(ACCOUNT_B64, GROUP_B64).get(`add:${bobKeyB64}`)?.envelope?.sentAtMs).toBe(h.now);

      // The Add landed: nothing further, however long it has been.
      pending = false;
      h.now += 24 * HOUR_MS;
      await contacts.resendCommitterEnvelopes(IDENTITY, group, false);
      expect(posted).toHaveLength(2);
    } finally {
      session.accountIdB64 = OWN_ID_B64;
    }
  });

  it("a pass that outlived its account sends nothing", async () => {
    holdGrantForBob();
    const other = { accountId: new Uint8Array(16).fill(0x33) } as unknown as IdentitySecrets;
    recordOwnRequest(b64(other.accountId), GROUP_B64, addRequest(), 5n, [DEVICE_KEY_B64]);
    await client().resendCommitterEnvelopes(other, memberGroup(() => true), false);
    expect(posted).toEqual([]);
  });
});
