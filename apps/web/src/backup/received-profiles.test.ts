// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Payload v8 — the names of the people this device knows travel in the
 * backup and come back through the profile store's ordinary receive gate.
 *
 * Writing side: the REAL profile store over the REAL encrypted IndexedDB
 * backend (`FakeIndexedDb`, a WebCrypto device key), read back by
 * `receivedProfilesForBackup` as the upload reads it, through the REAL core's
 * `buildBackupPayload` / `parseBackupPayload`. Restoring side: the offer the
 * profile runtime takes, filed through `ProfileStore.ingest`.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { WebDeviceCipher } from "../auth/device-cipher";
import { takeCarriedProfiles } from "../devices/carried-state";
import type { CarriedProfile } from "../devices/state-bundle";
import { sniffProfile } from "../profile/profile-envelope";
import { IdbProfileBackend } from "../profile/profile-idb";
import { MemoryProfileBackend, ProfileStore } from "../profile/profile-store";
import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import type { MessageLogPayload } from "../chat/message-log-store";
import type { Wasm } from "../wasm";
import { receivedProfilesForBackup, receivedProfilesOfParsed } from "./client";
import {
  lastWrittenByAuthor,
  offerRestoredProfiles,
  orderReceivedProfiles,
  restoredProfilesFor,
  withRestoredProfiles,
} from "./received-profiles";

const core = wasm as unknown as Wasm;
const ACCOUNT = "QkJCQkJCQkJCQkJCQkJCQg==";
const ACCOUNT_BYTES = base64Decode(ACCOUNT);
const key = (b: number): string => base64Encode(new Uint8Array(32).fill(b));
const CONTACT = key(0xc1);
const RECENT = key(0xa1);
const OLDER = key(0xa2);
const NEVER = key(0xa3);
/** The account identity's 64-byte phrase seed; the backup carries its
 *  32-byte Ed25519 seed. */
const OWN_SEED = new Uint8Array(64).fill(3);
function ownEd25519Seed(): Uint8Array {
  const id = wasm.Identity.fromSeed(OWN_SEED);
  try {
    return id.ed25519Seed;
  } finally {
    id.free();
  }
}
const isNewer = (a: bigint, b: bigint): boolean => wasm.profileIsNewer(a, b);

let deviceKey: WebDeviceCipher;

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

beforeEach(async () => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
  deviceKey = new WebDeviceCipher(
    await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]),
  );
});
afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
});

function received(name: string, stamp: bigint) {
  const sniff = sniffProfile(core, wasm.composeProfileEnvelope(name, undefined, stamp));
  if (sniff.kind !== "profile") throw new Error("fixture");
  return sniff.profile;
}

function ownKeyB64(): string {
  const id = wasm.Identity.fromSeed(OWN_SEED);
  try {
    return base64Encode(id.signaturePublicKey);
  } finally {
    id.free();
  }
}

/** The log the order reads: RECENT wrote at t=200, OLDER at t=100. */
const LOG = {
  version: 1,
  groups: {
    g: {
      cursor: "0",
      messages: [
        { messageId: "1", direction: "incoming", text: "", attachments: [], createdAt: new Date(100).toISOString(), authorKeyB64: OLDER },
        { messageId: "2", direction: "incoming", text: "", attachments: [], createdAt: new Date(200).toISOString(), authorKeyB64: RECENT },
        { messageId: "3", direction: "outgoing", text: "", attachments: [], createdAt: new Date(900).toISOString(), authorKeyB64: NEVER },
      ],
    },
  },
} as unknown as MessageLogPayload;

/** The sending device: profiles filed by the store, persisted encrypted. */
async function writingDevice(): Promise<void> {
  const store = new ProfileStore(new IdbProfileBackend(async () => deviceKey, () => new Map()), isNewer);
  await store.ingest(ACCOUNT, NEVER, received("Never Wrote", 5n));
  await store.ingest(ACCOUNT, OLDER, received("Older", 5n));
  await store.ingest(ACCOUNT, RECENT, received("Recent", 5n));
  await store.ingest(ACCOUNT, CONTACT, received("Contact", 5n));
  await store.ingest(ACCOUNT, ownKeyB64(), received("Me", 5n));
  // Let the serialized write chain land.
  for (let i = 0; i < 20; i += 1) await new Promise((r) => setTimeout(r, 0));
}

