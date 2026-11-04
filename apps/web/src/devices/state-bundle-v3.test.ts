// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * State bundle v3: group names, profiles and pending contacts reach a newly
 * paired device (2026-10-28). The codec, the bounds, and the receiving
 * decisions over the real wasm core's envelope codecs.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import * as wasm from "../../wasm/lakko_ffi_wasm";
import init from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { GroupNameStore, MEMORY_GROUP_NAME_BACKEND } from "../chat/group-name-store";
import { MemoryProfileBackend, ProfileStore } from "../profile/profile-store";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import {
  carriedGroupNamesOf,
  carriedProfilesOf,
  copiesOf,
  groupNamesToStore,
  offerCarriedNames,
  pendingContactOf,
  pendingContactsToAdd,
  takeCarriedGroupNames,
  takeCarriedProfiles,
} from "./carried-state";
import {
  MAX_CARRIED_GROUP_NAMES,
  MAX_CARRIED_HELD_MESSAGES,
  MAX_CARRIED_PENDING_CONTACTS,
  MAX_CARRIED_PROFILES,
  buildStateBundle,
  decodeStateBundle,
  encodeStateBundle,
  type CarriedPendingContact,
  type StateBundleV3,
} from "./state-bundle";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
});

const SENDER = "c2VuZGVyLWFjY291bnQtMDA=";
const key32 = (b: number): string => base64Encode(new Uint8Array(32).fill(b));
const GROUP_A = base64Encode(new Uint8Array(32).fill(0xa1));
const GROUP_B = base64Encode(new Uint8Array(32).fill(0xb2));
const encodeRaw = (v: unknown): Uint8Array => new TextEncoder().encode(JSON.stringify(v));

function nameEnvelope(name: string, stamp: bigint): string {
  return base64Encode(wasm.composeGroupNameEnvelope(name, stamp));
}

function profileEnvelope(displayName: string, stamp: bigint): string {
  return base64Encode(wasm.composeProfileEnvelope(displayName, undefined, stamp));
}

function pending(i: number, held = 0): CarriedPendingContact {
  const hex = i.toString(16).padStart(32, "0");
  return {
    accountIdHex: hex,
    signaturePubkeyB64: key32(i),
    x25519PubkeyB64: key32(i + 100),
    displayName: `VG${i}`,
    username: "",
    addedAtMs: 1_700_000_000_000 + i,
    linkIdB64: base64Encode(new Uint8Array(16).fill(i)),
    heldMessages: Array.from({ length: held }, (_, j) => ({
      text: `hello ${j}`,
      attachments: [],
      replyTo: null,
      heldAtMs: 1_700_000_000_000 + j,
    })),
  };
}

const empty: StateBundleV3 = {
  v: 3,
  contacts: [],
  pins: [],
  knownGroupIdsB64: [GROUP_A],
  ownPredecessors: [],
  groupNames: [],
  profiles: [],
  pendingContacts: [],
};

