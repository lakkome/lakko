// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 sections 1 to 4 — `@name`s of groups and channels at discovery.
 *
 * One namespace for people, groups and channels: one OPRF input per name
 * (`OprfBlinder.blind`, the username rule), one lookup key. This module holds
 * the client half of the handle routes:
 *
 * - {@link lookupKeyForName}: one OPRF round (one `discovery-lookup` token);
 * - {@link lookupHandle}: `POST /discovery/lookup`, a public group or channel
 *   answers `kind: "handle"` with its sealed record;
 * - {@link handleWrite}: the signed statement routes (claim, update, replace,
 *   rekey, retire), no session; a claim and a replace spend one
 *   `discovery-register` token.
 *
 * Refusals come back as {@link HandleWriteError} with the server's code
 * (`name-taken`, `statement-refused`, `no-such-handle`, ...), mapped to copy by
 * the caller, never shown as is.
 */

import { base64Decode, base64Encode } from "../auth/account-id";
import { DiscoveryClient, DiscoveryHttpError, DiscoveryNetworkError } from "./client";
import { finalizeOprf, type OprfPair } from "./oprf-pair";
import { loadWasm } from "../wasm";
import { anonTokens, anonymousFetchInit } from "../tokens/anon-token-pool";

const BASE = "/api/discovery";

/** One OPRF round over `name` under the username rule. */
export async function oprfForName(name: string): Promise<OprfPair & { readonly normalized: string }> {
  const wasm = await loadWasm();
  const blinder = wasm.OprfBlinder.blind(name);
  const normalized = blinder.normalizedUsername;
  const discovery = new DiscoveryClient();
  try {
    const resp = await discovery.oprfEvaluate({ blinded: blinder.blindedB64 });
    return { ...finalizeOprf(blinder, resp.evaluated), normalized };
  } finally {
    try {
      blinder.free();
    } catch {
      /* finalize consumed it */
    }
  }
}

/** The lookup key of `name`, `null` when it is not a well-formed name. */
export async function lookupKeyForName(name: string): Promise<Uint8Array | null> {
  try {
    return (await oprfForName(name)).lookupKey;
  } catch (err) {
    if (err instanceof DiscoveryNetworkError || err instanceof DiscoveryHttpError) throw err;
    return null;
  }
}

/** A public group or channel found by its exact `@name`: its sealed record. */
export async function lookupHandle(lookupKey: Uint8Array): Promise<Uint8Array | null> {
  const discovery = new DiscoveryClient();
  const res = await discovery.lookupRaw({ oprf_hash: base64Encode(lookupKey) });
  if (res === null) return null;
  if ((res as { kind?: string }).kind !== "handle") return null;
  if (res.public_record_b64 === undefined) return null;
  try {
    return base64Decode(res.public_record_b64);
  } catch {
    return null;
  }
}

/** A refused handle write, with the server's code. */
export class HandleWriteError extends Error {
  override name = "HandleWriteError";
  constructor(
    public readonly status: number,
    public readonly code: string | null,
  ) {
    super(`handle write refused: ${status} ${code ?? ""}`);
  }
}

/** What a claim, an update, a replace and a rekey answer. */
export interface HandleWriteResult {
  readonly certificate: Uint8Array;
  readonly leaseUntilMs: number;
}

export type HandleRoute = "claim" | "update" | "replace" | "rekey" | "retire";

/** One signed statement to its route. `record` and `terms` only for a public
 *  claim, update or replace. */
export async function handleWrite(
  route: HandleRoute,
  statementB64: string,
  record?: Uint8Array | null,
  terms?: readonly Uint8Array[],
): Promise<HandleWriteResult | null> {
  const path =
    route === "claim"
      ? "/discovery/handle/claim"
      : route === "replace"
        ? "/discovery/handle/replace"
        : route === "rekey"
          ? "/discovery/handle/rekey"
          : "/discovery/handle";
  const method = route === "update" ? "PUT" : route === "retire" ? "DELETE" : "POST";
  const body: Record<string, unknown> = { statement_b64: statementB64 };
  if (record !== undefined && record !== null) body.public_record_b64 = base64Encode(record);
  if (terms !== undefined && terms.length > 0) body.terms_b64 = terms.map((t) => base64Encode(t));
  const json = JSON.stringify(body);
  let response: Response;
  try {
    response =
      route === "claim" || route === "replace"
        ? await anonTokens.spend("discovery-register", (headerName, headerValue) =>
            fetch(
              `${BASE}${path}`,
              anonymousFetchInit({
                method,
                headers: { "content-type": "application/json", [headerName]: headerValue },
                body: json,
              }),
            ),
          )
        : await fetch(
            `${BASE}${path}`,
            anonymousFetchInit({ method, headers: { "content-type": "application/json" }, body: json }),
          );
  } catch (err) {
    if (err instanceof TypeError) throw new DiscoveryNetworkError(path, err);
    throw err;
  }
  if (!response.ok) {
    let code: string | null = null;
    try {
      code = ((await response.json()) as { error?: string }).error ?? null;
    } catch {
      code = null;
    }
    throw new HandleWriteError(response.status, code);
  }
  if (route === "retire" || response.status === 204) return null;
  const answer = (await response.json()) as { handle_certificate_b64?: string; lease_until_ms?: number };
  if (answer.handle_certificate_b64 === undefined) return null;
  return { certificate: base64Decode(answer.handle_certificate_b64), leaseUntilMs: answer.lease_until_ms ?? 0 };
}

/** A name-term search hit: a person (account id) or a public group or
 *  channel (its sealed record). */
export interface NameLookupAnyHit {
  readonly kind: "person" | "handle";
  readonly accountId: Uint8Array | null;
  readonly publicRecord: Uint8Array | null;
}

/** `POST /discovery/name/lookup`, keeping the handle hits a person search
 *  drops. */
export async function nameLookupAll(termLookupKey: Uint8Array): Promise<NameLookupAnyHit[]> {
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
    results?: { kind?: string; account_id_b64?: string; public_record_b64?: string }[];
  };
  const out: NameLookupAnyHit[] = [];
  for (const r of json.results ?? []) {
    try {
      out.push({
        kind: r.kind === "handle" ? "handle" : "person",
        accountId: r.account_id_b64 === undefined ? null : base64Decode(r.account_id_b64),
        publicRecord: r.public_record_b64 === undefined ? null : base64Decode(r.public_record_b64),
      });
    } catch {
      /* a row that does not decode names nothing */
    }
  }
  return out;
}
