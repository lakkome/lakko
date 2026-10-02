// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The account backup carries no MLS state, and a restore never loads any
 * (2026-10-02, payload v6).
 *
 * Two halves:
 *
 *   - through the real wasm core: `BackupClient` composes a payload from
 *     contacts and conversation ids alone, the parsed payload has no
 *     snapshot field, and a restore of that blob hands back contacts and
 *     ids and nothing a provider could be loaded from;
 *   - on the source: no backup module and no auth flow feeds a backup into
 *     `restoreStorage`, takes a snapshot for one, or writes the MLS snapshot
 *     store from one. `AuthContext.tsx` needs a Solid runtime this suite
 *     does not have, so its half is pinned on its source.
 *
 * Not reachable here: the server (the storage tier is a recorder), and the
 * ADR-0037 head on the upload (no auth service to mint one, so the blob is
 * written unframed, which is the documented soft-fail; the gate then takes
 * its fresh-install fail-open branch).
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));

import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import { base64Encode } from "../auth/account-id";
import { loadAccepted, upsertAccepted } from "../contacts/local-store";
import type { StorageClient } from "../storage/client";
import { BackupClient } from "./client";

const ACCOUNT_BYTES = new Uint8Array(16).fill(0x42);
const ACCOUNT = base64Encode(ACCOUNT_BYTES);
const SEED = new Uint8Array(32).fill(7);
/** The backup KEK is Argon2id over the phrase, and the dev wasm build is
 *  unoptimised: each seal or open is seconds, so these get room. */
const SLOW_MS = 60_000;
const G1 = base64Encode(new Uint8Array(32).fill(0xa1));
const G2 = base64Encode(new Uint8Array(32).fill(0xa2));

let phrase: Uint8Array;
let deviceKey: CryptoKey;

/** A storage tier that keeps the one blob it was given. */
function recorder(): { client: StorageClient; blob: () => Uint8Array | null } {
  let blob: Uint8Array | null = null;
  const client = {
    setSessionToken: (): void => {},
    putBackup: async (bytes: Uint8Array): Promise<void> => {
      blob = bytes;
    },
    getBackup: async (): Promise<Uint8Array | null> => blob,
  } as unknown as StorageClient;
  return { client, blob: () => blob };
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
  const mnemonic = wasm.RecoveryMnemonic.generate();
  phrase = new TextEncoder().encode(mnemonic.phrase);
  mnemonic.free();
  deviceKey = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
    "encrypt",
    "decrypt",
  ]);
});

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
  // No auth service: the head mint fails and the blob is written unframed.
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => {
      throw new Error("no network in this test");
    }),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
});

/** Run with the expected soft-fail warnings kept off the test output. */
async function quietly<T>(run: () => Promise<T>): Promise<T> {
  const warn = console.warn;
  console.warn = (): void => {};
  try {
    return await run();
  } finally {
    console.warn = warn;
  }
}

describe("the backup payload (real wasm core)", () => {
  it("is composed from contacts and conversation ids, and parses with no snapshot", async () => {
    upsertAccepted(
      {
        signaturePubkeyB64: base64Encode(new Uint8Array(32).fill(1)),
        x25519PubkeyB64: base64Encode(new Uint8Array(32).fill(2)),
        displayName: "Peer",
        acceptedAtMs: 5,
      },
      ACCOUNT,
    );
    const tier = recorder();
    const client = new BackupClient(tier.client);
    const uploaded = await quietly(() =>
      client.uploadNow(ACCOUNT_BYTES, SEED, phrase, deviceKey, [G1, G2]),
    );
    expect(uploaded.kind).toBe("uploaded");

    const blob = tier.blob();
    expect(blob).not.toBeNull();
    const parsedBlob = wasm.parseBackupBlob(blob as Uint8Array);
    const parsed = wasm.parseBackupPayload(wasm.openBackup(phrase, parsedBlob.envelope));
    // The parsed payload exposes exactly these fields. A snapshot is not
    // one of them, under any name.
    const fields = Object.getOwnPropertyNames(Object.getPrototypeOf(parsed)).filter(
      (n) => n !== "constructor" && n !== "free" && !n.startsWith("__"),
    );
    expect(fields.filter((n) => /snapshot|storage/i.test(n))).toEqual([]);
    expect((parsed as unknown as Record<string, unknown>)["wholeStorageSnapshot"]).toBeUndefined();
    expect(parsed.accountId).toEqual(ACCOUNT_BYTES);
    expect(parsed.ed25519Seed).toEqual(SEED);
    expect(parsed.contacts).toHaveLength(1);
    // The ids ride in the position the snapshot used to hold: both are
    // `Uint8Array`, so a call site that kept the old order would have put
    // them into the wrong field without a type error.
    expect(parsed.knownGroupIds).toHaveLength(64);
    expect(base64Encode(parsed.knownGroupIds.subarray(0, 32))).toBe(G1);
    expect(base64Encode(parsed.knownGroupIds.subarray(32, 64))).toBe(G2);
    expect(parsed.syncedState).toHaveLength(0);
    parsed.free();
    parsedBlob.free();
  }, SLOW_MS);

  it("a restore hands back contacts and conversation ids, and nothing to load a provider from", async () => {
    const peer = base64Encode(new Uint8Array(32).fill(1));
    upsertAccepted(
      {
        signaturePubkeyB64: peer,
        x25519PubkeyB64: base64Encode(new Uint8Array(32).fill(2)),
        displayName: "Peer",
        acceptedAtMs: 5,
      },
      ACCOUNT,
    );
    const tier = recorder();
    const client = new BackupClient(tier.client);
    await quietly(() => client.uploadNow(ACCOUNT_BYTES, SEED, phrase, deviceKey, [G1, G2]));

    // A fresh device: nothing local.
    (globalThis as unknown as Record<string, unknown>)["localStorage"] = new MemoryStorage();
    expect(loadAccepted(ACCOUNT)).toEqual([]);

    const result = await quietly(() =>
      client.restore(phrase, { accountId: ACCOUNT_BYTES, accountIdB64: ACCOUNT, deviceKey }),
    );
    expect(result.kind).toBe("restored");
    if (result.kind !== "restored") return;
    expect(Object.keys(result).sort()).toEqual([
      "acceptedContacts",
      "accountId",
      "createdAtMs",
      "ed25519Seed",
      "kind",
      "knownGroupIds",
      "syncedState",
    ]);
    expect(result.acceptedContacts.map((c) => c.signaturePubkeyB64)).toEqual([peer]);
    expect(result.knownGroupIds).toEqual([G1, G2]);
  }, SLOW_MS);

  it("a missing backup is `no-backup`, and a wrong phrase restores nothing", async () => {
    const tier = recorder();
    const client = new BackupClient(tier.client);
    const gate = { accountId: ACCOUNT_BYTES, accountIdB64: ACCOUNT, deviceKey };
    expect((await client.restore(phrase, gate)).kind).toBe("no-backup");

    await quietly(() => client.uploadNow(ACCOUNT_BYTES, SEED, phrase, deviceKey, [G1]));
    const other = wasm.RecoveryMnemonic.generate();
    const otherPhrase = new TextEncoder().encode(other.phrase);
    other.free();
    const refused = await quietly(() => client.restore(otherPhrase, gate));
    expect(refused.kind).toBe("wrong-phrase-or-tampered");
  }, SLOW_MS);
});