describe("state bundle v3 round trip", () => {
  it("carries a group name and a profile, and the new device stores them", async () => {
    const built = buildStateBundle([GROUP_A], SENDER, [], {
      groupNames: [{ groupIdB64: GROUP_A, envelopeB64: nameEnvelope("Test VG's", 1_000n) }],
      profiles: [{ personKeyB64: key32(5), envelopeB64: profileEnvelope("Carol", 2_000n) }],
      pendingContacts: [pending(7, 2)],
    });
    const decoded = decodeStateBundle(encodeStateBundle(built));
    expect(decoded).toEqual(built);
    if (decoded === null) throw new Error("unreachable");

    const names = carriedGroupNamesOf(decoded, wasm);
    expect(names.get(GROUP_A)).toEqual({ name: "Test VG's", updatedAtMs: 1_000n });

    const groupStore = new GroupNameStore(MEMORY_GROUP_NAME_BACKEND);
    await groupStore.open("acct");
    for (const [id, v] of groupNamesToStore(names, (k) => groupStore.get(k), wasm.groupNameIsNewer)) {
      await groupStore.set("acct", id, v);
    }
    expect(groupStore.nameOf(GROUP_A)).toBe("Test VG's");

    const people = carriedProfilesOf(decoded, wasm, () => false);
    const profileStore = new ProfileStore(new MemoryProfileBackend(), wasm.profileIsNewer);
    for (const [k, p] of people) await profileStore.ingest("acct", k, p);
    expect(profileStore.get(key32(5))?.displayName).toBe("Carol");
    expect(profileStore.get(key32(5))?.updatedAtMs).toBe(2_000n);

    // The pending contact and its held messages as copies.
    const local = pendingContactOf(decoded.pendingContacts[0] as CarriedPendingContact);
    expect(local.linkIdHex).toBe("07".repeat(16));
    expect(copiesOf(decoded.pendingContacts[0] as CarriedPendingContact).map((c) => c.text)).toEqual([
      "hello 0",
      "hello 1",
    ]);
  });

  it("the names reach a session store through the hand-off, once", () => {
    const names = new Map([[GROUP_A, { name: "G", updatedAtMs: 1n }]]);
    offerCarriedNames("acct-1", names, new Map());
    expect(takeCarriedProfiles("acct-1").size).toBe(0);
    expect(takeCarriedGroupNames("acct-2").size).toBe(0);
    expect(takeCarriedGroupNames("acct-1").get(GROUP_A)?.name).toBe("G");
    expect(takeCarriedGroupNames("acct-1").size).toBe(0);
  });
});

describe("state bundle v3 refusals", () => {
  it("refuses a v2 bundle", () => {
    const { groupNames: _g, profiles: _p, pendingContacts: _c, ...v2 } = empty;
    expect(decodeStateBundle(encodeRaw({ ...v2, v: 2 }))).toBeNull();
    expect(decodeStateBundle(encodeRaw({ ...empty, v: 2 }))).toBeNull();
    expect(decodeStateBundle(encodeRaw(empty))).toEqual(empty);
  });

  it("refuses a bundle missing any new array", () => {
    for (const field of ["groupNames", "profiles", "pendingContacts"] as const) {
      const copy: Record<string, unknown> = { ...empty };
      delete copy[field];
      expect(decodeStateBundle(encodeRaw(copy))).toBeNull();
    }
  });

  it("refuses the whole bundle over each bound", () => {
    const names = (n: number) =>
      Array.from({ length: n }, (_, i) => ({ groupIdB64: base64Encode(new Uint8Array([i >> 8, i & 255, 1])), envelopeB64: "AQ==" }));
    expect(decodeStateBundle(encodeRaw({ ...empty, groupNames: names(MAX_CARRIED_GROUP_NAMES) }))).not.toBeNull();
    expect(decodeStateBundle(encodeRaw({ ...empty, groupNames: names(MAX_CARRIED_GROUP_NAMES + 1) }))).toBeNull();

    const profiles = (n: number) =>
      Array.from({ length: n }, (_, i) => {
        const k = new Uint8Array(32);
        k[0] = i >> 8;
        k[1] = i & 255;
        return { personKeyB64: base64Encode(k), envelopeB64: "AQ==" };
      });
    expect(decodeStateBundle(encodeRaw({ ...empty, profiles: profiles(MAX_CARRIED_PROFILES) }))).not.toBeNull();
    expect(decodeStateBundle(encodeRaw({ ...empty, profiles: profiles(MAX_CARRIED_PROFILES + 1) }))).toBeNull();

    const contacts = (n: number) => Array.from({ length: n }, (_, i) => pending(i + 1));
    expect(
      decodeStateBundle(encodeRaw({ ...empty, pendingContacts: contacts(MAX_CARRIED_PENDING_CONTACTS) })),
    ).not.toBeNull();
    expect(
      decodeStateBundle(encodeRaw({ ...empty, pendingContacts: contacts(MAX_CARRIED_PENDING_CONTACTS + 1) })),
    ).toBeNull();

    expect(
      decodeStateBundle(encodeRaw({ ...empty, pendingContacts: [pending(1, MAX_CARRIED_HELD_MESSAGES)] })),
    ).not.toBeNull();
    expect(
      decodeStateBundle(encodeRaw({ ...empty, pendingContacts: [pending(1, MAX_CARRIED_HELD_MESSAGES + 1)] })),
    ).toBeNull();
  });

  it("refuses the whole bundle for a malformed or repeated entry", () => {
    const bad: unknown[] = [
      { ...empty, groupNames: [{ groupIdB64: GROUP_A }] },
      { ...empty, groupNames: [{ groupIdB64: GROUP_A, envelopeB64: "" }] },
      {
        ...empty,
        groupNames: [
          { groupIdB64: GROUP_A, envelopeB64: "AQ==" },
          { groupIdB64: GROUP_A, envelopeB64: "Ag==" },
        ],
      },
      { ...empty, profiles: [{ personKeyB64: base64Encode(new Uint8Array(31)), envelopeB64: "AQ==" }] },
      {
        ...empty,
        profiles: [
          { personKeyB64: key32(1), envelopeB64: "AQ==" },
          { personKeyB64: key32(1), envelopeB64: "AQ==" },
        ],
      },
      { ...empty, pendingContacts: [{ ...pending(1), accountIdHex: "XYZ" }] },
      { ...empty, pendingContacts: [{ ...pending(1), linkIdB64: base64Encode(new Uint8Array(8)) }] },
      { ...empty, pendingContacts: [pending(1), pending(1)] },
      {
        ...empty,
        pendingContacts: [{ ...pending(1), heldMessages: [{ text: "x", attachments: [], replyTo: null, heldAtMs: 1.5 }] }],
      },
      {
        ...empty,
        pendingContacts: [{ ...pending(1), heldMessages: [{ text: "x", attachments: {}, replyTo: null, heldAtMs: 1 }] }],
      },
      {
        ...empty,
        pendingContacts: [{ ...pending(1), heldMessages: [{ text: "x", attachments: [], replyTo: "7", heldAtMs: 1 }] }],
      },
    ];
    for (const b of bad) expect(decodeStateBundle(encodeRaw(b))).toBeNull();
  });
});

