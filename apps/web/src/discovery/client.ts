// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `DiscoveryClient` — fetch-based client for `lakko-svc-discovery`.
 *
 * Two layers, same shape as `auth/client.ts`:
 *
 *   1. **Low-level methods** mirror the three HTTP endpoints 1:1
 *      (`/discovery/oprf`, `/discovery/register`,
 *      `/discovery/lookup`). Take a typed body, return a typed
 *      response, do nothing else.
 *
 *   2. **High-level `register(accountId, username)` and
 *      `lookup(username)`** orchestrate the full multi-round-trip
 *      flow: OPRF blind → POST `/discovery/oprf` (one anonymous
 *      `discovery-lookup` token) → finalize → POST
 *      `/discovery/{register, lookup}`. Both
 *      orchestrators handle username normalisation internally
 *      (via the WASM `OprfBlinder`); callers pass the raw typed
 *      string.
 *
 * ADR-0054 S3: no route here takes a proof of work any more. The anonymous
 * routes a signed-in person uses - the OPRF evaluation, the channel-name
 * register and the three invite consumes - each spend one anonymous token
 * (`../tokens/anon-token-pool.ts`) and carry NO session: they are sent with
 * `anonymousFetchInit`, never through `authHeaders()`. The bearer routes
 * (register, identity, KeyPackage, the invite uploads) carry only the
 * session and can answer `429 {"code": "rate-limited"}`. `/discovery/lookup`
 * and `/discovery/channel/lookup` carry neither.
 *
 * Errors mirror the auth-side surface — `DiscoveryClientArgError` /
 * `DiscoveryHttpError` / `DiscoveryNetworkError` — so the UI can
 * distinguish "username taken" (HTTP 409 from `/register`) and
 * "username unknown" (HTTP 404 from `/lookup`) from network-level
 * failures.
 */
import { loadWasm } from "../wasm";
import {
  base64Decode,
  base64Encode,
  base64UrlDecodeNoPad,
  base64UrlEncodeNoPad,
} from "../auth/account-id";
import {
  anonTokens,
  anonymousFetchInit,
  type AnonTokenContext,
  type AnonTokenPool,
} from "../tokens/anon-token-pool";
import { inviteLinkWidths } from "../contacts/invite-link";
import { sessionFetch } from "../auth/session-fetch";
import { errorCodeOf } from "../auth/session-401";
import { finalizeOprf } from "./oprf-pair";
import type {
  ChannelLookupRequest,
  ChannelLookupResponse,
  ChannelRegisterRequest,
  ChannelRegisterResponse,
  IdentityFetchResponse,
  IdentityUploadRequest,
  IdentityUploadResponse,
  GroupInviteUploadRequest,
  InviteConsumeRequest,
  InviteConsumeResponse,
  InviteFetchResponse,
  InviteOwnerListRequest,
  InviteOwnerListResponse,
  InviteRevokeRequest,
  InviteUploadRequest,
  InviteUploadResponse,
  KeyPackageFetchResponse,
  KeyPackageUploadRequest,
  KeyPackageUploadResponse,
  LookupRequest,
  LookupResponse,
  OprfRequest,
  OprfResponse,
  RegisterRequest,
  RegisterResponse,
} from "./wire";

/** Default base URL — Vite dev proxy (`/api/discovery` →
 *  `127.0.0.1:8084`). Production nginx routes the same path. */
export const DEFAULT_BASE_URL = "/api/discovery";

/** Result of `DiscoveryClient.register`. */
export interface RegisterResult {
  /** The canonical form actually claimed (after NFKC + lowercase
   *  + trim + leading-`@` strip). Surface back to the UI so the
   *  user knows what handle is now bound to their account. */
  normalisedUsername: string;
}

/** Result of `DiscoveryClient.lookup`. The server returns 404
 *  for unknown usernames; the high-level orchestrator surfaces
 *  that as `account: null` rather than an exception, so a
 *  not-found is visible in the type. */
export interface LookupResult {
  normalisedUsername: string;
  /** Raw 16-byte account_id, or `null` if the username is not
   *  registered. */
  account: Uint8Array | null;
}

/** Result of `DiscoveryClient.registerChannelName` (G4-5,
 *  ADR-0010). Mirror of [`RegisterResult`] for the channel
 *  namespace. */
export interface ChannelRegisterResult {
  /** The canonical `@channelname` actually claimed (after the
   *  same NFKC + lowercase + trim + leading-`@` strip the
   *  username path applies via `OprfBlinder.blind`). */
  normalisedHandle: string;
}

/** Result of `DiscoveryClient.lookupChannel` (G4-5, ADR-0010).
 *  The server returns 404 for an unknown `@channelname`; the
 *  high-level orchestrator surfaces that as `channel: null`
 *  rather than an exception, so a not-found is visible in the
 *  type — same shape as [`LookupResult`]. */
export interface ChannelLookupResult {
  normalisedHandle: string;
  /** Raw 16-byte `channel_id`, or `null` when the handle is not
   *  registered. */
  channel: Uint8Array | null;
}

/** Result of `DiscoveryClient.consumeInvite` (Phase 1.18d2a +
 *  1.18d3c).
 *
 *  Discriminated so the joiner UI can branch cleanly:
 *    * `kind: "consumed"` — server bumped `used_count` and
 *      accepted (fresh consume by this joiner); proceed with
 *      `sendContactRequestToBundle`.
 *    * `kind: "already-consumed"` — server's PK on
 *      `(blob_id, joiner_tag)` already had a row, so the
 *      counter was NOT bumped. UI surfaces "you've already
 *      accepted this invite" + Decline-only — the joiner has
 *      done this before; re-sending a ContactRequest would
 *      land another envelope in the inviter's stranger inbox
 *      and is bad UX.
 *    * `kind: "exhausted"` — 410 Gone, `max_uses` ceiling hit
 *      by other joiners before this consume could land. UI
 *      shows "this invite has been used up" + Decline-only.
 *    * `kind: "not-found"` — 404; the blob_id was never issued
 *      OR has expired (server folds expired into 404 to deny
 *      status-code probing). UI surfaces "invite expired or
 *      revoked".
 *
 *  Other non-2xx codes still throw `DiscoveryHttpError` —
 *  reserve the discriminated path for the four response shapes
 *  the joiner is supposed to act on. */
export type ConsumeInviteResult =
  | { readonly kind: "consumed"; readonly usedCount: number; readonly maxUses: number }
  | { readonly kind: "already-consumed"; readonly usedCount: number; readonly maxUses: number }
  | { readonly kind: "exhausted" }
  | { readonly kind: "not-found" };

/** Result of `DiscoveryClient.revokeInvite` (Phase 1.18d2b-web).
 *
 *  Discriminated so the owner UI can branch cleanly:
 *    * `kind: "revoked"` — server deleted the row.
 *    * `kind: "not-found-or-not-owner"` — 404. Server folds
 *      "no row" / "signature did not verify" / "stored bytes
 *      failed re-decode" into the same status to deny status-
 *      code probing of the blob_id space; the UI surfaces a
 *      generic "this invite no longer exists or you don't own
 *      it" message.
 *
 *  Body-shape failures (bad base64, signed_at_ms outside the
 *  replay window, …) still throw `DiscoveryHttpError(400, …)`
 *  — they indicate a buggy client, not a probing attacker. */
export type RevokeInviteResult =
  | { readonly kind: "revoked" }
  | { readonly kind: "not-found-or-not-owner" };

/** One row in the [`DiscoveryClient.listInvitesByOwner`]
 *  response — normalised TS view of the wire shape. Field names
 *  are camelCased compared to the snake_case wire; the
 *  `blobIdSlug` is the URL-safe base64 the owner uses to
 *  reconstruct the share URL or feed into
 *  [`DiscoveryClient.revokeInvite`]. */
export interface ActiveInvite {
  /** URL-safe base64 (no padding) of the 6-byte ledger id —
   *  same `/i/<slug>` segment that appears in the share URL.
   *  Pass through `base64UrlDecodeNoPad` to recover the raw
   *  bytes when calling `revokeInvite`. */
  readonly blobIdSlug: string;
  /** UNIX millis the capability expires at. `0` means never
   *  expires (the `NEVER_EXPIRES` sentinel). */
  readonly expiryMs: number;
  /** Wire-format `max_uses` sentinel: `0` = SINGLE_USE,
   *  `0xFFFFFFFF` = unlimited, any other N = N consumes
   *  allowed. */
  readonly maxUses: number;
  /** Server-counted consumes. */
  readonly usedCount: number;
  /** Owner-supplied free-text label; empty string when not
   *  set. */
  readonly label: string;
  /** `true` if the capability flags the link as a referral
   *  signup-deep-link (Phase 1.9b4). */
  readonly bootstrapReferral: boolean;
  /** RFC 3339 timestamp the server recorded on first upload. */
  readonly createdAt: string;
}

