// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 5 - the discovery routes of scoped discovery: the username
 * certificate, the Everyone publication, the name lookup, and the OPRF of a
 * username or a name term.
 *
 * - `GET /discovery/username-certificate` (session): this account's
 *   discovery-signed `(lookup key of OPRF(username), account_id)` certificate, carried in
 *   profile v3 so a viewer can verify a username with no lookup;
 * - `GET /.well-known/lakko-username-cert-key`: the verifying key, pinned at
 *   build time when `VITE_LAKKO_USERNAME_CERT_KEY` is set (then never
 *   fetched);
 * - `PUT /discovery/visibility {public, name_terms_b64, public_record_b64}`
 *   (session): enter or leave Everyone;
 * - `POST /discovery/name/lookup {oprf_hash_b64}`: up to 20 people in
 *   Everyone whose name carries the term, with their public records.
 *
 * Every value sent to discovery is an OPRF `lookupKey`; the `output` stays on
 * the device (`discovery/oprf-pair.ts`).
 *
 * The OPRF evaluation itself spends one `discovery-lookup` anonymous token
 * through `DiscoveryClient.oprfEvaluate`.
 */
import { base64Decode, base64Encode, base64UrlDecodeNoPad } from "../auth/account-id";
import { DiscoveryClient, DiscoveryHttpError, DiscoveryNetworkError } from "../discovery/client";
import { finalizeOprf, type OprfPair } from "../discovery/oprf-pair";
import { loadWasm } from "../wasm";
import { sessionFetch } from "../auth/session-fetch";

/** One person in Everyone a name lookup returned. */
export interface NameLookupHit {
  readonly accountId: Uint8Array;
  /** Their public record, opened with the term's OPRF output; `null` when
   *  the server sent none. */
  readonly publicRecord: Uint8Array | null;
}

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
      response = await sessionFetch(`${BASE}${endpoint}`, { method: "GET", headers: this.authHeaders() });
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

  /**
   * Enter or leave Everyone. Entering sends the lookup key of every name term
   * and the public record (composed over the terms' OPRF outputs, which never
   * leave the device); leaving sends neither. ADR-0060 section 5.
   */
  async putVisibility(
    publication:
      | { readonly public: true; readonly nameTermLookupKeys: readonly Uint8Array[]; readonly publicRecord: Uint8Array }
      | { readonly public: false },
  ): Promise<void> {
    const endpoint = "/discovery/visibility";
    const body = publication.public
      ? {
          public: true,
          name_terms_b64: publication.nameTermLookupKeys.map((k) => base64Encode(k)),
          public_record_b64: base64Encode(publication.publicRecord),
        }
      : { public: false, name_terms_b64: [] };
    let response: Response;
    try {
      response = await sessionFetch(`${BASE}${endpoint}`, {
        method: "PUT",
        headers: { "content-type": "application/json", ...this.authHeaders() },
        body: JSON.stringify(body),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(endpoint, err);
    }
    if (!response.ok) {
      // The body carries the code a caller branches on (ADR-0098 step 5:
      // `403 {"error":"hidden-from-search"}`).
      const body = await response.text().catch(() => "");
      throw new DiscoveryHttpError(response.status, endpoint, body);
    }
  }

  /** People in Everyone carrying a name term, by the term's LOOKUP KEY (never
   *  its OPRF output): account id (16 bytes) and public record each. */
  async nameLookup(termLookupKey: Uint8Array): Promise<NameLookupHit[]> {
    const endpoint = "/discovery/name/lookup";
    let response: Response;
    try {
      response = await fetch(`${BASE}${endpoint}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ oprf_hash_b64: base64Encode(termLookupKey) }),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(endpoint, err);
    }
    if (!response.ok) throw new DiscoveryHttpError(response.status, endpoint, "");
    const json = (await response.json()) as {
      results?: { account_id_b64?: string; public_record_b64?: string }[];
    };
    const hits: NameLookupHit[] = [];
    for (const r of json.results ?? []) {
      if (r.account_id_b64 === undefined) continue;
      hits.push({
        accountId: base64Decode(r.account_id_b64),
        publicRecord: r.public_record_b64 === undefined ? null : base64Decode(r.public_record_b64),
      });
    }
    return hits;
  }

  /** One OPRF round over a name term (one anonymous token): the lookup key
   *  for discovery and the output for records. */
  async nameTermOprf(term: string): Promise<OprfPair> {
    const wasm = await loadWasm();
    const blinder = wasm.OprfBlinder.blindNameTerm(term);
    try {
      const resp = await this.discovery.oprfEvaluate({ blinded: blinder.blindedB64 });
      return finalizeOprf(blinder, resp.evaluated);
    } finally {
      try {
        blinder.free();
      } catch {
        /* finalize consumed it */
      }
    }
  }

  /**
   * The OPRF pairs of this account's or a conversation's OWN search terms,
   * in the order given: every term blinded, sent in batches of at most 66
   * (`POST /discovery/oprf/batch`, one `discovery-register` token each), and
   * each answer finalized with its own blinder. A `username` item is blinded
   * as a username, a `term` item as a name search word.
   */
  async ownTermsOprf(
    items: readonly { readonly kind: "term" | "username"; readonly value: string }[],
  ): Promise<OprfPair[]> {
    const wasm = await loadWasm();
    const out: OprfPair[] = [];
    for (let start = 0; start < items.length; start += DiscoveryClient.OPRF_BATCH_MAX) {
      const chunk = items.slice(start, start + DiscoveryClient.OPRF_BATCH_MAX);
      const blinders = chunk.map((it) =>
        it.kind === "username" ? wasm.OprfBlinder.blind(it.value) : wasm.OprfBlinder.blindNameTerm(it.value),
      );
      const consumed = new Set<number>();
      try {
        const evaluated = await this.discovery.oprfEvaluateBatch(blinders.map((b) => b.blindedB64));
        blinders.forEach((blinder, i) => {
          consumed.add(i);
          out.push(finalizeOprf(blinder, evaluated[i] ?? ""));
        });
      } finally {
        blinders.forEach((blinder, i) => {
          if (consumed.has(i)) return;
          try {
            blinder.free();
          } catch {
            /* already freed */
          }
        });
      }
    }
    return out;
  }

  /** One OPRF round over a username (one anonymous token): the lookup key
   *  for discovery and certificates, the output for records. */
  async usernameOprf(username: string): Promise<OprfPair> {
    const wasm = await loadWasm();
    const blinder = wasm.OprfBlinder.blind(username);
    try {
      const resp = await this.discovery.oprfEvaluate({ blinded: blinder.blindedB64 });
      return finalizeOprf(blinder, resp.evaluated);
    } finally {
      try {
        blinder.free();
      } catch {
        /* finalize consumed it */
      }
    }
  }
}
