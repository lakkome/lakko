// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `WebauthnClient` — fetch-based wrapper for the
 * `/webauthn/*` routes in `lakko-svc-auth` (Phase 2a2c).
 *
 * Glues the browser-side `navigator.credentials` API into the
 * server's `webauthn-rs` JSON surface. The server hands us
 * challenges with `base64url-no-pad` strings in every byte-array
 * slot (`challenge`, `user.id`, `allowCredentials[].id`,
 * `excludeCredentials[].id`); the WebAuthn JS API wants those as
 * `BufferSource`. Conversion happens here so the call sites stay
 * shape-symmetric with the rest of the auth surface.
 *
 * Surfaced operations:
 *
 *   * [`registerPasskey`] — start + finish a registration round
 *     for an authenticated session. Drives
 *     `navigator.credentials.create({ publicKey })` between the
 *     two server calls.
 *   * [`listCredentials`] — read the user's stored credentials.
 *   * [`deleteCredential`] — remove a credential by id.
 *   * [`loginWithPasskey`] — anonymous login flow. Drives
 *     `navigator.credentials.get({ publicKey })` and returns the
 *     same `(account_id, session_token, session_expires_at_ms)`
 *     triple OPAQUE login does, so the AuthContext can flip the
 *     session state with the same code path.
 *   * [`isWebauthnAvailable`] — quick capability check the UI
 *     uses to decide whether to render the passkey affordance.
 *
 * Errors fold into `WebauthnHttpError` (non-2xx response),
 * `WebauthnUnsupportedError` (browser missing
 * `navigator.credentials`), `WebauthnUserCancelledError`
 * (`NotAllowedError` from the authenticator), or
 * `WebauthnNetworkError` (fetch failure). The UI branches on
 * these to surface the right toast.
 */

import { AccountSuspendedError, DEFAULT_BASE_URL } from "./client";
import { isSuspendedBody, suspendedLandingOf } from "./suspension";
import { sessionFetch } from "./session-fetch";
import type { SessionPairWire } from "./session-pair";
import { FRESH_GRANT_HEADER } from "./fresh-auth";

// ---- Errors ------------------------------------------------------

/** Non-2xx response from the auth service. */
export class WebauthnHttpError extends Error {
  override name = "WebauthnHttpError";
  constructor(
    public readonly status: number,
    public readonly endpoint: string,
    public readonly body: string,
  ) {
    super(`webauthn ${endpoint} → ${status}: ${body}`);
  }
}

/** Network failure (DNS / connectivity / TLS). */
export class WebauthnNetworkError extends Error {
  override name = "WebauthnNetworkError";
  constructor(
    public readonly endpoint: string,
    cause: unknown,
  ) {
    super(`webauthn ${endpoint} network error: ${String(cause)}`);
  }
}

/** Browser lacks `navigator.credentials` or it returned an
 *  unsupported error. UI prompts the user to upgrade / use a
 *  different sign-in. */
export class WebauthnUnsupportedError extends Error {
  override name = "WebauthnUnsupportedError";
}

/** User dismissed the platform sheet, or the authenticator
 *  refused (no matching credential, locked, etc.). The DOM
 *  surface folds all of these into a single `NotAllowedError`;
 *  we mirror that here. */
export class WebauthnUserCancelledError extends Error {
  override name = "WebauthnUserCancelledError";
}

// ---- Wire shapes (mirror server-side `webauthn.rs`) -------------

export interface RegisterStartRequest {
  /** Human-visible label that surfaces in the authenticator UI
   *  (the user's password manager shows this when picking a
   *  passkey). Server does NOT store it. */
  readonly user_name: string;
  /** Bookkeeping label that lands on the credential row server-
   *  side ("MacBook Touch ID"). Visible in
   *  [`listCredentials`]. */
  readonly nickname: string;
  /** The fresh-auth grant (`fresh-auth.ts`): adding a passkey needs the
   *  password or a passkey proven just now. `403 fresh-auth-required`
   *  without a usable one. Sent on the finish as well. */
  readonly fresh_grant: string;
}

export interface RegisterStartResponse {
  readonly state_id: string;
  readonly nickname: string;
  /** Raw `CreationChallengeResponse` from webauthn-rs. The
   *  `publicKey` shape inside `options.publicKey` is what
   *  `navigator.credentials.create` accepts, but every byte
   *  array is base64url-string. [`encodeCreationOptions`]
   *  translates. */
  readonly options: ServerCreationOptions;
}

