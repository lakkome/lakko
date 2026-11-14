// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 on web: the rules of the device leaves (who is admitted, who is
 * retired, which device request an owner device takes, the admission mark,
 * decision A, the returning old device, the admission-seed register, the
 * rebuilt rotation inputs), each pure; and two flows driven through the REAL
 * wasm core: an owner device admits a sibling with one staged commit whose
 * Welcome the sibling joins with its OWN device identity, and a
 * co-publisher's device request crosses the PublisherGroup and is taken.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm, wasmSync: () => wasm }));

import {
  admissibleKeyPackages,
  admissionMarkVerdict,
  deviceRequestDue,
  DEVICE_REQUEST_RESEND_MS,
  epochOffsetAt,
  ownDeviceDiff,
  publisherLeaves,
  publisherWelcomeTargetsOf,
  mayActOnResetSignal,
  joinReplacingHeldGroup,
  replaceHeldGroupFor,
  refoundVerdict,
  returningDeviceVerdict,
  takeDeviceRequest,
  takesDeviceRequest,
} from "./publisher-devices";
import { reconcileAdmissionSeeds } from "./channel-admission-seed-register";
import { clearFoundedAlone, foundedAloneDone, isFoundedAlone, markFoundedAlone } from "./founded-alone-marker";
import {
  emptyAccessRecord,
  seedsToSend,
  admissionSeedId,
  channelEpochOfGroup,
  effectiveAdmissionMark,
  withRowsRead,
} from "./channel-access-record";
import { epochStartMsOf, rebuiltRotationState } from "./rotation-state-store";
import { keepsEpochSeeds } from "./publisher-handover";
import { rotationJitterMs, ROTATION_JITTER_MAX_MS } from "./ChannelPublisherLoops";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

function deviceOf(account: wasm.Identity, fill: number): wasm.Identity {
  return wasm.Identity.fromSeedBound(new Uint8Array(64).fill(fill), account.ed25519Seed, 1_700_000_000n);
}

const A = "aa".repeat(32);
const B = "bb".repeat(32);

describe("the owner's devices against the DeviceGroup roster (2.1)", () => {
  const leaves = [
    { leafKeyHex: "01", accountKeyHex: A },
    { leafKeyHex: "02", accountKeyHex: A },
    { leafKeyHex: "09", accountKeyHex: B },
  ];

  it("admits the roster devices with no leaf and retires the leaves that left, never itself", () => {
    expect(ownDeviceDiff({ accountKeyHex: A, leaves, rosterKeysHex: ["01", "03"], selfKeyHex: "01" })).toEqual({
      missing: ["03"],
      retire: ["02"],
    });
    // This device is never retired by itself, even off the roster.
    expect(ownDeviceDiff({ accountKeyHex: A, leaves, rosterKeysHex: ["02"], selfKeyHex: "01" }).retire).toEqual([]);
  });

  it("decides nothing without a roster, and never touches another account's leaves", () => {
    expect(ownDeviceDiff({ accountKeyHex: A, leaves, rosterKeysHex: null, selfKeyHex: "01" })).toEqual({ missing: [], retire: [] });
    expect(ownDeviceDiff({ accountKeyHex: A, leaves, rosterKeysHex: [], selfKeyHex: "01" })).toEqual({ missing: [], retire: [] });
    expect(ownDeviceDiff({ accountKeyHex: A, leaves, rosterKeysHex: ["01", "02"], selfKeyHex: "01" }).retire).toEqual([]);
  });

  it("adds a device only from a KeyPackage bound to the account whose leaf the roster named", () => {
    const kp = (n: number) => new Uint8Array([n]);
    const candidates = [
      { accountKeyHex: A, leafKeyHex: "03", keyPackage: kp(3) },
      { accountKeyHex: B, leafKeyHex: "04", keyPackage: kp(4) },
      { accountKeyHex: null, leafKeyHex: "05", keyPackage: kp(5) },
      { accountKeyHex: A, leafKeyHex: "06", keyPackage: kp(6) },
      { accountKeyHex: A, leafKeyHex: "03", keyPackage: kp(7) },
    ];
    expect(admissibleKeyPackages(candidates, A, ["03", "04", "05"]).map((k) => k[0])).toEqual([3]);
  });
});