describe("no path from a backup to MLS state (source)", () => {
  const src = fileURLToPath(new URL("..", import.meta.url));
  /** Code only: comment lines out, so a doc that explains the rule does not
   *  read as a breach of it. */
  const code = (path: string): string =>
    readFileSync(path, "utf8")
      .split("\n")
      .filter((l) => !/^\s*(\*|\/\/|\/\*)/.test(l))
      .join("\n");

  it("no backup module takes, carries or loads a provider snapshot", () => {
    const dir = join(src, "backup");
    const files = readdirSync(dir).filter((f) => /\.ts$/.test(f) && !/\.test\.ts$/.test(f));
    expect(files.length).toBeGreaterThan(5);
    for (const f of files) {
      const text = code(join(dir, f));
      expect(text, f).not.toMatch(
        /restoreStorage|snapshotStorage|wholeStorageSnapshot|saveMlsSnapshot|loadMlsSnapshot/,
      );
    }
  });

  it("the auth flows never feed a backup into the MLS provider", () => {
    const text = code(join(src, "auth", "AuthContext.tsx"));
    // No direct provider load or snapshot at all in the auth flows: the
    // device's OWN persisted state comes back through `rehydrateMlsState`.
    expect(text).not.toMatch(/\.restoreStorage\(/);
    expect(text).not.toMatch(/\.snapshotStorage\(/);
    expect(text).not.toMatch(/wholeStorageSnapshot|saveMlsSnapshot\(/);
    expect(text).toMatch(/rehydrateMlsState\(chats, /);
  });

  it("the only `buildBackupPayload` call passes six arguments, the ids fifth", () => {
    const text = code(join(src, "backup", "client.ts"));
    const calls = text.match(/wasm\.buildBackupPayload\(([^;]*?)\);/gs) ?? [];
    expect(calls).toHaveLength(1);
    const args = (calls[0] as string)
      .replace(/^wasm\.buildBackupPayload\(/, "")
      .replace(/\);$/, "")
      .split(",")
      .map((a) => a.trim())
      .filter((a) => a.length > 0);
    expect(args).toEqual([
      "accountId",
      "BigInt(Date.now())",
      "ed25519Seed",
      "contacts",
      "flatGroupIds",
      "currentSyncedStateBytes() ?? undefined",
    ]);
    // And nowhere else in the app.
    const others: string[] = [];
    const walk = (dir: string): void => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const p = join(dir, entry.name);
        if (entry.isDirectory()) walk(p);
        else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)) {
          if (p.endsWith(join("backup", "client.ts"))) continue;
          // The cheap test first: almost no file mentions it at all.
          const raw = readFileSync(p, "utf8");
          if (raw.includes("buildBackupPayload") && /buildBackupPayload\(/.test(code(p))) {
            others.push(p);
          }
        }
      }
    };
    walk(src);
    expect(others).toEqual([]);
  }, SLOW_MS);
});