/** What webauthn-rs serialises for a `CreationChallengeResponse`.
 *  Shape mirrors the WebAuthn level-2 spec but with base64url
 *  strings in every BufferSource slot. */
interface ServerCreationOptions {
  readonly publicKey: ServerPublicKeyCredentialCreationOptions;
}

interface ServerPublicKeyCredentialCreationOptions {
  readonly rp: { readonly id: string; readonly name: string };
  readonly user: {
    readonly id: string; // base64url
    readonly name: string;
    readonly displayName: string;
  };
  readonly challenge: string; // base64url
  readonly pubKeyCredParams: readonly { readonly type: string; readonly alg: number }[];
  readonly timeout?: number;
  readonly excludeCredentials?: readonly {
    readonly type: string;
    readonly id: string; // base64url
    readonly transports?: readonly string[];
  }[];
  readonly authenticatorSelection?: {
    readonly authenticatorAttachment?: "platform" | "cross-platform";
    readonly residentKey?: "discouraged" | "preferred" | "required";
    readonly requireResidentKey?: boolean;
    readonly userVerification?: "discouraged" | "preferred" | "required";
  };
  readonly attestation?: "none" | "indirect" | "direct" | "enterprise";
  readonly extensions?: Record<string, unknown>;
}

export interface RegisterFinishRequest {
  /** The same fresh-auth grant the start carried. */
  readonly fresh_grant: string;
  readonly state_id: string;
  readonly nickname: string;
  readonly credential: RegisterPublicKeyCredentialJSON;
}

export interface RegisterFinishResponse {
  readonly credential_id_b64: string;
}

/** The JSON shape webauthn-rs's `RegisterPublicKeyCredential`
 *  expects. */
interface RegisterPublicKeyCredentialJSON {
  readonly id: string; // base64url
  readonly rawId: string; // base64url
  readonly type: "public-key";
  readonly response: {
    readonly clientDataJSON: string; // base64url
    readonly attestationObject: string; // base64url
    readonly transports?: readonly string[];
  };
  readonly extensions?: Record<string, unknown>;
}

export interface CredentialSummary {
  readonly credential_id_b64: string;
  readonly nickname: string;
  readonly created_at_ms: number | null;
  readonly last_used_at_ms: number | null;
}

export interface ListCredentialsResponse {
  readonly credentials: readonly CredentialSummary[];
}

/** A3 — `GET /account/export` shape: what `services/auth` holds about the
 *  account (metadata + auth-factor existence; never the factor secrets). */
export interface AccountExport {
  readonly account_id_b64: string;
  readonly created_at_ms: number;
  readonly identity_version: number;
  readonly passkey_count: number;
  readonly has_totp: boolean;
}

export interface AuthStartRequest {
  /** Optional. When set, server returns an allow-list challenge
   *  for that account. When omitted, server returns a
   *  discoverable challenge (the authenticator picks the
   *  passkey). */
  readonly account_id_b64?: string;
}

export interface AuthStartResponse {
  readonly state_id: string;
  readonly discoverable: boolean;
  readonly options: ServerRequestOptions;
}

interface ServerRequestOptions {
  readonly publicKey: ServerPublicKeyCredentialRequestOptions;
}

interface ServerPublicKeyCredentialRequestOptions {
  readonly challenge: string; // base64url
  readonly timeout?: number;
  readonly rpId?: string;
  readonly allowCredentials?: readonly {
    readonly type: string;
    readonly id: string; // base64url
    readonly transports?: readonly string[];
  }[];
  readonly userVerification?: "discouraged" | "preferred" | "required";
  readonly extensions?: Record<string, unknown>;
}

export interface AuthFinishRequest {
  readonly state_id: string;
  readonly credential: AuthPublicKeyCredentialJSON;
  /** ADR-0074 section 0.4: this device's id; absent = an unbound session. */
  readonly device_id_b64?: string;
}

/** `POST /webauthn/auth/finish`: the account id plus the session pair
 *  (ADR-0073) every login route returns. */
export interface AuthFinishResponse extends SessionPairWire {
  readonly account_id_b64: string;
  /** The fresh-auth grant a passkey sign-in mints (`fresh-auth.ts`). */
  readonly fresh_grant?: string;
  readonly fresh_grant_expires_at_ms?: number;
}