describe("a device request on an owner device (2.2)", () => {
  const base = {
    deviceIsOwner: true,
    channelIdHex: "11",
    requestChannelHex: "11",
    ownerHex: A,
    memberAccountsHex: [A, B],
  };

  it("takes an admit from a current co-publisher whose KeyPackage binds that account", () => {
    expect(takesDeviceRequest({ ...base, action: "admit", senderHex: B, keyPackageAccountHex: B })).toBe(true);
  });

  it("refuses: not an owner device, another channel, a non-member, a foreign KeyPackage, the owner's own admit", () => {
    expect(takesDeviceRequest({ ...base, deviceIsOwner: false, action: "admit", senderHex: B, keyPackageAccountHex: B })).toBe(false);
    expect(takesDeviceRequest({ ...base, requestChannelHex: "12", action: "admit", senderHex: B, keyPackageAccountHex: B })).toBe(false);
    expect(takesDeviceRequest({ ...base, action: "admit", senderHex: "cc".repeat(32), keyPackageAccountHex: "cc".repeat(32) })).toBe(false);
    expect(takesDeviceRequest({ ...base, action: "admit", senderHex: B, keyPackageAccountHex: A })).toBe(false);
    expect(takesDeviceRequest({ ...base, action: "admit", senderHex: A, keyPackageAccountHex: A })).toBe(false);
    expect(takesDeviceRequest({ ...base, action: "admit", senderHex: undefined, keyPackageAccountHex: B })).toBe(false);
  });

  it("takes a retire only of a leaf of the sender's own account (the owner's own too)", () => {
    expect(takesDeviceRequest({ ...base, action: "retire", senderHex: B, leafAccountHex: B })).toBe(true);
    expect(takesDeviceRequest({ ...base, action: "retire", senderHex: A, leafAccountHex: A })).toBe(true);
    expect(takesDeviceRequest({ ...base, action: "retire", senderHex: B, leafAccountHex: A })).toBe(false);
    expect(takesDeviceRequest({ ...base, action: "retire", senderHex: B, leafAccountHex: null })).toBe(false);
  });

  it("is sent again only after the resend interval", () => {
    const sent = new Map([["k", 1_000]]);
    expect(deviceRequestDue(sent, "other", 1_000)).toBe(true);
    expect(deviceRequestDue(sent, "k", 1_000 + DEVICE_REQUEST_RESEND_MS - 1)).toBe(false);
    expect(deviceRequestDue(sent, "k", 1_000 + DEVICE_REQUEST_RESEND_MS)).toBe(true);
  });
});

describe("the admission mark (section 6, security pass)", () => {
  const base = { channelIdHex: "11", markChannelHex: "11", pendingSeq: 5, ownerHex: A };

  it("is taken only from the channel owner's account; a co-publisher's mark is ignored", () => {
    expect(admissionMarkVerdict({ ...base, throughSeq: 9n, senderHex: A })).toBe(9);
    // B is a current co-publisher: before the security pass its mark was kept.
    expect(admissionMarkVerdict({ ...base, throughSeq: 9n, senderHex: B })).toBeNull();
    expect(admissionMarkVerdict({ ...base, throughSeq: 9n, senderHex: B, ownerHex: null })).toBeNull();
    expect(admissionMarkVerdict({ ...base, throughSeq: 5n, senderHex: A })).toBeNull();
  });

  it("drops another channel's mark, a non-member's and an absurd one", () => {
    expect(admissionMarkVerdict({ ...base, markChannelHex: "12", throughSeq: 9n, senderHex: A })).toBeNull();
    expect(admissionMarkVerdict({ ...base, throughSeq: 9n, senderHex: "cc".repeat(32) })).toBeNull();
    expect(admissionMarkVerdict({ ...base, throughSeq: 2n ** 60n, senderHex: A })).toBeNull();
  });

  it("an owner mark above the rows this device has read is clamped, and counts further as it reads", () => {
    expect(effectiveAdmissionMark(3, 1_000, 40)).toBe(40);
    expect(effectiveAdmissionMark(50, 1_000, 40)).toBe(50);
    const owed = { ...emptyAccessRecord(), admittedThroughSeq: 3, markPendingSeq: 1_000, rowsReadThroughSeq: 40 };
    const read = withRowsRead(owed, 70);
    expect(read.rowsReadThroughSeq).toBe(70);
    expect(read.admittedThroughSeq).toBe(70);
    expect(withRowsRead({ ...owed, markPendingSeq: 60 }, 70).admittedThroughSeq).toBe(60);
    // No owner mark: reading moves nothing but the read position.
    expect(withRowsRead({ ...emptyAccessRecord(), admittedThroughSeq: 2 }, 70).admittedThroughSeq).toBe(2);
  });
});