async function listed(): Promise<CarriedProfile[]> {
  return receivedProfilesForBackup(
    core,
    ACCOUNT,
    deviceKey,
    { contactKeysB64: [CONTACT], ownEd25519Seed: ownEd25519Seed(), ownPredecessorSeeds: [] },
    undefined,
    async () => LOG,
  );
}

describe("what a backup lists of the people this device knows", () => {
  it("contacts first, then by how recently they wrote, the own account never", async () => {
    await writingDevice();
    const out = await listed();
    expect(out.map((p) => p.personKeyB64)).toEqual([CONTACT, RECENT, OLDER, NEVER]);
    expect(out.some((p) => p.personKeyB64 === ownKeyB64())).toBe(false);
  });

  it("the own key the upload leaves out is the account identity's signature key", () => {
    expect(base64Encode(wasm.linkRevocationPublicKey(ownEd25519Seed()))).toBe(ownKeyB64());
  });

  it("lastWrittenByAuthor reads incoming rows only, newest per author", () => {
    const m = lastWrittenByAuthor(LOG);
    expect(m.get(RECENT)).toBe(200);
    expect(m.get(OLDER)).toBe(100);
    expect(m.has(NEVER)).toBe(false);
  });

  it("orderReceivedProfiles is stable among equals", () => {
    const p = (k: string): CarriedProfile => ({ personKeyB64: k, envelopeB64: "" });
    expect(
      orderReceivedProfiles([p("x"), p("y"), p("z")], new Set(["z"]), new Map(), new Set(["y"])).map(
        (e) => e.personKeyB64,
      ),
    ).toEqual(["z", "x"]);
  });
});

describe("the round trip through the payload and the profile store", () => {
  it("every name comes back; a newer profile already held stays", async () => {
    await writingDevice();
    const list = await listed();
    const payload = wasm.buildBackupPayload(
      ACCOUNT_BYTES,
      1n,
      ownEd25519Seed(),
      [],
      new Uint8Array(0),
      undefined,
      undefined,
      undefined,
      undefined,
      undefined,
      list.map((p) => new wasm.BackupReceivedProfile(base64Decode(p.personKeyB64), base64Decode(p.envelopeB64))),
    );
    const parsed = wasm.parseBackupPayload(payload);
    const restored = receivedProfilesOfParsed(parsed.receivedProfiles);
    // The core keeps the head of the list that fits, stored sorted by key.
    expect(restored.map((p) => p.personKeyB64).sort()).toEqual([CONTACT, RECENT, OLDER, NEVER].sort());

    // The restoring device already learned a newer profile for RECENT.
    const store = new ProfileStore(new MemoryProfileBackend(), isNewer);
    await store.ingest(ACCOUNT, RECENT, received("Recent, renamed", 9n));

    expect(offerRestoredProfiles(ACCOUNT, restored, core)).toBe(4);
    // What the profile runtime does with the offer (`ProfileContext.tsx`).
    for (const [k, profile] of takeCarriedProfiles(ACCOUNT)) await store.ingest(ACCOUNT, k, profile);

    expect(store.get(CONTACT)?.displayName).toBe("Contact");
    expect(store.get(OLDER)?.displayName).toBe("Older");
    expect(store.get(NEVER)?.displayName).toBe("Never Wrote");
    expect(store.get(RECENT)?.displayName).toBe("Recent, renamed");
  });

  it("an upload before the store filed the restore still carries it", () => {
    const a: CarriedProfile = { personKeyB64: CONTACT, envelopeB64: "AA==" };
    const b: CarriedProfile = { personKeyB64: OLDER, envelopeB64: "AQ==" };
    offerRestoredProfiles(ACCOUNT, [a, b], core);
    expect(restoredProfilesFor(ACCOUNT)).toEqual([a, b]);
    const stored: CarriedProfile = { personKeyB64: CONTACT, envelopeB64: "Ag==" };
    expect(withRestoredProfiles([stored], restoredProfilesFor(ACCOUNT))).toEqual([stored, b]);
  });
});