interface AuthPublicKeyCredentialJSON {
  readonly id: string;
  readonly rawId: string;
  readonly type: "public-key";
  readonly response: {
    readonly clientDataJSON: string;
    readonly authenticatorData: string;
    readonly signature: string;
    readonly userHandle: string | null;
  };
  readonly extensions?: Record<string, unknown>;
}

// ---- Action binding (G-13, ADR-0014) ----------------------------
//
// Action binding elevates a *specific* sensitive operation to require a
// fresh passkey gesture: the server issues a challenge that is a function
// of an opaque 32-byte `action_hash`, so the authenticator's signature
// attests to THAT action. `assertAction` drives the
// `/auth/action/{start,finish}` ceremony; the caller computes the
// `action_hash` with one of the `actionHashFor*` helpers below and runs
// the ceremony as a gate *before* performing the operation.
//
// NOTE: this is the client-side gate (it forces a live passkey gesture in
// the app flow). Server-side endpoint enforcement — the sensitive endpoint
// itself demanding a completed ceremony — is a tracked follow-up; for the
// DeviceGroup remove-commit it is additionally bounded by the DS's
// zero-trust opacity (the DS cannot interpret an action_hash).

/** Domain separators bound into each action's `action_hash`. Keep these in
 *  lockstep with the iOS / Android clients so the stored hash means the
 *  same thing on every platform. */
export const ACTION_DOMAIN_DELETE_CREDENTIAL = "lakko/action/delete-credential/v1";
export const ACTION_DOMAIN_REMOVE_DEVICE = "lakko/action/remove-device/v1";
export const ACTION_DOMAIN_DELETE_ACCOUNT = "lakko/action/delete-account/v1";
/** ADR-0072 decision 6: letting one passkey restore the account's keys. The
 *  server does not consume a grant for this action today (ADR-0072 open
 *  question 4); the ceremony is what yields the passkey's PRF output inside
 *  a signed-in session, and it is bound to the action it serves. */
export const ACTION_DOMAIN_PASSKEY_RESTORES_KEYS = "lakko/action/passkey-restores-keys/v1";

export interface ActionStartRequest {
  /** URL-safe base64 of the 32-byte action hash. */
  readonly action_hash_b64: string;
}

export interface ActionStartResponse {
  readonly state_id: string;
  readonly options: ServerRequestOptions;
}

export interface ActionFinishRequest {
  readonly state_id: string;
  readonly action_hash_b64: string;
  readonly credential: AuthPublicKeyCredentialJSON;
}

export interface ActionFinishResponse {
  readonly account_id_b64: string;
  /** Echo of the action hash the passkey attested to. */
  readonly action_hash_b64: string;
  readonly credential_id_b64: string;
  /** URL-safe base64 of the signed `ActionAttestation` (G-13, ADR-0015).
   *  The remove-device flow carries this alongside its MLS Remove commit so
   *  peers can verify it; the delete-passkey flow ignores it (the server
   *  enforces that one via a single-use grant). */
  readonly attestation_b64: string;
}

// ---- Client ------------------------------------------------------

export class WebauthnClient {
  private sessionToken: string | null = null;

  constructor(public readonly baseUrl: string = DEFAULT_BASE_URL) {}

  /** Bind / rebind the bearer token used on the four authenticated
   *  routes. `null` clears. */
  setSessionToken(token: string | null): void {
    this.sessionToken = token;
  }

  /** Quick capability probe. Returns `true` only when the
   *  browser exposes `navigator.credentials.create/get` AND the
   *  WebAuthn `PublicKeyCredential` global is present. */
  static isWebauthnAvailable(): boolean {
    return (
      typeof window !== "undefined" &&
      typeof window.PublicKeyCredential === "function" &&
      typeof navigator !== "undefined" &&
      typeof navigator.credentials !== "undefined" &&
      typeof navigator.credentials.create === "function" &&
      typeof navigator.credentials.get === "function"
    );
  }