describe("decision A and the returning device (2.5)", () => {
  const ok = {
    registerRole: "owner" as const,
    holdsGroup: false,
    foundedAlone: true,
    rosterKeysHex: ["01"],
    selfKeyHex: "01",
  };

  it("re-founds only an owned channel, on a device that founded its DeviceGroup alone and is alone", () => {
    expect(refoundVerdict(ok)).toBe(true);
    expect(refoundVerdict({ ...ok, registerRole: "co-publisher" })).toBe(false);
    expect(refoundVerdict({ ...ok, holdsGroup: true })).toBe(false);
    expect(refoundVerdict({ ...ok, foundedAlone: false })).toBe(false);
    // A device with siblings never re-founds.
    expect(refoundVerdict({ ...ok, rosterKeysHex: ["01", "02"] })).toBe(false);
    expect(refoundVerdict({ ...ok, rosterKeysHex: ["02"] })).toBe(false);
    expect(refoundVerdict({ ...ok, rosterKeysHex: null })).toBe(false);
  });

  it("the founded-alone marker stays until every owned channel is held here, not at the first conversation", () => {
    const data = new Map<string, string>();
    const st = {
      getItem: (k: string) => data.get(k) ?? null,
      setItem: (k: string, v: string) => void data.set(k, v),
      removeItem: (k: string) => void data.delete(k),
    };
    markFoundedAlone("acct", st);
    expect(isFoundedAlone("acct", st)).toBe(true);
    expect(isFoundedAlone("other", st)).toBe(false);
    const held = new Set(["c1"]);
    // One owned channel founded again, one not yet: kept.
    expect(foundedAloneDone(["c1", "c2"], (id) => held.has(id))).toBe(false);
    // The register not loaded yet: kept.
    expect(foundedAloneDone(null, () => true)).toBe(false);
    held.add("c2");
    expect(foundedAloneDone(["c1", "c2"], (id) => held.has(id))).toBe(true);
    expect(foundedAloneDone([], () => false)).toBe(true);
    clearFoundedAlone("acct", st);
    expect(isFoundedAlone("acct", st)).toBe(false);
  });

  it("an old device drops its group only after repeated refusals and a journal on another epoch", () => {
    expect(returningDeviceVerdict({ refusals: 2, groupEpoch: 7n, journalEpoch: 2 })).toBe(true);
    expect(returningDeviceVerdict({ refusals: 1, groupEpoch: 7n, journalEpoch: 2 })).toBe(false);
    expect(returningDeviceVerdict({ refusals: 3, groupEpoch: 7n, journalEpoch: 7 })).toBe(false);
    expect(returningDeviceVerdict({ refusals: 3, groupEpoch: 7n, journalEpoch: null })).toBe(false);
  });

  it("the epoch offset of a group founded again is what the service says, never below 0", () => {
    expect(epochOffsetAt(9, 1)).toBe(8);
    expect(epochOffsetAt(1, 3)).toBe(0);
    expect(channelEpochOfGroup({ ...emptyAccessRecord(), epochOffset: 8 }, 2n)).toBe(10);
    expect(channelEpochOfGroup(emptyAccessRecord(), 2n)).toBe(2);
  });
});

