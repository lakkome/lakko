// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0058 section 5 - the discovery routes of scoped discovery: the username
 * certificate, the Everyone publication, the name lookup, and the OPRF of a
 * username or a name term.
 *
 * - `GET /discovery/username-certificate` (session): this account's
 *   discovery-signed `(OPRF(username), account_id)` certificate, carried in
 *   profile v3 so a viewer can verify a username with no lookup;
 * - `GET /.well-known/lakko-username-cert-key`: the verifying key, pinned at
 *   build time when `VITE_LAKKO_USERNAME_CERT_KEY` is set (then never
 *   fetched);
 * - `PUT /discovery/visibility {public, name_terms_b64}` (session): enter or
 *   leave Everyone;
 * - `POST /discovery/name/lookup {oprf_hash_b64}`: up to 20 account ids of
 *   people in Everyone whose name carries the term.
 *
 * The OPRF evaluation itself spends one `discovery-lookup` anonymous token
 * through `DiscoveryClient.oprfEvaluate`.
 */
import { base64Decode, base64Encode, base64UrlDecodeNoPad } from "../auth/account-id";
import { DiscoveryClient, DiscoveryHttpError, DiscoveryNetworkError } from "../discovery/client";
import { loadWasm } from "../wasm";

const BASE = "/api/discovery";

/** The build-time pinned certificate key, or `null`. */
function pinnedCertKey(): Uint8Array | null {
  const raw = (import.meta as { env?: Record<string, string | undefined> }).env
    ?.VITE_LAKKO_USERNAME_CERT_KEY;
  if (raw === undefined || raw.length === 0) return null;
  try {
    const key = base64UrlDecodeNoPad(raw);
    return key.length === 32 ? key : null;
  } catch {
    return null;
  }
}

let certKeyCache: Promise<Uint8Array> | null = null;

/** The username-certificate verifying key: the pinned one, else fetched once
 *  per page. Throws when neither is available. */
export function usernameCertKey(): Promise<Uint8Array> {
  const pinned = pinnedCertKey();
  if (pinned !== null) return Promise.resolve(pinned);
  if (certKeyCache === null) {
    certKeyCache = (async () => {
      const endpoint = "/.well-known/lakko-username-cert-key";
      let response: Response;
      try {
        response = await fetch(`${BASE}${endpoint}`, { method: "GET" });
      } catch (err) {
        throw new DiscoveryNetworkError(endpoint, err);
      }
      if (!response.ok) throw new DiscoveryHttpError(response.status, endpoint, "");
      const json = (await response.json()) as { key_b64url?: string };
      const key = base64UrlDecodeNoPad(json.key_b64url ?? "");
      if (key.length !== 32) throw new Error("username certificate key is not 32 bytes");
      return key;
    })().catch((err: unknown) => {
      certKeyCache = null;
      throw err;
    });
  }
  return certKeyCache;
}

/** A client of the scoped-discovery routes. */
export class ScopedDiscoveryClient {
  private readonly discovery: DiscoveryClient;

  constructor(private readonly sessionToken: () => string | null) {
    this.discovery = new DiscoveryClient(BASE, { sessionToken });
  }

  private authHeaders(): Record<string, string> {
    const token = this.sessionToken();
    return token === null ? {} : { Authorization: `Bearer ${token}` };
  }

  /** This account's username certificate, or `null` (no username). */
  async usernameCertificate(): Promise<Uint8Array | null> {
    const endpoint = "/discovery/username-certificate";
    let response: Response;
    try {
      response = await fetch(`${BASE}${endpoint}`, { method: "GET", headers: this.authHeaders() });
    } catch (err) {
      throw new DiscoveryNetworkError(endpoint, err);
    }
    if (response.status === 404) return null;
    if (!response.ok) throw new DiscoveryHttpError(response.status, endpoint, "");
    const json = (await response.json()) as { username_certificate_b64?: string };
    return json.username_certificate_b64 === undefined
      ? null
      : base64Decode(json.username_certificate_b64);
  }

  /** Enter (with the name's term hashes) or leave Everyone. */
  async putVisibility(isPublic: boolean, nameTermHashes: readonly Uint8Array[]): Promise<void> {
    const endpoint = "/discovery/visibility";
    let response: Response;
    try {
      response = await fetch(`${BASE}${endpoint}`, {
        method: "PUT",
        headers: { "content-type": "application/json", ...this.authHeaders() },
        body: JSON.stringify({
          public: isPublic,
          name_terms_b64: isPublic ? nameTermHashes.map((h) => base64Encode(h)) : [],
        }),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(endpoint, err);
    }
    if (!response.ok) throw new DiscoveryHttpError(response.status, endpoint, "");
  }

  /** Account ids (16 bytes each) of people in Everyone carrying a term. */
  async nameLookup(termHash: Uint8Array): Promise<Uint8Array[]> {
    const endpoint = "/discovery/name/lookup";
    let response: Response;
    try {
      response = await fetch(`${BASE}${endpoint}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ oprf_hash_b64: base64Encode(termHash) }),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(endpoint, err);
    }
    if (!response.ok) throw new DiscoveryHttpError(response.status, endpoint, "");
    const json = (await response.json()) as { account_ids_b64?: string[] };
    return (json.account_ids_b64 ?? []).map((b) => base64Decode(b));
  }

  /** The 64-byte OPRF output of a name term (one anonymous token). */
  async nameTermHash(term: string): Promise<Uint8Array> {
    const wasm = await loadWasm();
    const blinder = wasm.OprfBlinder.blindNameTerm(term);
    try {
      const resp = await this.discovery.oprfEvaluate({ blinded: blinder.blindedB64 });
      return blinder.finalize(resp.evaluated);
    } finally {
      try {
        blinder.free();
      } catch {
        /* finalize consumed it */
      }
    }
  }

  /** The 64-byte OPRF output of a username (one anonymous token). */
  async usernameHash(username: string): Promise<Uint8Array> {
    const wasm = await loadWasm();
    const blinder = wasm.OprfBlinder.blind(username);
    try {
      const resp = await this.discovery.oprfEvaluate({ blinded: blinder.blindedB64 });
      return blinder.finalize(resp.evaluated);
    } finally {
      try {
        blinder.free();
      } catch {
        /* finalize consumed it */
      }
    }
  }
}