/** Result of `DiscoveryClient.listInvitesByOwner`
 *  (Phase 1.18d2b-list). Discriminated so the owner UI can
 *  branch on the well-known 404 — "no identity bundle for this
 *  account_id OR the signature didn't verify" — distinctly
 *  from a clean empty list.
 *
 *  Body-shape failures (bad base64, signed_at_ms outside the
 *  replay window) still throw `DiscoveryHttpError(400, …)`. */
export type ListInvitesResult =
  | { readonly kind: "list"; readonly invites: ActiveInvite[] }
  | { readonly kind: "not-found-or-not-owner" };

/** Programmer error — empty / invalid inputs caught before any
 *  network round-trip. Distinct from HTTP errors so the UI can
 *  surface a form-validation message. */
export class DiscoveryClientArgError extends Error {
  override name = "DiscoveryClientArgError";
}

/** Server returned a non-2xx status. Carries the original status
 *  + endpoint so the UI can branch on (e.g.) 409 = taken,
 *  404 = unknown. */
/** The header carrying an account statement to discovery (ADR-0066 section
 *  2.2). The core names it too (`accountStatementHeaderName()`); a test pins
 *  the two together. */
export const ACCOUNT_STATEMENT_HEADER = "x-lakko-account-statement";

export class DiscoveryHttpError extends Error {
  override name = "DiscoveryHttpError";
  constructor(
    public readonly status: number,
    public readonly endpoint: string,
    public readonly body: string,
  ) {
    super(`discovery ${endpoint} → ${status}: ${body}`);
  }

  /** The machine-readable code of a `{"error": "<code>"}` body (ADR-0065:
   *  `key-generation-superseded`, `key-generation-not-newer`,
   *  `identity-change-needs-attestation`, `key-reset-attestation-invalid`,
   *  `key-reset-disabled`), or `null` when the body has none. */
  get errorCode(): string | null {
    return errorCodeOf(this.body);
  }
}

/** Network-level failure (DNS, TLS, no route, …). */
export class DiscoveryNetworkError extends Error {
  override name = "DiscoveryNetworkError";
  constructor(public readonly endpoint: string, cause: unknown) {
    super(`discovery ${endpoint} network error: ${String(cause)}`);
  }
}

/** The discovery service answered 429 to an invite fetch or consume (the
 *  open invite routes are behind a per-address token bucket since the
 *  short-link amendment of ADR-0028). The link may be perfectly valid; the UI
 *  says "try again later" and never "this link is invalid". */
export class DiscoveryRateLimitedError extends Error {
  override name = "DiscoveryRateLimitedError";
  constructor(
    public readonly endpoint: string,
    /** Seconds from a `Retry-After` header, when the server sent one. */
    public readonly retryAfterSecs: number | null,
  ) {
    super(`discovery ${endpoint} -> 429 rate limited`);
  }

  static from(response: Response, endpoint: string): DiscoveryRateLimitedError {
    const header = response.headers.get("retry-after");
    const secs = header === null ? Number.NaN : Number(header.trim());
    return new DiscoveryRateLimitedError(
      endpoint,
      Number.isFinite(secs) && secs >= 0 ? Math.ceil(secs) : null,
    );
  }
}

/** `true` when `err`, or anything it wraps (`cause`, a `ContactsError`
 *  variant's `cause`), is a {@link DiscoveryRateLimitedError}. */
export function isDiscoveryRateLimited(err: unknown): boolean {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== undefined && current !== null; depth += 1) {
    if (current instanceof DiscoveryRateLimitedError) return true;
    if (typeof current !== "object") return false;
    const r = current as { name?: unknown; cause?: unknown; variant?: { cause?: unknown } };
    if (r.name === "DiscoveryRateLimitedError") return true;
    current = r.cause ?? r.variant?.cause;
  }
  return false;
}

/** Refuse a ledger id of the wrong width before any request (ADR-0028
 *  short-link amendment: 6 bytes, read from the wasm core). */
async function requireLedgerId(blobId: Uint8Array): Promise<void> {
  const widths = inviteLinkWidths(await loadWasm());
  if (blobId.length !== widths.blobIdLen) {
    throw new DiscoveryClientArgError(`blobId must be ${widths.blobIdLen} bytes`);
  }
}

/** Decode the `blob_id` an upload returned: exactly 8 base64url characters
 *  decoding to 6 bytes. A server answering any other shape (the
 *  pre-amendment 43-character id included) is refused. */
async function decodeServerLedgerId(slug: string, endpoint: string): Promise<Uint8Array> {
  const widths = inviteLinkWidths(await loadWasm());
  let blobId: Uint8Array | null = null;
  if (typeof slug === "string" && slug.length === widths.blobIdUrlLen && /^[A-Za-z0-9_-]+$/.test(slug)) {
    blobId = base64UrlDecodeNoPad(slug);
  }
  if (blobId === null || blobId.length !== widths.blobIdLen) {
    throw new DiscoveryHttpError(
      200,
      endpoint,
      `server returned a blob_id that is not a ${widths.blobIdUrlLen}-character ledger id`,
    );
  }
  return blobId;
}

/**
 * What an invite upload sends (sealed links, ADR-0028 amendment of
 * 2026-09-28): the sealed blob plus the clear fields the server can no longer
 * read out of it. The link secret never appears here.
 */
export interface SealedInviteUpload {
  /** `InviteLinkMint.sealed`. */
  readonly sealed: Uint8Array;
  /** `InviteLinkMint.issuerPubkey` (32 bytes). */
  readonly issuerPubkey: Uint8Array;
  /** The capability's `expiry_ms` (`0n` = never expires). */
  readonly expiryMs: bigint;
  /** The `max_uses` sentinel (`0` = single-use, `0xFFFFFFFF` = unlimited). */
  readonly maxUses: number;
  /** `/i/` only: the label, for the owner's own list. */
  readonly label?: string;
  /** `/i/` only: the referral flag, for the owner's own list. */
  readonly bootstrapReferral?: boolean;
}

/** Largest sealed blob each ledger stores (the server's column widths, derived
 *  from each codec's `MAX_SEALED_LEN`, ADR-0028 amendment). Checked here so a
 *  misbehaving caller fails before a round trip. */
export const MAX_SEALED_INVITE_BYTES = 254;
export const MAX_SEALED_CALL_INVITE_BYTES = 4338;
export const MAX_SEALED_GROUP_INVITE_BYTES = 938;

function requireSealedUpload(upload: SealedInviteUpload, max: number): void {
  if (upload.sealed.length === 0) {
    throw new DiscoveryClientArgError("sealed must be non-empty");
  }
  if (upload.sealed.length > max) {
    throw new DiscoveryClientArgError(
      `sealed exceeds the ${max}-byte ceiling (got ${upload.sealed.length} bytes)`,
    );
  }
  if (upload.issuerPubkey.length !== 32) {
    throw new DiscoveryClientArgError("issuerPubkey must be 32 bytes");
  }
  if (upload.expiryMs < 0n || upload.expiryMs > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new DiscoveryClientArgError("expiryMs out of range");
  }
  if (!Number.isInteger(upload.maxUses) || upload.maxUses < 0 || upload.maxUses > 0xffff_ffff) {
    throw new DiscoveryClientArgError("maxUses must be a u32");
  }
}

function sealedUploadBody(upload: SealedInviteUpload): {
  sealed: string;
  issuer_pubkey: string;
  expiry_ms: number;
  max_uses: number;
} {
  return {
    sealed: base64Encode(upload.sealed),
    issuer_pubkey: base64Encode(upload.issuerPubkey),
    expiry_ms: Number(upload.expiryMs),
    max_uses: upload.maxUses,
  };
}

/** One device of an account, as the directory serves it (ADR-0050). */
export interface DeviceKeyPackage {
  /** The device leaf's 32-byte Ed25519 signature key. */
  readonly deviceId: Uint8Array;
  /** TLS-encoded MLS KeyPackage bytes. */
  readonly keyPackage: Uint8Array;
}

export class DiscoveryClient {
  /**
   * Phase 1.5b: bearer token threaded into the
   * `Authorization: Bearer <token>` header on every protected
   * `lakko-svc-discovery` write (register, identity upload,
   * keypackage upload, invite upload, invite revoke). `null`
   * means "anonymous" — protected endpoints will return 401. The
   * anonymous-token routes never carry it (ADR-0054 S3).
   */
  private sessionToken: string | null = null;

  /** Read at REQUEST time when set, so a client built once (a route's
   *  `ContactsClient`) always sends the current session's token and keeps
   *  working across a re-login. Takes precedence over
   *  {@link setSessionToken}. */
  private readonly sessionTokenProvider: (() => string | null) | undefined;

  /** The anonymous-token pool the token routes spend from. */
  private readonly tokens: AnonTokenPool;