describe("admission seeds: two of one version coexist (open question 4)", () => {
  const s1 = { version: 1, seedB64: btoa("1".repeat(32)) };
  const s2a = { version: 2, seedB64: btoa("a".repeat(32)) };
  const s2b = { version: 2, seedB64: btoa("b".repeat(32)) };

  it("the register and the local seeds are merged by seed, the register's missing ones written", () => {
    const { merged, toWrite } = reconcileAdmissionSeeds([s1, s2a], [s2b]);
    expect(merged).toEqual([s1, s2a, s2b]);
    expect(toWrite).toEqual([s1, s2a]);
    expect(reconcileAdmissionSeeds([s2a], [s2a]).toWrite).toEqual([]);
  });

  it("a publisher is sent every seed of a version, not one per version", () => {
    const record = { ...emptyAccessRecord(), admissionSeeds: [s1, s2a, s2b] };
    const p = { accountKeyB64: "", copublisherKeyB64: "", accountIdHex: "", sentVersions: [], sentSeedIds: [admissionSeedId(s2a.seedB64)] };
    expect(seedsToSend(record, p)).toEqual([s1, s2b]);
    // An older record that sent version 2 when it had one seed still owes
    // the second seed of that version.
    const old = { accountKeyB64: "", copublisherKeyB64: "", accountIdHex: "", sentVersions: [1, 2] };
    expect(seedsToSend(record, old)).toEqual([s2a, s2b]);
  });
});

describe("rotation by any owner device (section 4)", () => {
  it("waits a random 0 to 60 s", () => {
    expect(rotationJitterMs(() => 0)).toBe(0);
    expect(rotationJitterMs(() => 0.5)).toBe(ROTATION_JITTER_MAX_MS / 2);
    expect(rotationJitterMs(() => 1)).toBeLessThan(ROTATION_JITTER_MAX_MS);
  });

  it("rebuilds the post count from the listed posts, never below the stored count", () => {
    const stored = { lastRotationMs: 10n, postsSinceRotation: 3 };
    expect(rebuiltRotationState(stored, 7).postsSinceRotation).toBe(7);
    expect(rebuiltRotationState(stored, 1).postsSinceRotation).toBe(3);
    expect(epochStartMsOf("2026-11-14T10:00:00Z")).toBe(Date.parse("2026-11-14T10:00:00Z"));
    expect(epochStartMsOf("not a time")).toBeNull();
  });
});

describe("the epoch seeds hand-over reaches an owner device too (section 6)", () => {
  it("is kept on an owner device when addressed to its own account and sent by the owner account", () => {
    expect(
      keepsEpochSeeds({
        deviceIsOwner: true,
        channelIdHex: "11",
        envelopeChannelHex: "11",
        memberAccountKeyB64: "own",
        ownAccountKeysB64: ["own"],
        senderB64: "own",
        ownerKeyB64: "own",
      }),
    ).toBe(true);
  });
});