  /**
   * Drive the full passkey-registration flow:
   *
   * 1. `POST /webauthn/register/start` with `(user_name,
   *    nickname)`. Server returns `(state_id, options)`.
   * 2. `navigator.credentials.create({publicKey: options})`.
   * 3. `POST /webauthn/register/finish` with the resulting
   *    credential blob.
   *
   * Caller must have set the session token first via
   * [`setSessionToken`]. Throws [`WebauthnUnsupportedError`] when
   * the browser lacks the API, [`WebauthnUserCancelledError`]
   * when the user dismisses the sheet, or a [`WebauthnHttpError`]
   * / [`WebauthnNetworkError`] on a server hiccup.
   */
  async registerPasskey(
    req: RegisterStartRequest,
    opts: { prfSalt?: Uint8Array } = {},
  ): Promise<RegisterFinishResponse & PasskeyCeremonyExtras> {
    if (!WebauthnClient.isWebauthnAvailable()) {
      throw new WebauthnUnsupportedError();
    }
    const start = await this.post<RegisterStartRequest, RegisterStartResponse>(
      "/webauthn/register/start",
      req,
    );
    const publicKey = decodeCreationOptions(start.options.publicKey);
    if (opts.prfSalt !== undefined) {
      publicKey.extensions = { ...(publicKey.extensions ?? {}), ...prfExtensionInput(opts.prfSalt) };
    }
    let cred: Credential | null;
    try {
      cred = await navigator.credentials.create({ publicKey });
    } catch (err) {
      throw mapDomError(err);
    }
    if (cred === null) {
      throw new WebauthnUserCancelledError();
    }
    const pk = cred as PublicKeyCredential;
    const finish = await this.post<RegisterFinishRequest, RegisterFinishResponse>(
      "/webauthn/register/finish",
      {
        fresh_grant: req.fresh_grant,
        state_id: start.state_id,
        nickname: start.nickname,
        credential: encodeRegisterCredential(pk),
      },
    );
    return { ...finish, ...ceremonyExtras(pk) };
  }

  /** `GET /webauthn/credentials` — bearer-auth required. */
  async listCredentials(): Promise<ListCredentialsResponse> {
    const response = await this.fetchAuthed("/webauthn/credentials", {
      method: "GET",
    });
    return parseJsonOrThrow(response, "/webauthn/credentials");
  }

  /** `DELETE /webauthn/credentials/:id` — bearer-auth required. */
  async deleteCredential(credentialIdB64: string): Promise<void> {
    const endpoint = `/webauthn/credentials/${encodeURIComponent(credentialIdB64)}`;
    const response = await this.fetchAuthed(endpoint, {
      method: "DELETE",
    });
    if (!response.ok) {
      const body = await safeReadBody(response);
      throw new WebauthnHttpError(response.status, endpoint, body);
    }
  }

  /** A3 (GDPR erasure) — delete the whole account on `services/auth`
   *  (`DELETE /account`, FK-cascade over all auth state). Bearer-auth. When the
   *  account has a passkey the server requires the ADR-0014 action grant, so
   *  the caller must run [`assertAction`] with [`actionHashForDeleteAccount`]
   *  first (a plain-text 403 `action-authorization-required` surfaces here if
   *  it is missing); that gesture is the proof and no fresh-auth grant is
   *  needed. An account without a passkey needs the fresh-auth grant. */
  async deleteAccount(freshGrant: string | null): Promise<void> {
    const endpoint = "/account";
    // The fresh-auth grant rides in a header (the route has no body). An
    // account without a passkey and without a usable one gets
    // `403 {"error":"fresh-auth-required"}`.
    const response = await this.fetchAuthed(endpoint, {
      method: "DELETE",
      ...(freshGrant === null ? {} : { headers: { [FRESH_GRANT_HEADER]: freshGrant } }),
    });
    if (!response.ok) {
      const body = await safeReadBody(response);
      throw new WebauthnHttpError(response.status, endpoint, body);
    }
  }

  /** A3 (GDPR portability) — `GET /account/export`. Returns what
   *  `services/auth` holds about the account (bearer-auth). */
  async exportAccount(): Promise<AccountExport> {
    const response = await this.fetchAuthed("/account/export", { method: "GET" });
    return parseJsonOrThrow(response, "/account/export");
  }