  constructor(
    public readonly baseUrl: string = DEFAULT_BASE_URL,
    options: {
      readonly sessionToken?: () => string | null;
      readonly tokens?: AnonTokenPool;
    } = {},
  ) {
    this.sessionTokenProvider = options.sessionToken;
    this.tokens = options.tokens ?? anonTokens;
  }

  /** Update the bearer token in-place. Mirror of the iOS +
   *  Android `setSessionToken` shape. */
  setSessionToken(token: string | null): void {
    this.sessionToken = token;
  }

  /** Build the `Authorization` header value when a session token
   *  is set. `null` collapses to "no header" — caller spreads
   *  via `{ ...this.authHeaders() }`. */
  private authHeaders(): Record<string, string> {
    const token =
      this.sessionTokenProvider !== undefined ? this.sessionTokenProvider() : this.sessionToken;
    return token !== null ? { Authorization: `Bearer ${token}` } : {};
  }

  // -----------------------------------------------------------------
  // Low-level: 1:1 HTTP endpoints
  // -----------------------------------------------------------------

  /** `POST /discovery/oprf` — spends one `discovery-lookup` token, no
   *  session. */
  async oprfEvaluate(req: OprfRequest): Promise<OprfResponse> {
    return this.tokenPostJson("discovery-lookup", "/discovery/oprf", req);
  }

  async register(req: RegisterRequest): Promise<RegisterResponse> {
    return this.post("/discovery/register", req);
  }

  /** `POST /discovery/identity` — upsert the identity bundle. */
  async identityUpload(
    req: IdentityUploadRequest,
  ): Promise<IdentityUploadResponse> {
    return this.post("/discovery/identity", req);
  }

  /** `POST /discovery/keypackage` — upsert the user's MLS
   *  KeyPackage. */
  async keypackageUpload(
    req: KeyPackageUploadRequest,
  ): Promise<KeyPackageUploadResponse> {
    return this.post("/discovery/keypackage", req);
  }

  /** `POST /discovery/invite` — upload a verified
   *  `InviteCapability` blob, get the server-assigned
   *  `blob_id` back (Phase 1.18d1). The high-level
   *  [`uploadInvite`] wrapper validates the input. */
  async inviteUpload(req: InviteUploadRequest): Promise<InviteUploadResponse> {
    return this.post("/discovery/invite", req);
  }

  /** `POST /discovery/invite/{blob_id}/consume` — atomic
   *  `max_uses` enforcement (Phase 1.18d2a). The high-level
   *  [`consumeInvite`] wrapper computes the joiner tag and folds
   *  the 200 / 404 / 410 status trifecta into a discriminated
   *  [`ConsumeInviteResult`]. */
  async inviteConsume(
    blobIdSlug: string,
    req: InviteConsumeRequest,
  ): Promise<InviteConsumeResponse> {
    return this.tokenPostJson("invite-consume", `/discovery/invite/${blobIdSlug}/consume`, req);
  }

  /** `POST /discovery/call-invite` — upload a verified
   *  `CallInviteCapability` blob, get the server-assigned
   *  `blob_id` back (Phase 3c3-server). Sibling of
   *  [`inviteUpload`]; wire shape (`InviteUploadRequest`) is
   *  shared with the contact-invite endpoint — only the URL
   *  path differs. The high-level [`uploadCallInvite`] wrapper
   *  validates the input. */
  async callInviteUpload(
    req: GroupInviteUploadRequest,
  ): Promise<InviteUploadResponse> {
    return this.post("/discovery/call-invite", req);
  }

  /** `POST /discovery/call-invite/{blob_id}/consume` — atomic
   *  `max_uses` enforcement against the call-invite ledger
   *  (Phase 3c3-multi-use). The high-level
   *  [`consumeCallInvite`] wrapper drives the
   *  per-joiner tag computation and folds 200 / 404 / 410 into
   *  a discriminated [`ConsumeInviteResult`]. */
  async callInviteConsume(
    blobIdSlug: string,
    req: InviteConsumeRequest,
  ): Promise<InviteConsumeResponse> {
    return this.tokenPostJson(
      "invite-consume",
      `/discovery/call-invite/${blobIdSlug}/consume`,
      req,
    );
  }

