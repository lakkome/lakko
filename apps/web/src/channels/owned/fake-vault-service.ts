// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Test support: the vault routes of `lakko-svc-storage` in memory, behind
 * the real `VaultClient` (its `fetch` replaced), so the owned-channels tests
 * drive the same requests the app sends.
 *
 * What it enforces, as the service does: a vault is claimed under one write
 * key (`409 vault-claimed` under another); a head write is taken only at the
 * current version + 1, the first at 1 (`409 vault-version-conflict`
 * otherwise); every head and delete request carries a storage proof; a vault
 * nobody claimed answers `401`. It does not verify the proof's signature
 * (the client's signing is covered by `channel-vault-delete.test.ts`).
 *
 * Not imported by the app.
 */
import { STORAGE_PROOF_HEADER, VaultClient } from "../../storage/vault-client";

interface StoredVault {
  writePk: string;
  version: bigint;
  /** `version ‖ sealed`, as the service stores and returns it. */
  body: Uint8Array | null;
}

/** One request the service saw. */
export interface SeenRequest {
  readonly method: string;
  readonly path: string;
}

function json(status: number, error?: string): Response {
  return new Response(error === undefined ? null : JSON.stringify({ error }), {
    status,
    headers: error === undefined ? {} : { "content-type": "application/json" },
  });
}

async function bodyBytes(init: RequestInit): Promise<Uint8Array> {
  const b = init.body;
  if (b === undefined || b === null) return new Uint8Array(0);
  if (b instanceof Uint8Array) return new Uint8Array(b);
  if (b instanceof ArrayBuffer) return new Uint8Array(b);
  return new Uint8Array(await new Response(b).arrayBuffer());
}

/** The in-memory storage service. */
export class FakeVaultService {
  readonly vaults = new Map<string, StoredVault>();
  readonly seen: SeenRequest[] = [];
  /** Claims taken (each spends one `vault-claim` token in the app). */
  claims = 0;
  /** When set, every request fails as an offline network does. */
  offline = false;
  /** Runs before a head write is judged: a test writes "from another
   *  device" here to force a version conflict. */
  beforeHeadWrite: ((vaultIdHex: string) => void) | null = null;
  /** Fail the next `n` requests whose path matches, as the network does. */
  private failures: { match: (r: SeenRequest) => boolean; left: number }[] = [];

  /** The client the store under test talks through. */
  client(): VaultClient {
    return new VaultClient(
      "/storage",
      (url, init) => this.handle(url, init),
      { spend: (_context, send) => send("x-lakko-anon-token", "token") },
    );
  }

  /** Fail the next `count` requests matching `match` with a network error. */
  failNext(match: (r: SeenRequest) => boolean, count = 1): void {
    this.failures.push({ match, left: count });
  }

  /** How many vaults hold a head. */
  heads(): number {
    return [...this.vaults.values()].filter((v) => v.body !== null).length;
  }

  /** Write a head "from another device": the next version, any bytes the
   *  caller sealed. */
  writeHeadDirect(vaultIdHex: string, sealedAt: (version: bigint) => Uint8Array): void {
    const v = this.vaults.get(vaultIdHex);
    if (v === undefined) throw new Error("no such vault");
    const version = v.version + 1n;
    const sealed = sealedAt(version);
    const body = new Uint8Array(8 + sealed.length);
    new DataView(body.buffer).setBigUint64(0, version, false);
    body.set(sealed, 8);
    v.version = version;
    v.body = body;
  }

  private async handle(url: string, init: RequestInit): Promise<Response> {
    const method = init.method ?? "GET";
    const path = url.replace(/^\/storage/, "");
    const request = { method, path };
    this.seen.push(request);
    if (this.offline) throw new TypeError("network down");
    const failing = this.failures.find((f) => f.left > 0 && f.match(request));
    if (failing !== undefined) {
      failing.left -= 1;
      throw new TypeError("network down");
    }
    const headers = new Headers(init.headers);
    const m = /^\/vault\/([0-9a-f]{64})(\/head)?$/.exec(path);
    if (m === null) return json(404, "not-found");
    const id = m[1] as string;
    const isHead = m[2] !== undefined;
    const vault = this.vaults.get(id);
    if (!isHead && method === "PUT") {
      const parsed = JSON.parse(new TextDecoder().decode(await bodyBytes(init))) as { write_pk: string };
      if (vault !== undefined) return vault.writePk === parsed.write_pk ? json(200) : json(409, "vault-claimed");
      this.vaults.set(id, { writePk: parsed.write_pk, version: 0n, body: null });
      this.claims += 1;
      return json(201);
    }
    if (headers.get(STORAGE_PROOF_HEADER) === null || vault === undefined) return json(401, "proof-required");
    if (!isHead && method === "DELETE") {
      this.vaults.delete(id);
      return json(204);
    }
    if (isHead && method === "GET") {
      if (vault.body === null) return json(404, "no-head");
      return new Response(vault.body.slice().buffer as ArrayBuffer, { status: 200 });
    }
    if (isHead && method === "PUT") {
      this.beforeHeadWrite?.(id);
      const body = await bodyBytes(init);
      const version = new DataView(body.buffer, body.byteOffset, 8).getBigUint64(0, false);
      const current = this.vaults.get(id);
      if (current === undefined) return json(401, "proof-required");
      if (version !== current.version + 1n) return json(409, "vault-version-conflict");
      current.version = version;
      current.body = body;
      return json(204);
    }
    return json(405, "method");
  }
}
