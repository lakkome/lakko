// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0064 section 1 — the vault routes of `lakko-svc-storage`, on web.
 *
 * - claim: `PUT /vault/{id}` with `{"write_pk": "<base64url>"}` and one
 *   `vault-claim` anonymous token (201 new, 200 the same key, 409
 *   `vault-claimed` under another key);
 * - head: `PUT` / `GET /vault/{id}/head`, body and response
 *   `version (u64 big-endian) || sealed object`; a write is taken only at the
 *   current version + 1 (409 `vault-version-conflict`), a new head starts at
 *   1; a vault with no head answers 404.
 *
 * Every vault request carries the storage proof (`x-lakko-storage-proof`)
 * over the exact body, signed with the vault's write key
 * (`VaultKeys.signRequest`); the path it signs is the service path
 * (`/vault/<hex>/head`), never the reverse-proxy prefix. A failed proof, and
 * a vault nobody claimed, answer one 401. No session token rides along.
 *
 * Nothing here logs a key, an id or a body.
 */

import { base64UrlEncodeNoPad } from "../auth/account-id";
import { anonTokens, anonymousFetchInit, type AnonTokenPool } from "../tokens/anon-token-pool";
import { DEFAULT_BASE_URL } from "./client";

/** The storage proof header (`lakko_vault::proof::PROOF_HEADER_NAME`). */
export const STORAGE_PROOF_HEADER = "x-lakko-storage-proof";

/** One fetch of a vault route may take this long before it is given up
 *  (chosen, not measured): a head write never holds a commit longer. */
export const VAULT_REQUEST_TIMEOUT_MS = 10_000;

/** The part of `VaultKeys` the client needs, for one vault. */
export interface VaultSigner {
  /** The vault's 32-byte name on the service. */
  vaultId(): Uint8Array;
  /** The 32-byte write public key a claim stores. */
  writePublicKey(): Uint8Array;
  /** The `x-lakko-storage-proof` value for one request. */
  signRequest(method: string, path: string, body: Uint8Array): string;
}

/** A head the service holds: its version and the sealed object. */
export interface StoredHead {
  readonly version: bigint;
  readonly sealed: Uint8Array;
}

/** The answer to a head read. */
export type HeadRead =
  | { readonly kind: "head"; readonly head: StoredHead }
  /** The vault is claimed and holds no head. */
  | { readonly kind: "none" }
  /** 401: nobody claimed the vault (or the proof was refused). */
  | { readonly kind: "unclaimed" };

/** The answer to a head write. */
export type HeadWrite = "written" | "conflict" | "unclaimed";

/** A refusal the caller does not act on (a 4xx/5xx other than the ones
 *  above, a network failure). Carries the status only. */
export class VaultHttpError extends Error {
  override name = "VaultHttpError";
  constructor(public readonly status: number, public readonly code: string | null) {
    super(`vault request refused (${status}${code === null ? "" : ` ${code}`})`);
  }
}

/** What the client sends requests through; the tests inject a fake. */
export type VaultFetch = (url: string, init: RequestInit) => Promise<Response>;

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/** `version || sealed`, the wire of a head write. */
export function encodeVersioned(version: bigint, sealed: Uint8Array): Uint8Array {
  const out = new Uint8Array(8 + sealed.length);
  new DataView(out.buffer).setBigUint64(0, version, false);
  out.set(sealed, 8);
  return out;
}

/** The reverse of {@link encodeVersioned}; `null` for a body shorter than
 *  the version prefix. */
export function decodeVersioned(body: Uint8Array): StoredHead | null {
  if (body.length < 8) return null;
  const version = new DataView(body.buffer, body.byteOffset, 8).getBigUint64(0, false);
  return { version, sealed: body.slice(8) };
}

async function errorCode(r: Response): Promise<string | null> {
  try {
    const j = (await r.json()) as { error?: unknown };
    return typeof j.error === "string" ? j.error : null;
  } catch {
    return null;
  }
}