  /**
   * Drive the full passkey-login flow:
   *
   * 1. `POST /webauthn/auth/start`. If `accountIdB64` is set,
   *    server uses the allow-list path; otherwise the
   *    discoverable path.
   * 2. `navigator.credentials.get({publicKey: options})`.
   * 3. `POST /webauthn/auth/finish` with the asserted
   *    credential. Returns the `(account_id, session_token,
   *    session_expires_at_ms)` triple.
   *
   * No bearer token required — this is the login flow.
   *
   * `mediation` defaults to `"required"` (browser shows the
   * sheet). Set to `"conditional"` for autofill-style passkey
   * pickers (the future "type in field, browser suggests
   * passkey" path); we keep the API hook exposed but the
   * Phase 2a2c UI doesn't drive conditional mediation yet.
   */
  async loginWithPasskey(
    options: {
      accountIdB64?: string;
      mediation?: CredentialMediationRequirement;
      /** ADR-0072 decision 6: evaluate the PRF extension with this salt so the
       *  assertion also yields the credential's 32-byte secret. */
      prfSalt?: Uint8Array;
      /** ADR-0074 section 0.4: this device's id (URL-safe base64, no
       *  padding), when it is known for the account being signed in to;
       *  the session then names the device from the start. */
      deviceIdB64?: string;
    } = {},
  ): Promise<AuthFinishResponse & PasskeyCeremonyExtras> {
    if (!WebauthnClient.isWebauthnAvailable()) {
      throw new WebauthnUnsupportedError();
    }
    const start = await this.post<AuthStartRequest, AuthStartResponse>(
      "/webauthn/auth/start",
      options.accountIdB64 === undefined
        ? {}
        : { account_id_b64: options.accountIdB64 },
    );
    const publicKey = decodeRequestOptions(start.options.publicKey);
    if (options.prfSalt !== undefined) {
      publicKey.extensions = { ...(publicKey.extensions ?? {}), ...prfExtensionInput(options.prfSalt) };
    }
    let cred: Credential | null;
    try {
      const getOptions: CredentialRequestOptions = { publicKey };
      if (options.mediation !== undefined) {
        getOptions.mediation = options.mediation;
      }
      cred = await navigator.credentials.get(getOptions);
    } catch (err) {
      throw mapDomError(err);
    }
    if (cred === null) {
      throw new WebauthnUserCancelledError();
    }
    const pk = cred as PublicKeyCredential;
    const finish = await this.post<AuthFinishRequest, AuthFinishResponse>(
      "/webauthn/auth/finish",
      {
        state_id: start.state_id,
        credential: encodeAuthCredential(pk),
        ...(options.deviceIdB64 !== undefined ? { device_id_b64: options.deviceIdB64 } : {}),
      },
    );
    return { ...finish, ...ceremonyExtras(pk) };
  }

  /**
   * Drive the action-binding ceremony (G-13, ADR-0014):
   *
   * 1. `POST /auth/action/start` with the opaque `actionHashB64`.
   *    Server derives `challenge = H(action_hash ‖ nonce)`, returns
   *    `(state_id, options)`.
   * 2. `navigator.credentials.get({publicKey: options})` — the user
   *    performs the passkey gesture, signing over the action-bound
   *    challenge.
   * 3. `POST /auth/action/finish` with the same `action_hash_b64` and
   *    the asserted credential. Resolves only if the server verifies the
   *    signature attests to this action.
   *
   * Bearer-auth required ([`setSessionToken`]). Throws the same error
   * family as [`loginWithPasskey`] — in particular
   * [`WebauthnUserCancelledError`] when the user dismisses the sheet, so
   * the caller MUST treat a throw as "do not perform the action".
   */
  async assertAction(
    actionHashB64: string,
    options: {
      /** Evaluate the PRF extension with this salt, so the assertion also
       *  yields the credential's 32-byte secret (never sent to the server). */
      prfSalt?: Uint8Array;
      /** Offer only this credential (base64url, no pad) in the sheet. */
      onlyCredentialIdB64?: string;
    } = {},
  ): Promise<ActionFinishResponse & PasskeyCeremonyExtras> {
    if (!WebauthnClient.isWebauthnAvailable()) {
      throw new WebauthnUnsupportedError();
    }
    const start = await this.post<ActionStartRequest, ActionStartResponse>(
      "/auth/action/start",
      { action_hash_b64: actionHashB64 },
    );
    const publicKey = decodeRequestOptions(start.options.publicKey);
    if (options.onlyCredentialIdB64 !== undefined) {
      publicKey.allowCredentials = [
        { type: "public-key", id: base64urlToBuffer(options.onlyCredentialIdB64) },
      ];
    }
    if (options.prfSalt !== undefined) {
      publicKey.extensions = { ...(publicKey.extensions ?? {}), ...prfExtensionInput(options.prfSalt) };
    }
    let cred: Credential | null;
    try {
      cred = await navigator.credentials.get({ publicKey });
    } catch (err) {
      throw mapDomError(err);
    }
    if (cred === null) {
      throw new WebauthnUserCancelledError();
    }
    const pk = cred as PublicKeyCredential;
    const finish = await this.post<ActionFinishRequest, ActionFinishResponse>(
      "/auth/action/finish",
      {
        state_id: start.state_id,
        action_hash_b64: actionHashB64,
        credential: encodeAuthCredential(pk),
      },
    );
    return { ...finish, ...ceremonyExtras(pk) };
  }