describe("a co-publisher device joins only with the owner key the register recorded", () => {
  it("a co-publisher channel with no recorded owner key is no target; an Owner channel is", () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x71));
    const regs = [
      { channelIdHex: "01".repeat(16), subscribed: true, role: "owner" as const, publisherGroupIdHex: "", ownerAccountKey: new Uint8Array(0) },
      { channelIdHex: "02".repeat(16), subscribed: true, role: "co-publisher" as const, publisherGroupIdHex: "ab".repeat(32), ownerAccountKey: new Uint8Array(0) },
      { channelIdHex: "03".repeat(16), subscribed: true, role: "co-publisher" as const, publisherGroupIdHex: "cd".repeat(32), ownerAccountKey: new Uint8Array(32).fill(4) },
      { channelIdHex: "04".repeat(16), subscribed: false, role: "owner" as const, publisherGroupIdHex: "", ownerAccountKey: new Uint8Array(0) },
    ];
    const targets = publisherWelcomeTargetsOf(wasm, owner.ed25519Seed, regs, () => false);
    expect(targets.map((t) => [t.channelIdHex.slice(0, 2), t.role])).toEqual([
      ["01", "owner"],
      ["03", "co-publisher"],
    ]);
    expect(Array.from(targets[1]?.ownerKey ?? [])).toEqual(Array.from(new Uint8Array(32).fill(4)));
    expect(publisherWelcomeTargetsOf(wasm, owner.ed25519Seed, regs, () => true)).toEqual([]);
  });

  it("a Welcome from a device of another account is refused; one from the recorded owner lands", () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x72));
    const stranger = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x73));
    const cop = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x74));
    const ownerDevice = deviceOf(owner, 0x82);
    const strangerDevice = deviceOf(stranger, 0x83);
    const copDevice = deviceOf(cop, 0x84);
    const channelId = new Uint8Array(16).fill(0x75);
    const real = new wasm.PublisherGroup(ownerDevice, owner.ed25519Seed, channelId);
    const fake = new wasm.PublisherGroup(strangerDevice, stranger.ed25519Seed, channelId);
    const welcomeOf = (g: wasm.PublisherGroup, d: wasm.Identity): Uint8Array => {
      const req = g.retainJoinRequest(d, copDevice.createKeyPackage());
      const added = g.stageApproveExternalJoin(d, req.joinerSignatureKey);
      req.free();
      g.mergeStagedCommit(d);
      const w = added.welcome;
      added.free();
      return w;
    };
    try {
      const [target] = publisherWelcomeTargetsOf(
        wasm,
        cop.ed25519Seed,
        [
          {
            channelIdHex: hex(channelId),
            subscribed: true,
            role: "co-publisher",
            publisherGroupIdHex: hex(real.groupId()),
            ownerAccountKey: owner.signaturePublicKey,
          },
        ],
        () => false,
      );
      if (target === undefined) throw new Error("no target");
      // The stranger's group, under the stranger's own id and the register's
      // group id alike: refused, the owner key is never taken from the Welcome.
      const strangerWelcome = welcomeOf(fake, strangerDevice);
      expect(() => wasm.PublisherGroup.joinFromWelcome(copDevice, strangerWelcome, target.groupId, channelId, target.ownerKey)).toThrow();
      expect(() => wasm.PublisherGroup.joinFromWelcome(copDevice, strangerWelcome, fake.groupId(), channelId, target.ownerKey)).toThrow();
      const joined = wasm.PublisherGroup.joinFromWelcome(
        copDevice,
        welcomeOf(real, ownerDevice),
        target.groupId,
        channelId,
        target.ownerKey,
      );
      expect(Array.from(joined.channelSeed(copDevice))).toEqual(Array.from(real.channelSeed(ownerDevice)));
      joined.free();
    } finally {
      real.free();
      fake.free();
    }
  });
});

describe("a suspected reset never deletes a group on a DS signal alone (security pass)", () => {
  it("acts only on a device with siblings, never on a clock-skew refusal", () => {
    expect(mayActOnResetSignal({ rosterKeysHex: ["01", "02"], clockSkewed: false })).toBe(true);
    expect(mayActOnResetSignal({ rosterKeysHex: ["01"], clockSkewed: false })).toBe(false);
    expect(mayActOnResetSignal({ rosterKeysHex: null, clockSkewed: false })).toBe(false);
    expect(mayActOnResetSignal({ rosterKeysHex: ["01", "02"], clockSkewed: true })).toBe(false);
  });

  it("the held group stays for a Welcome not from the owner; an owner-signed Welcome for the same group replaces it", () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x91));
    const stranger = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x92));
    const oldDevice = deviceOf(owner, 0xa1);
    const newDevice = deviceOf(owner, 0xa2);
    const strangerDevice = deviceOf(stranger, 0xa3);
    const channelId = new Uint8Array(16).fill(0x93);
    // The old device's group (the chain it was on), and the group the
    // account's other device founded again at the same id.
    const old = new wasm.PublisherGroup(oldDevice, owner.ed25519Seed, channelId);
    const refounded = new wasm.PublisherGroup(newDevice, owner.ed25519Seed, channelId);
    const fake = new wasm.PublisherGroup(strangerDevice, stranger.ed25519Seed, channelId);
    old.free();
    const admit = (g: wasm.PublisherGroup, d: wasm.Identity): Uint8Array => {
      const c = g.stageCommit(d, [oldDevice.createKeyPackage()], new Uint8Array(), false);
      const w = c.welcome ?? new Uint8Array();
      c.free();
      return w;
    };
    const target = {
      channelIdHex: hex(channelId),
      groupId: wasm.publisherGroupIdForSeed(owner.ed25519Seed, channelId),
      ownerKey: wasm.publisherOwnerAccountKey(owner.ed25519Seed),
      role: "owner" as const,
    };
    try {
      // A stranger's Welcome: the held group stays.
      const strangerWelcome = (() => {
        const req = fake.retainJoinRequest(strangerDevice, oldDevice.createKeyPackage());
        const added = fake.stageApproveExternalJoin(strangerDevice, req.joinerSignatureKey);
        req.free();
        const w = added.welcome;
        added.free();
        return w;
      })();
      expect(replaceHeldGroupFor(wasm, oldDevice, owner, target, channelId, strangerWelcome)).toBeNull();
      expect(wasm.PublisherGroup.load(oldDevice, owner.ed25519Seed, channelId)).toBeDefined();
      // Garbage: stays too.
      expect(replaceHeldGroupFor(wasm, oldDevice, owner, target, channelId, new Uint8Array([1, 2, 3]))).toBeNull();
      expect(wasm.PublisherGroup.load(oldDevice, owner.ed25519Seed, channelId)).toBeDefined();
      // The owner's other device admits it: replaced, then joined.
      const welcome = admit(refounded, newDevice);
      refounded.mergeStagedCommit(newDevice);
      expect(replaceHeldGroupFor(wasm, oldDevice, owner, target, channelId, welcome)).not.toBeNull();
      const joined = wasm.PublisherGroup.joinFromWelcome(oldDevice, welcome, target.groupId, channelId, target.ownerKey);
      expect(Array.from(joined.channelSeed(oldDevice))).toEqual(Array.from(refounded.channelSeed(newDevice)));
      joined.free();
    } finally {
      refounded.free();
      fake.free();
    }
  });
});