/** The vault routes of the storage service. */
export class VaultClient {
  constructor(
    private readonly baseUrl: string = DEFAULT_BASE_URL,
    private readonly send: VaultFetch = (url, init) => fetch(url, init),
    private readonly tokens: Pick<AnonTokenPool, "spend"> = anonTokens,
  ) {}

  private init(method: string, headers: Record<string, string>, body?: Uint8Array): RequestInit {
    let signal: AbortSignal | undefined;
    try {
      signal = AbortSignal.timeout(VAULT_REQUEST_TIMEOUT_MS);
    } catch {
      signal = undefined;
    }
    return anonymousFetchInit({
      method,
      headers,
      ...(body === undefined ? {} : { body: body as BodyInit }),
      ...(signal === undefined ? {} : { signal }),
    });
  }

  /** Claim the vault under its write key; spends one `vault-claim` token.
   *  Resolves on 201 / 200; throws {@link VaultHttpError} otherwise (409:
   *  another key holds the name). */
  async claim(vault: VaultSigner): Promise<void> {
    const path = `/vault/${hexOf(vault.vaultId())}`;
    const body = new TextEncoder().encode(JSON.stringify({ write_pk: base64UrlEncodeNoPad(vault.writePublicKey()) }));
    const r = await this.tokens.spend("vault-claim", (name, value) =>
      this.send(
        `${this.baseUrl}${path}`,
        this.init("PUT", { [name]: value, "content-type": "application/json" }, body),
      ),
    );
    if (r.status === 201 || r.status === 200) return;
    throw new VaultHttpError(r.status, await errorCode(r));
  }

  /** Read the head. */
  async getHead(vault: VaultSigner): Promise<HeadRead> {
    const path = `/vault/${hexOf(vault.vaultId())}/head`;
    const proof = vault.signRequest("GET", path, new Uint8Array(0));
    const r = await this.send(`${this.baseUrl}${path}`, this.init("GET", { [STORAGE_PROOF_HEADER]: proof }));
    if (r.status === 401) return { kind: "unclaimed" };
    if (r.status === 404) return { kind: "none" };
    if (!r.ok) throw new VaultHttpError(r.status, await errorCode(r));
    const head = decodeVersioned(new Uint8Array(await r.arrayBuffer()));
    if (head === null) throw new VaultHttpError(r.status, "short-body");
    return { kind: "head", head };
  }

  /** Write the head at `version` (the current version + 1, or 1 for the
   *  first). */
  async putHead(vault: VaultSigner, version: bigint, sealed: Uint8Array): Promise<HeadWrite> {
    const path = `/vault/${hexOf(vault.vaultId())}/head`;
    const body = encodeVersioned(version, sealed);
    const proof = vault.signRequest("PUT", path, body);
    const r = await this.send(
      `${this.baseUrl}${path}`,
      this.init("PUT", { [STORAGE_PROOF_HEADER]: proof, "content-type": "application/octet-stream" }, body),
    );
    if (r.status === 200 || r.status === 201 || r.status === 204) return "written";
    if (r.status === 401) return "unclaimed";
    const code = await errorCode(r);
    if (r.status === 409 && code === "vault-version-conflict") return "conflict";
    throw new VaultHttpError(r.status, code);
  }

  /** Delete the vault and everything in it (`DELETE /vault/{id}`, the proof
   *  over an empty body). `"deleted"` on 204; `"absent"` on 401 (a vault
   *  nobody claimed answers the same as a refused proof); throws
   *  {@link VaultHttpError} otherwise. */
  async deleteVault(vault: VaultSigner): Promise<"deleted" | "absent"> {
    const path = `/vault/${hexOf(vault.vaultId())}`;
    const proof = vault.signRequest("DELETE", path, new Uint8Array(0));
    const r = await this.send(`${this.baseUrl}${path}`, this.init("DELETE", { [STORAGE_PROOF_HEADER]: proof }));
    if (r.status === 204 || r.status === 200) return "deleted";
    if (r.status === 401) return "absent";
    throw new VaultHttpError(r.status, await errorCode(r));
  }
}
