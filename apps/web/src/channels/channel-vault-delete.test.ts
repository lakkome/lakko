// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0064 section 1 on web: the client deletes its channel vaults. The
 * DELETE is signed with the vault's write key over an empty body; deleting
 * one never throws (the channel is already deleted); before the account is
 * deleted every owned channel's vault is deleted, bounded in time, and a
 * session with no owned channel (a guest) deletes none.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm, wasmSync: () => wasm }));

import {
  CHANNEL_REF_KIND,
  deleteChannelVault,
  deleteOwnedChannelVaults,
  installOwnedChannelVaults,
  setVaultDeleterForTests,
} from "./channel-vault-head";
import { STORAGE_PROOF_HEADER, VaultClient, VaultHttpError } from "../storage/vault-client";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

afterEach(() => {
  setVaultDeleterForTests(null);
  installOwnedChannelVaults(null);
});

const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");

describe("the vault DELETE", () => {
  it("is signed with the vault's write key over an empty body, at the vault's path", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0xa1));
    const channelId = new Uint8Array(16).fill(0xa2);
    const keys = wasm.vaultKeys(owner);
    const ref = wasm.VaultRef.conversation(CHANNEL_REF_KIND, channelId);
    const seen: { url: string; method: string; proof: string | null; body: unknown }[] = [];
    const client = new VaultClient("/api/storage", async (url, init) => {
      seen.push({ url, method: init.method ?? "", proof: new Headers(init.headers).get(STORAGE_PROOF_HEADER), body: init.body });
      return new Response(null, { status: 204 });
    });
    const signer = {
      vaultId: () => keys.vaultId(ref),
      writePublicKey: () => keys.writePublicKey(ref),
      signRequest: (m: string, p: string, b: Uint8Array) => keys.signRequest(ref, m, p, b),
    };
    try {
      expect(await client.deleteVault(signer)).toBe("deleted");
      expect(seen).toHaveLength(1);
      expect(seen[0]?.method).toBe("DELETE");
      expect(seen[0]?.url).toBe(`/api/storage/vault/${hex(keys.vaultId(ref))}`);
      expect(seen[0]?.body).toBeUndefined();
      // The storage proof header the service checks.
      const proof = seen[0]?.proof ?? "";
      expect(proof.length).toBeGreaterThan(0);
      expect(proof).toMatch(/^v1\./);
      // A vault nobody claimed answers 401: nothing to delete.
      const absent = new VaultClient("/api/storage", async () => Response.json({ error: "vault-proof-refused" }, { status: 401 }));
      expect(await absent.deleteVault(signer)).toBe("absent");
      const broken = new VaultClient("/api/storage", async () => new Response(null, { status: 503 }));
      await expect(broken.deleteVault(signer)).rejects.toBeInstanceOf(VaultHttpError);
    } finally {
      ref.free();
      keys.free();
    }
  });
});

describe("deleting a channel's vault after the channel", () => {
  it("never throws: a failure is logged and the channel stays deleted", async () => {
    setVaultDeleterForTests(async () => {
      throw new Error("storage down");
    });
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0xb1));
    await expect(deleteChannelVault(owner, new Uint8Array(16).fill(1))).resolves.toBe(false);
  });

  it("asks for the channel's vault under the lineage identity", async () => {
    const asked: { identity: unknown; channelId: string }[] = [];
    setVaultDeleterForTests(async (identity, channelId) => {
      asked.push({ identity, channelId: hex(channelId) });
      return true;
    });
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0xb2));
    expect(await deleteChannelVault(owner, new Uint8Array(16).fill(2))).toBe(true);
    expect(asked).toEqual([{ identity: owner, channelId: "02".repeat(16) }]);
  });
});

describe("before the account is deleted", () => {
  it("deletes the vault of every owned channel", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0xc1));
    const asked: string[] = [];
    setVaultDeleterForTests(async (_i, channelId) => {
      asked.push(hex(channelId));
      return true;
    });
    installOwnedChannelVaults({ lineage: () => owner, ownedChannelIds: () => ["11".repeat(16), "22".repeat(16)] });
    expect(await deleteOwnedChannelVaults()).toBe(2);
    expect(asked.sort()).toEqual(["11".repeat(16), "22".repeat(16)]);
  });

  it("does not wait past its budget for a slow service, and a failure does not throw", async () => {
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0xc2));
    setVaultDeleterForTests((_i, channelId) =>
      channelId[0] === 0x11 ? new Promise<boolean>(() => undefined) : Promise.reject(new Error("refused")),
    );
    installOwnedChannelVaults({ lineage: () => owner, ownedChannelIds: () => ["11".repeat(16), "22".repeat(16)] });
    const started = Date.now();
    await expect(deleteOwnedChannelVaults(50)).resolves.toBe(0);
    expect(Date.now() - started).toBeLessThan(2_000);
  });

  it("a guest (no owned channel) or a session with no source deletes none", async () => {
    let calls = 0;
    setVaultDeleterForTests(async () => {
      calls += 1;
      return true;
    });
    expect(await deleteOwnedChannelVaults()).toBe(0);
    const owner = wasm.Identity.fromSeed(new Uint8Array(64).fill(0xc3));
    installOwnedChannelVaults({ lineage: () => owner, ownedChannelIds: () => [] });
    expect(await deleteOwnedChannelVaults()).toBe(0);
    expect(calls).toBe(0);
  });
});