describe("a replacing Welcome whose join fails (security pass follow-up)", () => {
  it("puts the held group back: it still loads with the seed it had", () => {
    vi.spyOn(console, "info").mockImplementation(() => undefined);
    vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x95));
    const oldDevice = deviceOf(owner, 0xb1);
    const newDevice = deviceOf(owner, 0xb2);
    const channelId = new Uint8Array(16).fill(0x96);
    const old = new wasm.PublisherGroup(oldDevice, owner.ed25519Seed, channelId);
    const seedBefore = Array.from(old.channelSeed(oldDevice));
    const epochBefore = old.epoch();
    old.free();
    const refounded = new wasm.PublisherGroup(newDevice, owner.ed25519Seed, channelId);
    const target = {
      channelIdHex: hex(channelId),
      groupId: wasm.publisherGroupIdForSeed(owner.ed25519Seed, channelId),
      ownerKey: wasm.publisherOwnerAccountKey(owner.ed25519Seed),
      role: "owner" as const,
    };
    try {
      const c = refounded.stageCommit(newDevice, [oldDevice.createKeyPackage()], new Uint8Array(), false);
      const welcome = c.welcome ?? new Uint8Array();
      c.free();
      refounded.mergeStagedCommit(newDevice);
      // The Welcome passes the peek (the owner's device, the same group id);
      // the core then refuses the join itself (here: asked for another group
      // id, which the join checks against the Welcome after staging it).
      const otherId = new Uint8Array(32).fill(0x07);
      const joined = joinReplacingHeldGroup(wasm, oldDevice, owner, target, channelId, welcome, () =>
        wasm.PublisherGroup.joinFromWelcome(oldDevice, welcome, otherId, channelId, target.ownerKey),
      );
      expect(joined).toBeNull();
      const kept = wasm.PublisherGroup.load(oldDevice, owner.ed25519Seed, channelId);
      expect(kept).toBeDefined();
      expect(kept?.epoch()).toBe(epochBefore);
      expect(Array.from(kept?.channelSeed(oldDevice) ?? [])).toEqual(seedBefore);
      kept?.free();
      // And the same Welcome, joined as it is, replaces it.
      const replaced = joinReplacingHeldGroup(wasm, oldDevice, owner, target, channelId, welcome);
      expect(replaced).not.toBeNull();
      expect(Array.from(replaced?.channelSeed(oldDevice) ?? [])).toEqual(Array.from(refounded.channelSeed(newDevice)));
      replaced?.free();
    } finally {
      refounded.free();
    }
  });
});