  /**
   * The PRF output of ONE of the signed-in account's passkeys, obtained
   * inside the session (no new sign-in): the action ceremony bound to
   * "let this passkey restore keys", offered for that credential only, with
   * the PRF evaluation requested. `prfOutput` is `null` when the
   * authenticator or the browser does not evaluate PRF; the output never
   * leaves this device. The server verified the assertion, so the
   * credential that answered is one of the account's; the caller still
   * checks it is the one it asked for.
   */
  async evaluatePrfFor(
    credentialIdB64: string,
    prfSalt: Uint8Array,
  ): Promise<PasskeyCeremonyExtras> {
    const actionHashB64 = await deriveActionHash(
      ACTION_DOMAIN_PASSKEY_RESTORES_KEYS,
      new Uint8Array(base64urlToBuffer(credentialIdB64)),
    );
    const result = await this.assertAction(actionHashB64, {
      prfSalt,
      onlyCredentialIdB64: credentialIdB64,
    });
    return {
      credentialId: result.credentialId,
      prfOutput: result.prfOutput,
      prfEnabled: result.prfEnabled,
    };
  }

  // -----------------------------------------------------------------
  // Low-level transport
  // -----------------------------------------------------------------

  private async post<Req, Res>(endpoint: string, body: Req): Promise<Res> {
    const response = await this.fetchAuthed(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    return parseJsonOrThrow<Res>(response, endpoint);
  }

  private async fetchAuthed(endpoint: string, init: RequestInit): Promise<Response> {
    const url = `${this.baseUrl}${endpoint}`;
    const headers = new Headers(init.headers);
    if (this.sessionToken !== null) {
      headers.set("authorization", `Bearer ${this.sessionToken}`);
    }
    try {
      return await sessionFetch(url, { ...init, headers });
    } catch (err) {
      throw new WebauthnNetworkError(endpoint, err);
    }
  }
}

async function parseJsonOrThrow<T>(response: Response, endpoint: string): Promise<T> {
  if (!response.ok) {
    if (response.status === 403) {
      // ADR-0089: a passkey sign-in of a suspended account answers with the
      // restricted session; read whole, never truncated or logged.
      const full = await response.text().catch(() => "");
      const landing = isSuspendedBody(full) ? suspendedLandingOf(full) : null;
      if (landing !== null) throw new AccountSuspendedError(endpoint, landing);
      throw new WebauthnHttpError(403, endpoint, full.length > 512 ? `${full.slice(0, 509)}...` : full);
    }
    const body = await safeReadBody(response);
    throw new WebauthnHttpError(response.status, endpoint, body);
  }
  return (await response.json()) as T;
}

async function safeReadBody(response: Response): Promise<string> {
  try {
    const text = await response.text();
    return text.length > 512 ? `${text.slice(0, 509)}...` : text;
  } catch {
    return "";
  }
}

// ---- DOM-error mapping ------------------------------------------

function mapDomError(err: unknown): Error {
  if (err instanceof Error) {
    if (err.name === "NotAllowedError" || err.name === "AbortError") {
      return new WebauthnUserCancelledError();
    }
    if (
      err.name === "NotSupportedError" ||
      err.name === "InvalidStateError"
    ) {
      return new WebauthnUnsupportedError();
    }
  }
  return err instanceof Error ? err : new Error(String(err));
}

// ---- base64url ↔ ArrayBuffer / encoders -------------------------

// ---- action_hash derivation (G-13, ADR-0014) --------------------

/** `action_hash = SHA-256(domain_utf8 ‖ identifier)`, URL-safe base64.
 *  SHA-256 is native everywhere (WebCrypto / CryptoKit / MessageDigest),
 *  so the same convention computes identically on every client. The
 *  server treats the result as opaque 32 bytes. */
async function deriveActionHash(domain: string, identifier: Uint8Array): Promise<string> {
  const domainBytes = new TextEncoder().encode(domain);
  const input = new Uint8Array(domainBytes.length + identifier.length);
  input.set(domainBytes, 0);
  input.set(identifier, domainBytes.length);
  const digest = await crypto.subtle.digest("SHA-256", input);
  return bufferToBase64url(digest);
}

/** Action hash for deleting the passkey credential identified by
 *  `credentialIdB64` (the value [`listCredentials`] returns). */
export async function actionHashForDeleteCredential(
  credentialIdB64: string,
): Promise<string> {
  const idBytes = new Uint8Array(base64urlToBuffer(credentialIdB64));
  return deriveActionHash(ACTION_DOMAIN_DELETE_CREDENTIAL, idBytes);
}

/** Action hash for removing the DeviceGroup leaf whose MLS signature
 *  public key is `signaturePublicKey`. */
export async function actionHashForRemoveDevice(
  signaturePublicKey: Uint8Array,
): Promise<string> {
  return deriveActionHash(ACTION_DOMAIN_REMOVE_DEVICE, signaturePublicKey);
}

/** A3 — action hash for deleting the account identified by its raw 16-byte
 *  `accountId`. Matches `services/auth::delete_account_action_hash`. */
export async function actionHashForDeleteAccount(
  accountId: Uint8Array,
): Promise<string> {
  return deriveActionHash(ACTION_DOMAIN_DELETE_ACCOUNT, accountId);
}

function base64urlToBuffer(s: string): ArrayBuffer {
  // The server emits no-pad; restore if needed.
  const padded = s + "===".slice((s.length + 3) % 4);
  const std = padded.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(std);
  const buffer = new ArrayBuffer(binary.length);
  const view = new Uint8Array(buffer);
  for (let i = 0; i < binary.length; i += 1) {
    view[i] = binary.charCodeAt(i);
  }
  return buffer;
}

function bufferToBase64url(buf: ArrayBuffer): string {
  const view = new Uint8Array(buf);
  let binary = "";
  for (let i = 0; i < view.length; i += 1) {
    // `noUncheckedIndexedAccess` makes `view[i]` typed as
    // `number | undefined`. The for-bounds check guarantees
    // a value, but TS can't see that — assert here.
    binary += String.fromCharCode(view[i] as number);
  }
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function decodeCreationOptions(
  src: ServerPublicKeyCredentialCreationOptions,
): PublicKeyCredentialCreationOptions {
  // `exactOptionalPropertyTypes` requires us to omit fields
  // rather than passing `undefined`. Build the object
  // incrementally so the DOM type's strict `?:` shape is
  // satisfied.
  const out: PublicKeyCredentialCreationOptions = {
    rp: src.rp,
    user: {
      id: base64urlToBuffer(src.user.id),
      name: src.user.name,
      displayName: src.user.displayName,
    },
    challenge: base64urlToBuffer(src.challenge),
    pubKeyCredParams: src.pubKeyCredParams.map((p) => ({
      type: p.type as PublicKeyCredentialType,
      alg: p.alg,
    })),
  };
  if (src.timeout !== undefined) {
    out.timeout = src.timeout;
  }
  if (src.excludeCredentials !== undefined) {
    out.excludeCredentials = src.excludeCredentials.map((c) =>
      buildCredentialDescriptor(c),
    );
  }
  if (src.authenticatorSelection !== undefined) {
    out.authenticatorSelection = src.authenticatorSelection;
  }
  if (src.attestation !== undefined) {
    out.attestation = src.attestation;
  }
  if (src.extensions !== undefined) {
    out.extensions = src.extensions as AuthenticationExtensionsClientInputs;
  }
  return out;
}

function decodeRequestOptions(
  src: ServerPublicKeyCredentialRequestOptions,
): PublicKeyCredentialRequestOptions {
  const out: PublicKeyCredentialRequestOptions = {
    challenge: base64urlToBuffer(src.challenge),
  };
  if (src.timeout !== undefined) {
    out.timeout = src.timeout;
  }
  if (src.rpId !== undefined) {
    out.rpId = src.rpId;
  }
  if (src.allowCredentials !== undefined) {
    out.allowCredentials = src.allowCredentials.map((c) =>
      buildCredentialDescriptor(c),
    );
  }
  if (src.userVerification !== undefined) {
    out.userVerification = src.userVerification;
  }
  if (src.extensions !== undefined) {
    out.extensions = src.extensions as AuthenticationExtensionsClientInputs;
  }
  return out;
}

function buildCredentialDescriptor(c: {
  readonly type: string;
  readonly id: string;
  readonly transports?: readonly string[];
}): PublicKeyCredentialDescriptor {
  const desc: PublicKeyCredentialDescriptor = {
    type: c.type as PublicKeyCredentialType,
    id: base64urlToBuffer(c.id),
  };
  if (c.transports !== undefined) {
    desc.transports = c.transports as AuthenticatorTransport[];
  }
  return desc;
}

function encodeRegisterCredential(cred: PublicKeyCredential): RegisterPublicKeyCredentialJSON {
  const r = cred.response as AuthenticatorAttestationResponse;
  // `getTransports()` lands in the spec; not every authenticator
  // returns one, so we omit the field rather than passing an empty
  // array webauthn-rs would still validate.
  const transports =
    typeof r.getTransports === "function" ? r.getTransports() : undefined;
  const responseJson: RegisterPublicKeyCredentialJSON["response"] =
    transports !== undefined && transports.length > 0
      ? {
          clientDataJSON: bufferToBase64url(r.clientDataJSON),
          attestationObject: bufferToBase64url(r.attestationObject),
          transports,
        }
      : {
          clientDataJSON: bufferToBase64url(r.clientDataJSON),
          attestationObject: bufferToBase64url(r.attestationObject),
        };
  return {
    id: cred.id,
    rawId: bufferToBase64url(cred.rawId),
    type: "public-key",
    response: responseJson,
    extensions: serverSafeExtensionResults(cred),
  };
}

function encodeAuthCredential(cred: PublicKeyCredential): AuthPublicKeyCredentialJSON {
  const r = cred.response as AuthenticatorAssertionResponse;
  return {
    id: cred.id,
    rawId: bufferToBase64url(cred.rawId),
    type: "public-key",
    response: {
      clientDataJSON: bufferToBase64url(r.clientDataJSON),
      authenticatorData: bufferToBase64url(r.authenticatorData),
      signature: bufferToBase64url(r.signature),
      userHandle: r.userHandle === null ? null : bufferToBase64url(r.userHandle),
    },
    extensions: serverSafeExtensionResults(cred),
  };
}

/** What a ceremony yields beyond the server's answer (ADR-0072 decision 6): the raw
 *  credential id, and the PRF `first` output when the authenticator
 *  evaluated it (`null` otherwise: unsupported, or not requested). */
export interface PasskeyCeremonyExtras {
  readonly credentialId: Uint8Array;
  readonly prfOutput: Uint8Array | null;
  /** What a REGISTRATION reported about PRF: `true` when the new credential
   *  supports it (the output may still come only at the next use), `false`
   *  when it does not, `null` when the browser said nothing (an assertion,
   *  or no PRF support at all). */
  readonly prfEnabled: boolean | null;
}

function ceremonyExtras(cred: PublicKeyCredential): PasskeyCeremonyExtras {
  const results = cred.getClientExtensionResults() as {
    prf?: { enabled?: boolean; results?: { first?: ArrayBuffer | ArrayBufferView } };
  };
  const first = results.prf?.results?.first;
  let prfOutput: Uint8Array | null = null;
  if (first !== undefined) {
    const bytes =
      first instanceof ArrayBuffer
        ? new Uint8Array(first)
        : new Uint8Array(first.buffer, first.byteOffset, first.byteLength);
    prfOutput = bytes.length === 32 ? new Uint8Array(bytes) : null;
  }
  const enabled = results.prf?.enabled;
  return {
    credentialId: new Uint8Array(cred.rawId),
    prfOutput,
    prfEnabled: typeof enabled === "boolean" ? enabled : null,
  };
}

/** The extension inputs that ask the authenticator to evaluate PRF. */
function prfExtensionInput(salt: Uint8Array): AuthenticationExtensionsClientInputs {
  return { prf: { eval: { first: salt } } } as unknown as AuthenticationExtensionsClientInputs;
}

/** The client extension results the server receives. The PRF output is a
 *  secret the server must never see, so it is dropped before the blob is
 *  serialised (JSON would turn the ArrayBuffer into `{}` anyway; this makes
 *  the omission deliberate rather than accidental). */
function serverSafeExtensionResults(cred: PublicKeyCredential): Record<string, unknown> {
  const all = { ...(cred.getClientExtensionResults() as Record<string, unknown>) };
  delete all["prf"];
  return all;
}