describe("state bundle v3 receiving decisions", () => {
  it("a newer stored group name is not overwritten by an older carried one", async () => {
    const decoded = decodeStateBundle(
      encodeStateBundle(
        buildStateBundle([GROUP_A, GROUP_B], SENDER, [], {
          groupNames: [
            { groupIdB64: GROUP_A, envelopeB64: nameEnvelope("Old", 1_000n) },
            { groupIdB64: GROUP_B, envelopeB64: nameEnvelope("Same", 3_000n) },
          ],
        }),
      ),
    );
    if (decoded === null) throw new Error("unreachable");
    const held = new Map([
      [GROUP_A, { name: "Renamed", updatedAtMs: 2_000n }],
      [GROUP_B, { name: "Same", updatedAtMs: 3_000n }],
    ]);
    const toStore = groupNamesToStore(carriedGroupNamesOf(decoded, wasm), (k) => held.get(k), wasm.groupNameIsNewer);
    expect(toStore.size).toBe(0);
  });

  it("a newer stored profile is not overwritten by an older carried one", async () => {
    const decoded = decodeStateBundle(
      encodeStateBundle(
        buildStateBundle([], SENDER, [], {
          profiles: [{ personKeyB64: key32(5), envelopeB64: profileEnvelope("Old Carol", 1_000n) }],
        }),
      ),
    );
    if (decoded === null) throw new Error("unreachable");
    const store = new ProfileStore(new MemoryProfileBackend(), wasm.profileIsNewer);
    await store.ingest("acct", key32(5), { displayName: "New Carol", username: null, updatedAtMs: 2_000n });
    for (const [k, p] of carriedProfilesOf(decoded, wasm, () => false)) {
      expect(await store.ingest("acct", k, p)).toBe(false);
    }
    expect(store.get(key32(5))?.displayName).toBe("New Carol");
  });

  it("a profile under this account's own key is never taken", () => {
    const bundle = {
      profiles: [
        { personKeyB64: key32(1), envelopeB64: profileEnvelope("Me", 5n) },
        { personKeyB64: key32(2), envelopeB64: profileEnvelope("Bob", 5n) },
      ],
    };
    const taken = carriedProfilesOf(bundle, wasm, (k) => k === key32(1));
    expect([...taken.keys()]).toEqual([key32(2)]);
  });

  it("an entry that does not decode is skipped, not the rest", () => {
    const names = carriedGroupNamesOf(
      {
        groupNames: [
          { groupIdB64: GROUP_A, envelopeB64: "AQID" },
          { groupIdB64: GROUP_B, envelopeB64: nameEnvelope("Fine", 1n) },
        ],
      },
      wasm,
    );
    expect([...names.keys()]).toEqual([GROUP_B]);
  });

  it("a pending contact held in any form is not added again", () => {
    const carried = [pending(1), pending(2), pending(3), pending(4)];
    const toAdd = pendingContactsToAdd(
      carried,
      [
        { signaturePubkeyB64: key32(1) },
        { signaturePubkeyB64: key32(99), accountIdHex: (2).toString(16).padStart(32, "0") },
      ],
      [pendingContactOf(pending(3))],
    );
    expect(toAdd.map((c) => c.accountIdHex)).toEqual([(4).toString(16).padStart(32, "0")]);
  });
});