describe("through the real core", () => {
  it("an owner device admits a sibling in one staged commit; the sibling joins with its own device", () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x31));
    const first = deviceOf(owner, 0x41);
    const second = deviceOf(owner, 0x42);
    const channelId = new Uint8Array(16).fill(0x33);
    const g = new wasm.PublisherGroup(first, owner.ed25519Seed, channelId);
    try {
      const leaves = publisherLeaves(g);
      const diff = ownDeviceDiff({
        accountKeyHex: hex(g.ownerAccountKey()),
        leaves,
        rosterKeysHex: [hex(first.signaturePublicKey), hex(second.signaturePublicKey)],
        selfKeyHex: hex(first.signaturePublicKey),
      });
      expect(diff.missing).toEqual([hex(second.signaturePublicKey)]);
      const kp = second.createKeyPackage();
      const admit = admissibleKeyPackages(
        [{ accountKeyHex: hex(wasm.keyPackageAccountKey(kp) ?? new Uint8Array()), leafKeyHex: hex(wasm.keyPackageSignatureKey(kp)), keyPackage: kp }],
        hex(g.ownerAccountKey()),
        diff.missing,
      );
      const staged = g.stageCommit(first, admit, new Uint8Array(), false);
      const welcome = staged.welcome;
      staged.free();
      // Staged: still at N, the seed unchanged until the DS takes it.
      expect(g.hasStagedCommit()).toBe(true);
      expect(g.epoch()).toBe(0n);
      expect(g.mergeStagedCommit(first)).toBe(true);
      expect(welcome).toBeDefined();
      const joined = wasm.PublisherGroup.joinFromWelcome(
        second,
        welcome ?? new Uint8Array(),
        wasm.publisherGroupIdForSeed(owner.ed25519Seed, channelId),
        channelId,
        wasm.publisherOwnerAccountKey(owner.ed25519Seed),
      );
      expect(Array.from(joined.channelSeed(second))).toEqual(Array.from(g.channelSeed(first)));
      expect(joined.isOwnerDevice(second)).toBe(true);
      expect(g.isOwnedBy(owner.ed25519Seed)).toBe(true);
      joined.free();
    } finally {
      g.free();
    }
  });

  it("a co-publisher device's admit request crosses the group and an owner device takes it", () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x51));
    const cop = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x52));
    const ownerDevice = deviceOf(owner, 0x61);
    const copDevice = deviceOf(cop, 0x62);
    const copSecond = deviceOf(cop, 0x63);
    const channelId = new Uint8Array(16).fill(0x53);
    const g = new wasm.PublisherGroup(ownerDevice, owner.ed25519Seed, channelId);
    try {
      // The co-publisher's first device joins by an approve.
      const req = g.retainJoinRequest(ownerDevice, copDevice.createKeyPackage());
      const added = g.stageApproveExternalJoin(ownerDevice, req.joinerSignatureKey);
      req.free();
      expect(g.mergeStagedCommit(ownerDevice)).toBe(true);
      const copGroup = wasm.PublisherGroup.joinFromWelcome(copDevice, added.welcome, g.groupId(), channelId, owner.signaturePublicKey);
      added.free();
      // It asks for its second device.
      const envelope = wasm.composePublisherDeviceAdmit(channelId, copSecond.createKeyPackage(), wasm.signMailboxBinding(cop), []);
      const message = copGroup.createApplicationMessage(copDevice, envelope);
      const processed = g.processIncomingVerifyingRoles(ownerDevice, message);
      const plaintext = processed.applicationData ?? new Uint8Array();
      const sender = processed.senderAccount;
      processed.free();
      expect(hex(sender ?? new Uint8Array())).toBe(hex(cop.signaturePublicKey));
      expect(takeDeviceRequest(wasm, hex(channelId), g, ownerDevice, plaintext, sender)).toBe(true);
      // A plain chat message is not a device request.
      expect(takeDeviceRequest(wasm, hex(channelId), g, ownerDevice, new TextEncoder().encode("hello"), sender)).toBe(false);
      copGroup.free();
    } finally {
      g.free();
    }
  });
});
