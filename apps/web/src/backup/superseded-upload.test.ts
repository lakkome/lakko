// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A device whose session auth refuses uploads no backup (base scenario 4b).
 *
 * Storage verifies the bearer statelessly (ADR-0073 residual), so until the
 * access token expires it accepts a write from a device whose account keys
 * were reset on another device. The ADR-0037 head mint is the upload's one
 * call to auth: when auth refuses the session there (`401`, among them
 * `key-generation-superseded`; or `403`), the upload stops. It used to fall
 * back to the unframed legacy envelope, which let a stale device overwrite
 * the account's backup. Any other mint failure keeps the soft-fail.
 */
import { WebDeviceCipher, type DeviceCipher } from "../auth/device-cipher";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));

import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import type { StorageClient } from "../storage/client";
import { BackupClient } from "./client";
import { BackupHeadMintRefusedError, headMintFailureAllowsLegacy } from "./head-mint";

const ACCOUNT_BYTES = new Uint8Array(16).fill(0x42);
const SEED = new Uint8Array(32).fill(7);
const SLOW_MS = 60_000;

let phrase: Uint8Array;
let deviceKey: DeviceCipher;

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

/** Auth answers the head mint with `status` and `body`. */
function authAnswers(status: number, body: string): void {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(body, { status, headers: { "content-type": "application/json" } })),
  );
}

async function quietly<T>(run: () => Promise<T>): Promise<T> {
  const warn = console.warn;
  console.warn = (): void => {};
  try {
    return await run();
  } finally {
    console.warn = warn;
  }
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
  const mnemonic = wasm.RecoveryMnemonic.generate();
  phrase = new TextEncoder().encode(mnemonic.phrase);
  mnemonic.free();
  deviceKey = new WebDeviceCipher(
    await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]),
  );
});

beforeEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = new FakeIndexedDb();
});

afterEach(() => {
  vi.unstubAllGlobals();
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
});

describe("the backup upload of a device auth refuses", () => {
  it("writes nothing when the head mint answers 401 key-generation-superseded", async () => {
    authAnswers(401, '{"error":"key-generation-superseded"}');
    const tier = recorder();
    const client = new BackupClient(tier.client);
    client.setSessionToken("stale-access-token");
    const result = await quietly(() => client.uploadNow(ACCOUNT_BYTES, SEED, phrase, deviceKey, []));
    expect(result.kind).toBe("transport-failed");
    expect(tier.blob()).toBeNull();
  }, SLOW_MS);

  it("writes nothing when the head mint answers 403", async () => {
    authAnswers(403, "");
    const tier = recorder();
    const client = new BackupClient(tier.client);
    client.setSessionToken("token");
    const result = await quietly(() => client.uploadNow(ACCOUNT_BYTES, SEED, phrase, deviceKey, []));
    expect(result.kind).toBe("transport-failed");
    expect(tier.blob()).toBeNull();
  }, SLOW_MS);

  it("keeps the soft-fail on a server error: the unframed envelope is written", async () => {
    authAnswers(503, "");
    const tier = recorder();
    const client = new BackupClient(tier.client);
    client.setSessionToken("token");
    const result = await quietly(() => client.uploadNow(ACCOUNT_BYTES, SEED, phrase, deviceKey, []));
    expect(result.kind).toBe("uploaded");
    const blob = tier.blob();
    expect(blob).not.toBeNull();
    const parsed = wasm.parseBackupBlob(blob as Uint8Array);
    try {
      expect(parsed.framed).toBe(false);
    } finally {
      parsed.free();
    }
  }, SLOW_MS);
});

describe("headMintFailureAllowsLegacy", () => {
  it("is false only for a refused session", () => {
    expect(headMintFailureAllowsLegacy(new BackupHeadMintRefusedError(401))).toBe(false);
    expect(headMintFailureAllowsLegacy(new BackupHeadMintRefusedError(403))).toBe(false);
    expect(headMintFailureAllowsLegacy(new Error("network"))).toBe(true);
    expect(headMintFailureAllowsLegacy(new Error("mintBackupHead: /auth/backup/head → 503"))).toBe(true);
  });
});