  /** `DELETE /discovery/invite/{blob_id}` — owner-signed
   *  revocation (Phase 1.18d2b). The high-level
   *  [`revokeInvite`] wrapper signs the canonical bytes via
   *  `wasm.signInviteRevocation` and folds the 204 / 404
   *  response shapes into a discriminated result.
   *
   *  Note: not implemented as a `post()` because the verb is
   *  DELETE and the body shape is distinct enough that
   *  factoring through the existing JSON helper would obscure
   *  more than it shares. */
  async inviteRevoke(blobIdSlug: string, req: InviteRevokeRequest): Promise<void> {
    const endpoint = `/discovery/invite/${blobIdSlug}`;
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "DELETE",
        headers: {
          "content-type": "application/json",
          ...this.authHeaders(),
        },
        body: JSON.stringify(req),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(`${endpoint} (DELETE)`, err);
    }
    if (response.status === 204) return;
    const text = await safeReadBody(response);
    throw new DiscoveryHttpError(response.status, `${endpoint} (DELETE)`, text);
  }

  /** `DELETE /discovery/call-invite/{blob_id}` — owner-signed
   *  revocation of a [`CallInviteCapability`] blob (Phase
   *  3c3-revoke). Sibling of [`inviteRevoke`]; the wire shape
   *  is identical ([`InviteRevokeRequest`]) but the signature
   *  is minted under the distinct `lakko-call-invite-revoke/v1`
   *  domain separator so a captured contact-invite revoke can't
   *  be replayed here (and vice versa).
   *
   *  High-level wrapper: [`revokeCallInvite`]. */
  async callInviteRevoke(blobIdSlug: string, req: InviteRevokeRequest): Promise<void> {
    const endpoint = `/discovery/call-invite/${blobIdSlug}`;
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "DELETE",
        headers: {
          "content-type": "application/json",
          ...this.authHeaders(),
        },
        body: JSON.stringify(req),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(`${endpoint} (DELETE)`, err);
    }
    if (response.status === 204) return;
    const text = await safeReadBody(response);
    throw new DiscoveryHttpError(response.status, `${endpoint} (DELETE)`, text);
  }

  /** A3 (GDPR erasure) — `DELETE /discovery/account`. Purges this account's
   *  discovery footprint (username binding, identity bundle, KeyPackage, owned
   *  invites). Bearer-auth; the server keys the purge on the verified session
   *  token's account_id, so the caller can only erase their own. Idempotent
   *  (204). */
  async deleteAccount(): Promise<void> {
    const endpoint = "/discovery/account";
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "DELETE",
        headers: { ...this.authHeaders() },
      });
    } catch (err) {
      throw new DiscoveryNetworkError(`${endpoint} (DELETE)`, err);
    }
    if (response.status === 204) return;
    const text = await safeReadBody(response);
    throw new DiscoveryHttpError(response.status, `${endpoint} (DELETE)`, text);
  }

  /** Wire-level `POST /discovery/invite/by-owner` (Phase
   *  1.18d2b-list). Returns the raw wire response on 200,
   *  `null` on 404, throws `DiscoveryHttpError` on any other
   *  non-2xx. The session-token middleware is the DoS gate;
   *  the body signature is the actual ownership proof — see
   *  the high-level [`listInvitesByOwner`] wrapper for the
   *  ergonomic API. */
  async inviteOwnerList(
    req: InviteOwnerListRequest,
  ): Promise<InviteOwnerListResponse | null> {
    const endpoint = "/discovery/invite/by-owner";
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          ...this.authHeaders(),
        },
        body: JSON.stringify(req),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(`${endpoint} (POST)`, err);
    }
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(response.status, `${endpoint} (POST)`, text);
    }
    return (await response.json()) as InviteOwnerListResponse;
  }

  /**
   * `GET /discovery/identity/{accountId_b64url}`. Returns the
   * raw bundle on 200, `null` on 404 (typed not-found path),
   * throws on any other non-2xx.
   *
   * The path uses **base64url-without-padding** because axum
   * rejects `+` and `/` in path segments; the rest of the wire
   * format uses standard-alphabet base64.
   */
  async identityFetch(
    accountId: Uint8Array,
  ): Promise<IdentityFetchResponse | null> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    const accountIdB64Url = base64UrlEncodeNoPad(accountId);
    const url = `${this.baseUrl}/discovery/identity/${accountIdB64Url}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/identity (GET)", err);
    }
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/identity (GET)",
        text,
      );
    }
    return (await response.json()) as IdentityFetchResponse;
  }

  /**
   * `GET /discovery/keypackage/{accountId_b64url}`. Returns the
   * raw KeyPackage shape on 200, `null` on 404, throws on
   * other non-2xx.
   */
  async keypackageFetch(
    accountId: Uint8Array,
  ): Promise<KeyPackageFetchResponse | null> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    const accountIdB64Url = base64UrlEncodeNoPad(accountId);
    const url = `${this.baseUrl}/discovery/keypackage/${accountIdB64Url}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/keypackage (GET)", err);
    }
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/keypackage (GET)",
        text,
      );
    }
    return (await response.json()) as KeyPackageFetchResponse;
  }

  /**
   * `POST /discovery/lookup`. Returns the raw response on 200,
   * `null` on 404 (the typed not-found path), throws on any
   * other non-2xx.
   */
  async lookupRaw(req: LookupRequest): Promise<LookupResponse | null> {
    const url = `${this.baseUrl}/discovery/lookup`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(req),
      });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/lookup", err);
    }
    if (response.status === 404) {
      return null;
    }
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/lookup",
        text,
      );
    }
    return (await response.json()) as LookupResponse;
  }

  /** `POST /discovery/channel/register` — bind an OPRF-hashed
   *  `@channelname` to a `channel_id`. Spends one `discovery-register`
   *  anonymous token and carries no session (ADR-0054 S3). */
  async channelRegister(
    req: ChannelRegisterRequest,
  ): Promise<ChannelRegisterResponse> {
    return this.tokenPostJson("discovery-register", "/discovery/channel/register", req);
  }

  /**
   * `POST /discovery/channel/lookup`. Returns the raw response on
   * 200, `null` on 404 (the typed not-found path), throws on any
   * other non-2xx. Mirror of [`lookupRaw`] for the channel
   * namespace — unauthenticated, so no bearer header.
   */
  async channelLookupRaw(
    req: ChannelLookupRequest,
  ): Promise<ChannelLookupResponse | null> {
    const url = `${this.baseUrl}/discovery/channel/lookup`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(req),
      });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/channel/lookup", err);
    }
    if (response.status === 404) {
      return null;
    }
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/channel/lookup",
        text,
      );
    }
    return (await response.json()) as ChannelLookupResponse;
  }

  // -----------------------------------------------------------------
  // High-level: full register / lookup flows
  // -----------------------------------------------------------------

  /**
   * Bind `@username` to `accountId` server-side.
   *
   * Flow: OPRF blind → POST `/discovery/oprf` (one anonymous token) →
   * finalize → POST `/discovery/register` (bearer).
   *
   * Username normalisation is handled inside `OprfBlinder.blind`
   * (the WASM helper); the result is also surfaced back via
   * `RegisterResult.normalisedUsername` so the UI can show what
   * was actually registered.
   *
   * Throws `DiscoveryHttpError` with `status === 409` on
   * username collision (server-side `INSERT IGNORE` semantics —
   * usernames are first-come-first-served and immutable in
   * Phase 1.5 per `services/discovery/src/registry.rs` notes).
   */
  async registerUsername(
    accountId: Uint8Array,
    username: string,
  ): Promise<RegisterResult> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    const wasm = await loadWasm();
    const blinder = wasm.OprfBlinder.blind(username);
    const normalisedUsername = blinder.normalizedUsername;

    const oprfResp = await this.oprfEvaluate({ blinded: blinder.blindedB64 });

    // ADR-0060 section 5: discovery receives the lookup key only; the raw
    // OPRF output stays on this device (it keys sealed records).
    const lookupKeyB64 = base64Encode(
      finalizeOprf(blinder, oprfResp.evaluated).lookupKey,
    );

    const registerResp = await this.register({
      account_id: base64Encode(accountId),
      oprf_hash: lookupKeyB64,
    });
    if (!registerResp.ok) {
      throw new DiscoveryHttpError(
        200,
        "/discovery/register",
        "server reported ok=false",
      );
    }
    return { normalisedUsername };
  }

  /**
   * Upload (or replace) the identity bundle for the given
   * account. Drives one bearer `POST /discovery/identity`.
   *
   * Idempotent — the server upserts via
   * `INSERT … ON DUPLICATE KEY UPDATE`. Calling this on every
   * login with a deterministically-derived Identity (1.16d3b2)
   * is a cheap no-op when the keys haven't changed.
   *
   * G-4 (ADR-0016): on an AccountIdentity rotation (recover) the
   * caller passes `supersessionProof` — the old-key-signed proof
   * bridging old→new. It rides in the SAME request as the new
   * keys (ADR-0016 rule #1, atomic emit), and the server stores
   * it opaquely (invariant #2). Omitting it (the signup / login /
   * passkey paths) clears any previously-stored proof server-side,
   * which is exactly the "non-rotation re-upload" behaviour.
   *
   * @param supersessionProof  Raw proof bytes (from
   *        `signSupersessionProof`) or a pre-base64'd string.
   *        Omit on non-rotation uploads.
   * @param leafAuth  G-30 #2 (ADR-0019): raw 64-byte genesis leaf
   *        self-signature bytes (from `signLeafGenesis`) or a
   *        pre-base64'd string, authorising a NEW transparency-log
   *        binding. Supplied by the signup + fresh-start-mint callers
   *        (where a new key is published). Omit on a routine same-key
   *        re-upload — the log append is idempotent there.
   * @param keyResetAttest  ADR-0065: the raw 177-byte key-reset attestation
   *        (from `AuthClient.resetAccountKeys`) or a pre-base64'd string
   *        (standard alphabet). Supplied ONLY by the key-reset publish; it is
   *        what lets discovery accept a changed key without a supersession
   *        proof. Omit everywhere else.
   * @param genesisAttest  ADR-0036 (G-30 finding 2): raw 153-byte
   *        `GenesisAttestation` bytes (from `AuthClient.genesisAttest`) or a
   *        pre-base64'd string, the auth-attested proof that only the
   *        OPAQUE-authenticated account could have published this genesis
   *        key. Supplied ONLY by the genesis publish sites (first-ever signup
   *        + the lost-everything fresh-start mint) alongside `leafAuth`. Omit
   *        on a rotation / routine re-upload — the server `COALESCE`-preserves
   *        any stored value there.
   */
  async uploadIdentityBundle(
    accountId: Uint8Array,
    signaturePubkey: Uint8Array,
    x25519Pubkey: Uint8Array,
    supersessionProof?: Uint8Array | string,
    leafAuth?: Uint8Array | string,
    genesisAttest?: Uint8Array | string,
    keyResetAttest?: Uint8Array | string,
  ): Promise<void> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    if (signaturePubkey.length !== 32) {
      throw new DiscoveryClientArgError(
        "signaturePubkey must be 32 bytes (Ed25519)",
      );
    }
    if (x25519Pubkey.length !== 32) {
      throw new DiscoveryClientArgError(
        "x25519Pubkey must be 32 bytes (HPKE init_key)",
      );
    }

    // Normalise the optional proof to a base64 string for the
    // wire (the field is `supersession_proof?: string`). A raw
    // `Uint8Array` gets encoded here; a string is passed through
    // as-is (already base64). `undefined` stays `undefined` so the
    // field is omitted entirely (server clears the stored proof).
    let proofB64: string | undefined;
    if (supersessionProof !== undefined) {
      proofB64 =
        typeof supersessionProof === "string"
          ? supersessionProof
          : base64Encode(supersessionProof);
    }

    // G-30 #2 (ADR-0019): normalise the optional genesis leaf
    // self-signature to base64 for the wire (`leaf_auth?: string`),
    // mirroring the supersession-proof handling above. `undefined`
    // stays omitted — the server only requires it for a NEW binding.
    let leafAuthB64: string | undefined;
    if (leafAuth !== undefined) {
      leafAuthB64 =
        typeof leafAuth === "string" ? leafAuth : base64Encode(leafAuth);
    }

    // ADR-0036: normalise the optional genesis attestation to base64 for
    // the wire (`genesis_attest?: string`), mirroring the `leafAuth`
    // handling above. `undefined` stays omitted — a rotation / re-upload
    // sends none and the server preserves any stored value via `COALESCE`.
    let genesisAttestB64: string | undefined;
    if (genesisAttest !== undefined) {
      genesisAttestB64 =
        typeof genesisAttest === "string"
          ? genesisAttest
          : base64Encode(genesisAttest);
    }


    // ADR-0065: the key-reset attestation, standard base64 like every
    // sibling field (auth hands it out URL-safe; the caller passes the raw
    // bytes and this re-encodes).
    let keyResetAttestB64: string | undefined;
    if (keyResetAttest !== undefined) {
      keyResetAttestB64 =
        typeof keyResetAttest === "string" ? keyResetAttest : base64Encode(keyResetAttest);
    }

    const resp = await this.identityUpload({
      account_id: base64Encode(accountId),
      signature_pubkey: base64Encode(signaturePubkey),
      x25519_pubkey: base64Encode(x25519Pubkey),
      ...(proofB64 !== undefined ? { supersession_proof: proofB64 } : {}),
      ...(leafAuthB64 !== undefined ? { leaf_auth: leafAuthB64 } : {}),
      ...(genesisAttestB64 !== undefined
        ? { genesis_attest: genesisAttestB64 }
        : {}),
      ...(keyResetAttestB64 !== undefined ? { key_reset_attest: keyResetAttestB64 } : {}),
    });
    if (!resp.ok) {
      throw new DiscoveryHttpError(
        200,
        "/discovery/identity",
        "server reported ok=false",
      );
    }
  }

  /**
   * Upload (or replace) the user's MLS KeyPackage. Drives one
   * bearer `POST /discovery/keypackage`.
   *
   * Unlike the identity bundle, KeyPackages are **not**
   * deterministic — `Identity::create_key_package()` generates
   * fresh randomness on every call (RFC 9420 forward-secrecy
   * property). Calling this on every login refreshes the
   * `init_key` on the server, so a fresh sender starting a new
   * 1×1 group gets bytes the recipient hasn't already used.
   *
   * The Phase-1 directory only stores ONE KeyPackage per
   * account at a time (consume-on-use is 1.16e6+ work), so a
   * sender racing two sessions can in theory seed two groups
   * with the same `init_key`. Documented MVP trade-off.
   */
  async uploadKeyPackage(
    accountId: Uint8Array,
    keyPackageBytes: Uint8Array,
    deviceId: Uint8Array,
  ): Promise<void> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    if (deviceId.length !== 32) {
      throw new DiscoveryClientArgError("deviceId must be 32 bytes");
    }
    if (keyPackageBytes.length === 0) {
      throw new DiscoveryClientArgError("keyPackageBytes must be non-empty");
    }
    // Hard ceiling mirrors `MAX_KEYPACKAGE_BYTES = 2048` on the
    // server. Surfacing here saves a round-trip on a
    // misbehaving caller.
    const MAX = 2048;
    if (keyPackageBytes.length > MAX) {
      throw new DiscoveryClientArgError(
        `keyPackageBytes exceeds the ${MAX}-byte ceiling ` +
          `(got ${keyPackageBytes.length} bytes)`,
      );
    }


    const resp = await this.keypackageUpload({
      account_id: base64Encode(accountId),
      device_id: base64Encode(deviceId),
      keypackage: base64Encode(keyPackageBytes),
    });
    if (!resp.ok) {
      throw new DiscoveryHttpError(
        200,
        "/discovery/keypackage",
        "server reported ok=false",
      );
    }
  }

  /**
   * Resolve `accountId` to every device's KeyPackage bytes (TLS-encoded
   * MLS), oldest device first (ADR-0050). Returns `null` when the account
   * has published none. A caller adding the account adds ALL of them; the
   * peer's device binding inside each leaf is what the core verifies.
   */
  async fetchKeyPackages(
    accountId: Uint8Array,
  ): Promise<DeviceKeyPackage[] | null> {
    const raw = await this.keypackageFetch(accountId);
    if (raw === null) return null;
    return raw.keypackages.map((d) => ({
      deviceId: base64Decode(d.device_id),
      keyPackage: base64Decode(d.keypackage),
    }));
  }

  /** `DELETE /discovery/keypackage/device/{device_id_b64url}` — remove one of the
   *  authenticated account's device KeyPackages (ADR-0050 device removal).
   *  Idempotent (204).
   *
   *  ADR-0066 section 2.2: the session's OWN device row needs nothing more;
   *  ANOTHER device's row needs `statement`, the 138-byte `device-revoke`
   *  account statement for that id (`signDeviceRevokeStatement`), sent as
   *  the `x-lakko-account-statement` header. Without a valid one the server
   *  answers `403 {"error":"account-key-required"}`. Every delete tombstones
   *  the id for 30 days, the device's own included. */
  async deleteKeyPackage(deviceId: Uint8Array, statement?: Uint8Array): Promise<void> {
    if (deviceId.length !== 32) {
      throw new DiscoveryClientArgError("deviceId must be 32 bytes");
    }
    const endpoint = `/discovery/keypackage/device/${base64UrlEncodeNoPad(deviceId)}`;
    const url = `${this.baseUrl}${endpoint}`;
    const headers: Record<string, string> = { ...this.authHeaders() };
    if (statement !== undefined) {
      headers[ACCOUNT_STATEMENT_HEADER] = base64UrlEncodeNoPad(statement);
    }
    let response: Response;
    try {
      response = await sessionFetch(url, { method: "DELETE", headers });
    } catch (err) {
      throw new DiscoveryNetworkError(`${endpoint} (DELETE)`, err);
    }
    if (response.status === 204) return;
    const text = await safeReadBody(response);
    throw new DiscoveryHttpError(response.status, `${endpoint} (DELETE)`, text);
  }

  /**
   * Upload a sealed `/i/` link blob (`InviteCapability.compose(...)`'s
   * `InviteLinkMint`) and receive the server-assigned `blob_id` (6 random
   * bytes, ADR-0028 short-link amendment). Not idempotent: every upload mints
   * a fresh id. The server cannot open the blob (sealed-link amendment,
   * 2026-09-28); the clear fields carry what it needs.
   *
   * @throws `DiscoveryClientArgError` on local input validation,
   *         `DiscoveryHttpError` on server rejection,
   *         `DiscoveryNetworkError` on transport failure.
   */
  async uploadInvite(upload: SealedInviteUpload): Promise<Uint8Array> {
    requireSealedUpload(upload, MAX_SEALED_INVITE_BYTES);
    const resp = await this.inviteUpload({
      ...sealedUploadBody(upload),
      label: upload.label ?? "",
      bootstrap_referral: upload.bootstrapReferral ?? false,
    });
    return decodeServerLedgerId(resp.blob_id, "/discovery/invite");
  }

  /**
   * Fetch a previously-uploaded `InviteCapability` by its
   * server-assigned `blob_id`. Returns `null` on 404 (the
   * server folds expired blobs into the same status — see
   * `services/discovery/src/invite.rs::fetch` for why we don't
   * use 410). Returns the SEALED link blob on 200; the caller opens it
   * with the link secret from the fragment
   * (`wasm.InviteCapability.openLink`), which also verifies the
   * capability inside, so a substituted blob opens nothing. Throws
   * `DiscoveryRateLimitedError` on 429 (per-address rate limit).
   */
  async fetchInvite(blobId: Uint8Array): Promise<Uint8Array | null> {
    await requireLedgerId(blobId);
    const slug = base64UrlEncodeNoPad(blobId);
    const url = `${this.baseUrl}/discovery/invite/${slug}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/invite (GET)", err);
    }
    if (response.status === 429) throw DiscoveryRateLimitedError.from(response, "/discovery/invite (GET)");
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/invite (GET)",
        text,
      );
    }
    const raw = (await response.json()) as InviteFetchResponse;
    return base64Decode(raw.sealed);
  }

  /**
   * Atomically bump the consume counter for the invite at
   * `blobId` with per-joiner dedup (Phase 1.18d2a + 1.18d3c).
   * Returns a discriminated [`ConsumeInviteResult`] so the
   * joiner UI can branch:
   *
   *   * `"consumed"` — server accepted, bumped the counter,
   *     and recorded this joiner in the side table.
   *     Caller proceeds with `sendContactRequestToBundle(...)`.
   *   * `"already-consumed"` — server's `(blob_id, joiner_tag)`
   *     PK already had a row, so the counter was NOT bumped.
   *     Caller surfaces "you've already accepted this invite"
   *     + Decline-only and skips the ContactRequest send (a
   *     fresh CR would land another envelope in the inviter's
   *     stranger inbox; bad UX).
   *   * `"exhausted"` — 410 Gone, the `max_uses` ceiling was
   *     hit by other joiners before this consume could land.
   *     Caller surfaces "this invite has been used up" and
   *     offers Decline only.
   *   * `"not-found"` — 404, never-issued OR expired (server
   *     folds both into the same status). Caller surfaces
   *     "invite expired or revoked".
   *
   * Other non-2xx codes still throw `DiscoveryHttpError` —
   * the discriminated path is reserved for the four response
   * shapes the joiner is supposed to act on without a developer
   * involved.
   *
   * Spends one anonymous `invite-consume` token and carries no
   * session (ADR-0054 S3), plus one WASM call to compute the per-joiner
   * dedup tag locally before posting.
   *
   * @param blobId            6-byte ledger id
   *                          (the same value `uploadInvite`
   *                          returned).
   * @param capabilityBytes   The PLAINTEXT capability wire the
   *                          link opened to (`capability.wire`),
   *                          never the sealed blob.
   * @param joinerAccountId   The joiner's 16-byte UUIDv7
   *                          account_id. Never sent to the
   *                          server in plaintext — only the
   *                          opaque HMAC tag derived from it
   *                          crosses the wire.
   */
  async consumeInvite(
    blobId: Uint8Array,
    capabilityBytes: Uint8Array,
    joinerAccountId: Uint8Array,
  ): Promise<ConsumeInviteResult> {
    await requireLedgerId(blobId);
    if (joinerAccountId.length !== 16) {
      throw new DiscoveryClientArgError("joinerAccountId must be 16 bytes");
    }

    // Compute the per-joiner dedup tag locally before any
    // network call — keeps the joiner_account_id off the wire
    // and lets the token wait happen in parallel with the
    // tag derivation in future iterations.
    const wasm = await loadWasm();
    const joinerTag = wasm.computeJoinerInviteTag(
      capabilityBytes,
      joinerAccountId,
    );

    const slug = base64UrlEncodeNoPad(blobId);
    return this.consumeLedger(`/discovery/invite/${slug}/consume`, joinerTag);
  }

  /**
   * Upload a sealed `/c/` link blob (`composeCallInviteCapability(...)`'s
   * `InviteLinkMint`) and get its server-assigned `blob_id` back. Sibling of
   * [`uploadInvite`]; every upload mints a fresh 6-byte id.
   */
  async uploadCallInvite(upload: SealedInviteUpload): Promise<Uint8Array> {
    requireSealedUpload(upload, MAX_SEALED_CALL_INVITE_BYTES);
    const resp = await this.callInviteUpload(sealedUploadBody(upload));
    return decodeServerLedgerId(resp.blob_id, "/discovery/call-invite");
  }

  /**
   * Fetch a previously-uploaded `CallInviteCapability` by its
   * server-assigned `blob_id` (Phase 3c3-client).
   *
   * Sibling of [`fetchInvite`]. Returns `null` on 404 (server
   * folds expired blobs into the same status to deny attacker
   * status-code probing). Returns the SEALED link blob on
   * 200; the caller opens it with the fragment's secret
   * (`wasm.CallInviteCapability.openLink`), so a substituted blob
   * opens nothing. Throws
   * `DiscoveryRateLimitedError` on 429 (per-address rate limit).
   */
  async fetchCallInvite(blobId: Uint8Array): Promise<Uint8Array | null> {
    await requireLedgerId(blobId);
    const slug = base64UrlEncodeNoPad(blobId);
    const url = `${this.baseUrl}/discovery/call-invite/${slug}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/call-invite (GET)", err);
    }
    if (response.status === 429) throw DiscoveryRateLimitedError.from(response, "/discovery/call-invite (GET)");
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/call-invite (GET)",
        text,
      );
    }
    const raw = (await response.json()) as InviteFetchResponse;
    return base64Decode(raw.sealed);
  }

  /**
   * Atomically bump the consume counter for the call-invite at
   * `blobId` with per-joiner dedup (Phase 3c3-client).
   *
   * Sibling of [`consumeInvite`]. The discriminated result
   * shape is identical:
   *   * `"consumed"` — server accepted, counter bumped,
   *     `(blob_id, joiner_tag)` recorded in the side table.
   *     Caller proceeds with the joiner-side MLS Welcome
   *     fetch + group bootstrap (see
   *     `lakko_calls::link_invite::accept_call_invite`).
   *   * `"already-consumed"` — `(blob_id, joiner_tag)` already
   *     existed; counter NOT bumped. UI surfaces "you've
   *     already joined this call" + opens the existing call
   *     group rather than re-bootstrapping.
   *   * `"exhausted"` — 410, `max_uses` ceiling already hit by
   *     other joiners.
   *   * `"not-found"` — 404, never-issued OR expired.
   *
   * Reuses the **same** `wasm.computeJoinerInviteTag(...)`
   * helper as the contact-invite flow — salt is per-blob, so
   * contact-invite and call-invite tags for the same joiner
   * don't collide on the server side (proven by
   * `joiner_tag_works_on_call_invite_bytes` in
   * `lakko_contacts::invite`).
   *
   * @param blobId            6-byte ledger id.
   * @param capabilityBytes   The same wire-format bytes the
   *                          server holds — mandatory for the
   *                          per-blob salt of `computeJoinerInviteTag`.
   * @param joinerAccountId   The joiner's 16-byte UUIDv7
   *                          account_id. Never crosses the wire
   *                          in plaintext.
   */
  async consumeCallInvite(
    blobId: Uint8Array,
    capabilityBytes: Uint8Array,
    joinerAccountId: Uint8Array,
  ): Promise<ConsumeInviteResult> {
    await requireLedgerId(blobId);
    if (joinerAccountId.length !== 16) {
      throw new DiscoveryClientArgError("joinerAccountId must be 16 bytes");
    }

    const wasm = await loadWasm();
    const joinerTag = wasm.computeJoinerInviteTag(
      capabilityBytes,
      joinerAccountId,
    );

    const slug = base64UrlEncodeNoPad(blobId);

    return this.consumeLedger(`/discovery/call-invite/${slug}/consume`, joinerTag);
  }

  /** `POST /discovery/group-invite` — upload a verified
   *  `GroupInviteCapability` blob, get the server-assigned
   *  `blob_id` back (ADR-0028, G-3). The high-level
   *  [`uploadGroupInvite`] wrapper validates the input. */
  async groupInviteUpload(
    req: GroupInviteUploadRequest,
  ): Promise<InviteUploadResponse> {
    return this.post("/discovery/group-invite", req);
  }

  /**
   * Upload a sealed `/g/` link blob (`composeGroupInviteCapability*`'s
   * `InviteLinkMint`) and get its server-assigned `blob_id` back (ADR-0028).
   * `maxUses` is a SOFT anti-spam throttle (`0` = single-use, `0xFFFFFFFF` =
   * unlimited), not part of the signed capability; the admin approval gate is
   * the security boundary.
   */
  async uploadGroupInvite(upload: SealedInviteUpload): Promise<Uint8Array> {
    requireSealedUpload(upload, MAX_SEALED_GROUP_INVITE_BYTES);
    const resp = await this.groupInviteUpload(sealedUploadBody(upload));
    return decodeServerLedgerId(resp.blob_id, "/discovery/group-invite");
  }

  /**
   * Fetch a previously-uploaded `GroupInviteCapability` by its
   * server-assigned `blob_id` (ADR-0028, G-3).
   *
   * Sibling of [`fetchCallInvite`]. Returns `null` on 404 (server folds
   * expired blobs into the same status to deny attacker status-code
   * probing). Returns the SEALED link blob; the caller opens it with the fragment's
   * secret (`wasm.GroupInviteCapability.openLink`), so a blob swapped in under
   * a live id opens nothing. Throws `DiscoveryRateLimitedError` on
   * 429 (per-address rate limit).
   */
  async fetchGroupInvite(blobId: Uint8Array): Promise<Uint8Array | null> {
    await requireLedgerId(blobId);
    const slug = base64UrlEncodeNoPad(blobId);
    const url = `${this.baseUrl}/discovery/group-invite/${slug}`;
    let response: Response;
    try {
      response = await fetch(url, { method: "GET" });
    } catch (err) {
      throw new DiscoveryNetworkError("/discovery/group-invite (GET)", err);
    }
    if (response.status === 429) throw DiscoveryRateLimitedError.from(response, "/discovery/group-invite (GET)");
    if (response.status === 404) return null;
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(
        response.status,
        "/discovery/group-invite (GET)",
        text,
      );
    }
    const raw = (await response.json()) as InviteFetchResponse;
    return base64Decode(raw.sealed);
  }

  /**
   * Atomically bump the consume counter for the group-invite at `blobId`
   * with per-joiner dedup (ADR-0028, G-3).
   *
   * Sibling of [`consumeCallInvite`]. The discriminated result shape is
   * identical: `"consumed"` / `"already-consumed"` (counter NOT bumped) /
   * `"exhausted"` (410) / `"not-found"` (404, never-issued OR expired). The
   * consume is anti-spam defence-in-depth — the admin approval gate
   * (ADR-0027) remains the security boundary.
   *
   * Reuses the **same** `wasm.computeJoinerInviteTag(...)` helper as the
   * contact- and call-invite flows — salt is per-blob, so tags don't
   * collide across invite kinds.
   *
   * @param joinerAccountId  The joiner's 16-byte UUIDv7 account_id. Never
   *                         crosses the wire in plaintext.
   */
  async consumeGroupInvite(
    blobId: Uint8Array,
    capabilityBytes: Uint8Array,
    joinerAccountId: Uint8Array,
  ): Promise<ConsumeInviteResult> {
    await requireLedgerId(blobId);
    if (joinerAccountId.length !== 16) {
      throw new DiscoveryClientArgError("joinerAccountId must be 16 bytes");
    }

    const wasm = await loadWasm();
    const joinerTag = wasm.computeJoinerInviteTag(
      capabilityBytes,
      joinerAccountId,
    );

    const slug = base64UrlEncodeNoPad(blobId);

    return this.consumeLedger(`/discovery/group-invite/${slug}/consume`, joinerTag);
  }

  /**
   * Owner-signed revocation of an invite blob (Phase 1.18d2b-web).
   *
   * Driven by the same Ed25519 key that signed the capability:
   * `wasm.signInviteRevocation(seed, blobId, signedAtMs)`
   * builds the canonical bytes (`REVOKE_DOMAIN_V1 || blob_id ||
   * signed_at_ms_be`) and signs. We POST `signed_at_ms +
   * signature` as the DELETE body; server recovers the
   * inviter's pubkey from the stored capability bytes and
   * verifies the signature against the same canonical layout.
   *
   * Replay window: the server enforces `±10 min` against its
   * own clock. The web client uses `Date.now()` directly — a
   * machine with a clock more than ~10 min skewed will see
   * 400-bad-window failures, mirroring the consume path.
   *
   * Returns a discriminated [`RevokeInviteResult`]:
   *   * `"revoked"` on 204
   *   * `"not-found-or-not-owner"` on 404 (server folds "no
   *     row" / "bad signature" / "stored bytes failed
   *     re-decode" into one status to deny status-code
   *     probing)
   *
   * Other non-2xx still throw `DiscoveryHttpError`.
   *
   * @param blobId      Raw 6-byte ledger id (the
   *                    same value `uploadInvite` returned).
   * @param signingSeed The owner's 32-byte Ed25519 seed —
   *                    typically `chats.identity().ed25519Seed`.
   */
  async revokeInvite(
    blobId: Uint8Array,
    signingSeed: Uint8Array,
  ): Promise<RevokeInviteResult> {
    await requireLedgerId(blobId);
    if (signingSeed.length !== 32) {
      throw new DiscoveryClientArgError("signingSeed must be 32 bytes");
    }

    const wasm = await loadWasm();
    // Bind the timestamp once so we sign the SAME value we
    // post; reading `Date.now()` twice could in principle yield
    // distinct values across the await boundary. The server's
    // ±10-min window absorbs the millisecond difference, but
    // pinning the value here is cheap and removes the doubt.
    const signedAtMs = Date.now();
    const signature = wasm.signInviteRevocation(
      signingSeed,
      blobId,
      BigInt(signedAtMs),
    );
    const slug = base64UrlEncodeNoPad(blobId);
    try {
      await this.inviteRevoke(slug, {
        signed_at_ms: signedAtMs,
        signature: base64Encode(signature),
      });
      return { kind: "revoked" };
    } catch (err) {
      if (err instanceof DiscoveryHttpError && err.status === 404) {
        return { kind: "not-found-or-not-owner" };
      }
      throw err;
    }
  }

  /**
   * Owner-signed revocation of a [`CallInviteCapability`] blob
   * (Phase 3c3-revoke).
   *
   * Sibling of [`revokeInvite`] for the call-invite ecosystem.
   * Driven by the same Ed25519 key that signed the capability:
   * `wasm.signCallInviteRevocation(seed, blobId, signedAtMs)`
   * builds the canonical bytes
   * (`CALL_INVITE_REVOKE_DOMAIN_V1 || blob_id ||
   * signed_at_ms_be`) and signs. The distinct domain separator
   * (vs. contact-invite revoke) ensures a captured signature
   * for one kind cannot revoke a blob of the other kind.
   *
   * Replay window: server enforces `±10 min`
   * (`CALL_REVOKE_REPLAY_WINDOW_MS`) against its own clock.
   *
   * Returns a discriminated [`RevokeInviteResult`]:
   *   * `"revoked"` on 204
   *   * `"not-found-or-not-owner"` on 404 (server folds "no
   *     row" / "bad signature" / "stored bytes failed
   *     re-decode" into one status)
   *
   * @param blobId      Raw 6-byte ledger id (the
   *                    same value `uploadCallInvite` returned).
   * @param signingSeed The owner's 32-byte Ed25519 seed —
   *                    typically `chats.identity().ed25519Seed`.
   */
  async revokeCallInvite(
    blobId: Uint8Array,
    signingSeed: Uint8Array,
  ): Promise<RevokeInviteResult> {
    await requireLedgerId(blobId);
    if (signingSeed.length !== 32) {
      throw new DiscoveryClientArgError("signingSeed must be 32 bytes");
    }

    const wasm = await loadWasm();
    const signedAtMs = Date.now();
    const signature = wasm.signCallInviteRevocation(
      signingSeed,
      blobId,
      BigInt(signedAtMs),
    );
    const slug = base64UrlEncodeNoPad(blobId);
    try {
      await this.callInviteRevoke(slug, {
        signed_at_ms: signedAtMs,
        signature: base64Encode(signature),
      });
      return { kind: "revoked" };
    } catch (err) {
      if (err instanceof DiscoveryHttpError && err.status === 404) {
        return { kind: "not-found-or-not-owner" };
      }
      throw err;
    }
  }

  /** ADR-0028 revoke — wire-level `DELETE /discovery/group-invite/{blob_id}`.
   *  Sibling of [`callInviteRevoke`]; the body carries the issuer-signed
   *  revocation. The high-level [`revokeGroupInvite`] wrapper drives the sign. */
  async groupInviteRevoke(blobIdSlug: string, req: InviteRevokeRequest): Promise<void> {
    const endpoint = `/discovery/group-invite/${blobIdSlug}`;
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "DELETE",
        headers: {
          "content-type": "application/json",
          ...this.authHeaders(),
        },
        body: JSON.stringify(req),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(`${endpoint} (DELETE)`, err);
    }
    if (response.status === 204) return;
    const text = await safeReadBody(response);
    throw new DiscoveryHttpError(response.status, `${endpoint} (DELETE)`, text);
  }

  /** ADR-0028 revoke — issuer-signed delete of a group-invite blob. Signs the
   *  group-invite revoke canonical bytes with `signingSeed` (the issuer's
   *  ed25519 seed) and DELETEs the ledger row; the server verifies against the
   *  stored capability's issuer pubkey. 404 folds into `not-found-or-not-owner`
   *  (a wrong signer / already-deleted row). */
  async revokeGroupInvite(
    blobId: Uint8Array,
    signingSeed: Uint8Array,
  ): Promise<RevokeInviteResult> {
    await requireLedgerId(blobId);
    if (signingSeed.length !== 32) {
      throw new DiscoveryClientArgError("signingSeed must be 32 bytes");
    }
    const wasm = await loadWasm();
    const signedAtMs = Date.now();
    const signature = wasm.signGroupInviteRevocation(
      signingSeed,
      blobId,
      BigInt(signedAtMs),
    );
    const slug = base64UrlEncodeNoPad(blobId);
    try {
      await this.groupInviteRevoke(slug, {
        signed_at_ms: signedAtMs,
        signature: base64Encode(signature),
      });
      return { kind: "revoked" };
    } catch (err) {
      if (err instanceof DiscoveryHttpError && err.status === 404) {
        return { kind: "not-found-or-not-owner" };
      }
      throw err;
    }
  }

  /**
   * Owner-signed list of every live invite this account owns
   * (Phase 1.18d2b-list).
   *
   * Driven by the same Ed25519 key that signed every
   * `InviteCapability` the owner has uploaded. The canonical
   * bytes (`OWNER_LIST_DOMAIN_V1 || owner_account_id ||
   * signed_at_ms_be`) are signed via
   * `wasm.signOwnerListQuery`; server resolves the verifying
   * key from `identity_bundles.signature_pubkey` for the
   * supplied `account_id` and verifies before returning rows.
   *
   * Distinct domain separator from
   * [`signInviteRevocation`] ensures a captured revoke
   * signature can't be replayed against this endpoint.
   *
   * Replay window: server enforces `±10 min` against its own
   * clock — same constant as the revoke flow.
   *
   * Returns a discriminated [`ListInvitesResult`]:
   *   * `"list"` with the parsed rows on 200 (empty array
   *     when the owner has no live invites — distinct from a
   *     malformed-request failure)
   *   * `"not-found-or-not-owner"` on 404 (server folds "no
   *     identity bundle for `account_id`" / "signature
   *     mismatch" / "stored pubkey malformed" into one
   *     status to deny status-code probing)
   *
   * Other non-2xx still throw `DiscoveryHttpError`.
   *
   * @param accountId    The owner's 16-byte UUIDv7 account_id
   *                     (matches `IdentitySecrets.accountId`).
   * @param signingSeed  The owner's 32-byte Ed25519 seed —
   *                     typically `chats.identity().ed25519Seed`.
   */
  async listInvitesByOwner(
    accountId: Uint8Array,
    signingSeed: Uint8Array,
  ): Promise<ListInvitesResult> {
    if (accountId.length !== 16) {
      throw new DiscoveryClientArgError("accountId must be 16 bytes");
    }
    if (signingSeed.length !== 32) {
      throw new DiscoveryClientArgError("signingSeed must be 32 bytes");
    }

    const wasm = await loadWasm();
    // Bind the timestamp once so the wire body's `signed_at_ms`
    // matches the canonical bytes we just signed — same
    // single-source pattern as `revokeInvite`.
    const signedAtMs = Date.now();
    const signature = wasm.signOwnerListQuery(
      signingSeed,
      accountId,
      BigInt(signedAtMs),
    );

    const raw = await this.inviteOwnerList({
      owner_account_id: base64Encode(accountId),
      signed_at_ms: signedAtMs,
      signature: base64Encode(signature),
    });
    if (raw === null) {
      return { kind: "not-found-or-not-owner" };
    }
    const invites = raw.invites.map((row) => ({
      blobIdSlug: row.blob_id_slug,
      expiryMs: row.expiry_ms,
      maxUses: row.max_uses,
      usedCount: row.used_count,
      label: row.label,
      bootstrapReferral: row.bootstrap_referral,
      createdAt: row.created_at,
    }));
    return { kind: "list", invites };
  }

  /**
   * Resolve `@username` to its raw 16-byte `account_id`, or
   * `null` when the username is not registered.
   *
   * Same flow as `registerUsername` modulo the second endpoint:
   * OPRF (one anonymous token) → POST `/discovery/lookup`.
   */
  async lookup(username: string): Promise<LookupResult> {
    const wasm = await loadWasm();
    const blinder = wasm.OprfBlinder.blind(username);
    const normalisedUsername = blinder.normalizedUsername;

    const oprfResp = await this.oprfEvaluate({ blinded: blinder.blindedB64 });

    // ADR-0060 section 5: discovery receives the lookup key only; the raw
    // OPRF output stays on this device (it keys sealed records).
    const lookupKeyB64 = base64Encode(
      finalizeOprf(blinder, oprfResp.evaluated).lookupKey,
    );

    const lookupResp = await this.lookupRaw({
      oprf_hash: lookupKeyB64,
    });
    if (lookupResp === null) {
      return { normalisedUsername, account: null };
    }
    return {
      normalisedUsername,
      account: base64Decode(lookupResp.account_id),
    };
  }

  // -----------------------------------------------------------------
  // High-level: @channel discovery (G4-5, ADR-0010)
  //
  // The channel namespace reuses the SAME OPRF flow as @username
  // (the `/discovery/oprf` evaluate endpoint is namespace-agnostic
  // — one static key over the blinded handle). Only the second
  // endpoint differs: `/discovery/channel/{register,lookup}`
  // instead of `/discovery/{register,lookup}`, and the id type is
  // a 16-byte channel_id pseudonym rather than a 16-byte account
  // UUIDv7.
  // -----------------------------------------------------------------

  /**
   * Bind `@channelname` to a 16-byte `channel_id`.
   *
   * Flow: OPRF blind → POST `/discovery/oprf` (one `discovery-lookup`
   * token) → finalize → POST `/discovery/channel/register` (one
   * `discovery-register` token). Identical to
   * [`registerUsername`] modulo the id type + second endpoint.
   *
   * Handle normalisation (NFKC + lowercase + trim + leading-`@`
   * strip) happens inside `OprfBlinder.blind`; the canonical form
   * is surfaced back via `ChannelRegisterResult.normalisedHandle`.
   *
   * The write is session-token-gated (a DoS rate-limit role, not
   * an ownership proof — see `services/discovery/src/channel_names.rs`).
   * Throws `DiscoveryHttpError` with `status === 409` when the
   * handle is already registered (first-come-first-served).
   */
  async registerChannelName(
    channelId: Uint8Array,
    handle: string,
  ): Promise<ChannelRegisterResult> {
    if (channelId.length !== 16) {
      throw new DiscoveryClientArgError("channelId must be 16 bytes");
    }
    const wasm = await loadWasm();
    const blinder = wasm.OprfBlinder.blindChannel(handle);
    const normalisedHandle = blinder.normalizedUsername;

    const oprfResp = await this.oprfEvaluate({ blinded: blinder.blindedB64 });

    // ADR-0060 section 5: discovery receives the lookup key only; the raw
    // OPRF output stays on this device (it keys sealed records).
    const lookupKeyB64 = base64Encode(
      finalizeOprf(blinder, oprfResp.evaluated).lookupKey,
    );

    const registerResp = await this.channelRegister({
      oprf_hash: lookupKeyB64,
      channel_id: base64Encode(channelId),
    });
    if (!registerResp.ok) {
      throw new DiscoveryHttpError(
        200,
        "/discovery/channel/register",
        "server reported ok=false",
      );
    }
    return { normalisedHandle };
  }

  /**
   * Resolve `@channelname` to its raw 16-byte `channel_id`, or
   * `null` when the handle is not registered.
   *
   * Same flow as [`registerChannelName`] modulo the second
   * endpoint: OPRF → POST `/discovery/channel/lookup`.
   * Exact-match only (the server does no prefix search).
   */
  async lookupChannel(handle: string): Promise<ChannelLookupResult> {
    const wasm = await loadWasm();
    const blinder = wasm.OprfBlinder.blindChannel(handle);
    const normalisedHandle = blinder.normalizedUsername;

    const oprfResp = await this.oprfEvaluate({ blinded: blinder.blindedB64 });

    // ADR-0060 section 5: discovery receives the lookup key only; the raw
    // OPRF output stays on this device (it keys sealed records).
    const lookupKeyB64 = base64Encode(
      finalizeOprf(blinder, oprfResp.evaluated).lookupKey,
    );

    const lookupResp = await this.channelLookupRaw({
      oprf_hash: lookupKeyB64,
    });
    if (lookupResp === null) {
      return { normalisedHandle, channel: null };
    }
    return {
      normalisedHandle,
      channel: base64Decode(lookupResp.channel_id),
    };
  }

  // -----------------------------------------------------------------
  // Internals
  // -----------------------------------------------------------------

  /**
   * One request on an anonymous-token route: spends one token of `context`
   * from the pool, sends NO session (`anonymousFetchInit`), and retries once
   * with another token when the route refuses the first as spent / invalid /
   * expired. Returns the final response; transport failures become
   * `DiscoveryNetworkError`.
   */
  private async tokenPost(
    context: AnonTokenContext,
    endpoint: string,
    body: unknown,
  ): Promise<Response> {
    const url = `${this.baseUrl}${endpoint}`;
    const json = JSON.stringify(body);
    try {
      return await this.tokens.spend(context, (headerName, headerValue) =>
        fetch(
          url,
          anonymousFetchInit({
            method: "POST",
            headers: { "content-type": "application/json", [headerName]: headerValue },
            body: json,
          }),
        ),
      );
    } catch (err) {
      if (err instanceof TypeError) throw new DiscoveryNetworkError(endpoint, err);
      throw err;
    }
  }

  /** {@link tokenPost} for a route whose 2xx is JSON. */
  private async tokenPostJson<Res>(
    context: AnonTokenContext,
    endpoint: string,
    body: unknown,
  ): Promise<Res> {
    const response = await this.tokenPost(context, endpoint, body);
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(response.status, endpoint, text);
    }
    return (await response.json()) as Res;
  }

  /** The three ledger consumes (ADR-0028): one `invite-consume` token, the
   *  joiner tag, and the 200 / 404 / 410 / 429 fold. */
  private async consumeLedger(
    endpoint: string,
    joinerTag: Uint8Array,
  ): Promise<ConsumeInviteResult> {
    const response = await this.tokenPost("invite-consume", endpoint, {
      joiner_tag: base64Encode(joinerTag),
    } satisfies InviteConsumeRequest);
    if (response.status === 429) throw DiscoveryRateLimitedError.from(response, endpoint);
    if (response.status === 404) return { kind: "not-found" };
    if (response.status === 410) return { kind: "exhausted" };
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(response.status, endpoint, text);
    }
    const raw = (await response.json()) as InviteConsumeResponse;
    return {
      kind: raw.already_consumed ? "already-consumed" : "consumed",
      usedCount: raw.used_count,
      maxUses: raw.max_uses,
    };
  }

  private async post<Req, Res>(endpoint: string, body: Req): Promise<Res> {
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      // Phase 1.5b: every POST through here carries the Authorization
      // header when a session token is set. The anonymous-token routes
      // never come through here (ADR-0054 S3, see `tokenPost`).
      const baseHeaders =
        body === undefined ? {} : { "content-type": "application/json" };
      // ADR-0065: through the session-aware fetch, which refreshes a
      // managed bearer ahead of expiry and retries once on a 401.
      response = await sessionFetch(url, {
        method: "POST",
        headers: { ...baseHeaders, ...this.authHeaders() },
        body: body === undefined ? null : JSON.stringify(body),
      });
    } catch (err) {
      throw new DiscoveryNetworkError(endpoint, err);
    }

    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new DiscoveryHttpError(response.status, endpoint, text);
    }
    return (await response.json()) as Res;
  }
}

async function safeReadBody(response: Response): Promise<string> {
  try {
    const text = await response.text();
    return text.length > 512 ? `${text.slice(0, 512)}…` : text;
  } catch {
    return "<unreadable body>";
  }
}