describe("state bundle v3 building", () => {
  it("keeps one name per known group in the known order, and the first N", () => {
    const built = buildStateBundle([GROUP_B, GROUP_A], SENDER, [], {
      groupNames: [
        { groupIdB64: GROUP_A, envelopeB64: "AQ==" },
        { groupIdB64: GROUP_A, envelopeB64: "Ag==" },
        { groupIdB64: GROUP_B, envelopeB64: "Aw==" },
        { groupIdB64: key32(9), envelopeB64: "BA==" },
      ],
    });
    expect(built.groupNames).toEqual([
      { groupIdB64: GROUP_B, envelopeB64: "Aw==" },
      { groupIdB64: GROUP_A, envelopeB64: "AQ==" },
    ]);
  });

  it("orders profiles by key, one per key, bounded", () => {
    const profiles = Array.from({ length: MAX_CARRIED_PROFILES + 5 }, (_, i) => {
      const k = new Uint8Array(32);
      k[0] = 255 - (i >> 8);
      k[1] = 255 - (i & 255);
      return { personKeyB64: base64Encode(k), envelopeB64: "AQ==" };
    });
    const built = buildStateBundle([], SENDER, [], { profiles: [...profiles, profiles[0]!] });
    expect(built.profiles).toHaveLength(MAX_CARRIED_PROFILES);
    const keys = built.profiles.map((p) => p.personKeyB64);
    expect([...keys].sort()).toEqual(keys);
    expect(decodeStateBundle(encodeStateBundle(built))).not.toBeNull();
  });

  it("keeps the first held messages of a pending contact", () => {
    const built = buildStateBundle([], SENDER, [], {
      pendingContacts: [pending(1, MAX_CARRIED_HELD_MESSAGES + 3)],
    });
    const held = built.pendingContacts[0]?.heldMessages ?? [];
    expect(held).toHaveLength(MAX_CARRIED_HELD_MESSAGES);
    expect(held[0]?.text).toBe("hello 0");
  });

  it("stays under the size cap by dropping profiles, never contacts", () => {
    const big = base64Encode(new Uint8Array(600));
    const profiles = Array.from({ length: MAX_CARRIED_PROFILES }, (_, i) => {
      const k = new Uint8Array(32);
      k[0] = i >> 8;
      k[1] = i & 255;
      return { personKeyB64: base64Encode(k), envelopeB64: big };
    });
    const built = buildStateBundle([], SENDER, [], { profiles });
    expect(built.profiles.length).toBeLessThan(MAX_CARRIED_PROFILES);
    expect(built.profiles.length).toBeGreaterThan(0);
    expect(decodeStateBundle(encodeStateBundle(built))).not.toBeNull();
  });
});
