// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `AuthClient` — fetch-based client for `lakko-svc-auth`.
 *
 * Two layers of API:
 *
 *   1. **Low-level methods** mirror the seven HTTP endpoints 1:1.
 *      Take a typed request, return a typed response, do nothing
 *      else. Useful for tests and for the recovery flow that
 *      1.16c3c will assemble on top.
 *
 *   2. **High-level `signup` / `login`** orchestrate the full
 *      multi-round-trip flow:
 *      `/pow/challenge` → `/signup/start` → `/signup/finish` (and
 *      the equivalent for login). They drive the OPAQUE WASM classes
 *      (`OpaqueRegister` / `OpaqueLogin`) and, for signup only, the
 *      proof-of-work solver (ADR-0054 S4: account creation is the one
 *      request that still pays one).
 *
 * Limits (ADR-0054 S4) answer `429 {"error": code, "retry_after_secs": N}`
 * with `Retry-After`; `AuthHttpError.errorCode` / `.retryAfter` expose them
 * and `auth-error-copy.ts` turns them into screen copy. A 429 is never a
 * sign-out.
 *
 * Errors propagate as plain `Error` instances. Status-code-derived
 * messages preserve the server's body when small, since the
 * server's auth handlers already redact OPAQUE-sensitive details
 * (see `services/auth/src/opaque.rs::login_finish_inner`).
 */
import { loadWasm } from "../wasm";
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { identityProofB64, identityProofOfSeedB64 } from "./identity-proof";
import {
  base64Decode,
  base64Encode,
  base64UrlDecodeNoPad,
  base64UrlEncodeNoPad,
  generateAccountId,
} from "./account-id";
import { solvePow } from "./pow";
import { normalizeTotpCode } from "./totp-code";
import { sessionFetch } from "./session-fetch";
import { sendRecallFinish, sendWithStaleRetry, staleServerTimeMs } from "./statement-stale";
import { sessionPairOf, type SessionPair } from "./session-pair";
import { freshGrantOf, type FreshGrant } from "./fresh-auth";
import { isSuspendedBody, suspendedLandingOf, type SuspendedLanding } from "./suspension";
import type {
  DeviceAuthorizeRedeemRequest,
  DeviceAuthorizeRedeemResponse,
  DeviceAuthorizeStartResponse,
  GenesisAttestRequest,
  GenesisAttestResponse,
  KeyResetFinishRequest,
  KeyRotateFinishRequest,
  SessionRefreshUpgradeRequest,
  KeyResetFinishResponse,
  KeyResetStartRequest,
  RecallFinishRequest,
  RecallFinishResponse,
  RecallStartRequest,
  RecallStartResponse,
  RecallStatusRequest,
  KeyResetStartResponse,
  LoginFinishRequest,
  LoginFinishResponse,
  LoginHandleDeleteResponse,
  LoginHandlePutRequest,
  LoginHandlePutResponse,
  LoginStartRequest,
  LoginStartResponse,
  PasswordResetFinishRequest,
  PasswordResetFinishResponse,
  PasswordResetStartRequest,
  PasswordResetStartResponse,
  PowChallengeResponse,
  SessionRefreshRequest,
  SessionRefreshResponse,
  SessionBindDeviceRequest,
  SessionBindDeviceResponse,
  DeviceRevokeRequest,
  SignupFinishRequest,
  SignupFinishResponse,
  SignupStartRequest,
  SignupStartResponse,
  TotpEnrollFinishRequest,
  TotpEnrollStartResponse,
  TotpRecoverRequest,
  TotpRecoverResponse,
  PasskeyVaultDeleteResponse,
  PasskeyVaultGetRequest,
  PasskeyVaultGetResponse,
  PasskeyVaultListResponse,
  PasskeyVaultPutRequest,
  DeviceRequestCreateRequest,
  DeviceRequestCreateResponse,
  DeviceRequestStatusRequest,
  DeviceRequestStatusResponse,
  DeviceRequestsListResponse,
  DeviceRequestAnswerRequest,
  DeviceRequestOutcome,
  DeviceRequestPayloadRequest,
  DeviceRequestResultRequest,
} from "./wire";

/** Default base URL for the dev environment. Vite's proxy maps
 *  `/api/auth/*` → `http://127.0.0.1:8081/*`. Production deployments
 *  route the same path at the reverse-proxy (nginx) layer, so the
 *  app code uses one URL across environments. */
export const DEFAULT_BASE_URL = "/api/auth";

/** The fixed length of the ADR-0073 key-reset attestation. */
export const KEY_RESET_ATTESTATION_LEN = 145;

/** Result of `AuthClient.signup`. `exportKey` is the OPAQUE export
 *  key the device root key gets derived from in 1.16c3c2 — the
 *  caller MUST handle it carefully (no console.log, no plain
 *  storage). */
export interface SignupResult {
  /** Raw 16 bytes — the canonical id the server returned and that
   *  the client must use on every subsequent login. */
  accountId: Uint8Array;
  /** OPAQUE export key bytes (64 for `LakkoCipherSuite`). */
  exportKey: Uint8Array;
  /** The 12-word recovery phrase, present when signup was called
   *  with `enrolRecovery: true` (the default). The UI MUST
   *  display this exactly once and ask the user to record it
   *  somewhere safe — losing it means losing the only way back
   *  into the account if the password is forgotten. */
  recoveryPhrase?: string;
  /** ADR-0073: the session pair (access token, refresh credential, key
   *  generation) the landing installs. */
  session: SessionPair;
  /** The fresh-auth grant the signup finish minted (the password was typed
   *  just now), or `null`. Memory only. */
  freshGrant: FreshGrant | null;
}

/** Options for `AuthClient.signup`. */
export interface SignupOptions {
  /** Generate a BIP39 mnemonic and enrol it as a parallel OPAQUE
   *  recovery envelope alongside the password. Defaults to `true`
   *  — every new account SHOULD have a recovery passphrase. */
  enrolRecovery?: boolean;
  /** Cancels the signup's proof of work and everything after it. */
  signal?: AbortSignal;
}

/** Result of `AuthClient.login`. */
export interface LoginResult {
  accountId: Uint8Array;
  /** Symmetric session key shared with the server. */
  sessionKey: Uint8Array;
  /** Same export key the user got at signup time — deterministic
   *  for the same password. */
  exportKey: Uint8Array;
  /** ADR-0073: the session pair the landing installs. */
  session: SessionPair;
  /** ADR-0073 (amended 2026-10-06): the fresh-auth grant the password
   *  login minted, or `null` when the response carried none. Memory only. */
  freshGrant: FreshGrant | null;
}

/** Result of `AuthClient.deviceAuthorizeRedeem` (ADR-0018 factor (a)).
 *  The same `(account_id, session_token, expiry)` triple every other
 *  login flow returns. */
export interface DeviceAuthorizeRedeemResult {
  /** Raw 16-byte account id. On the device-grant redeem this is where a
   *  fresh device first learns it (ADR-0056). */
  accountId: Uint8Array;
  /** ADR-0073: the session pair the landing installs. */
  session: SessionPair;
}

/** Result of `AuthClient.totpEnrollStart` (ADR-0018 factor (c)).
 *  Provisioning material for the authenticator app — display the
 *  QR + the manual-entry secret exactly once; neither is
 *  persisted client-side. */
export interface TotpEnrollStartResult {
  /** `otpauth://totp/...` provisioning URI — render as a QR. */
  otpauthUri: string;
  /** RFC 4648 base32 (no padding) secret for manual entry. */
  secretB32: string;
}

/** Result of `AuthClient.totpRecover` (ADR-0018 factor (c)). The
 *  same `(account_id, session_token, expiry)` triple every other
 *  login flow returns — ACCESS only, no identity material. */
export interface TotpRecoverResult {
  /** Raw 16-byte account id. On the device-grant redeem this is where a
   *  fresh device first learns it (ADR-0056). */
  accountId: Uint8Array;
  /** ADR-0073: the session pair the landing installs. */
  session: SessionPair;
}

/** Result of `AuthClient.recover`. */
export interface RecoverResult {
  /** The account whose password was just rotated. */
  accountId: Uint8Array;
  /** OPAQUE export key for the **new** password. Use this to
   *  derive a fresh device root key — the previous device key
   *  (if any) is no longer reachable from the new password. */
  exportKey: Uint8Array;
  /** ADR-0073: a fresh session pair for the rotated password (same key
   *  generation: a password reset does not bump it). */
  session: SessionPair;
}

/** Result of `AuthClient.resetAccountKeys` (ADR-0073 section 2). */
export interface KeyResetResult {
  /** The raw 145-byte key-reset attestation (decoded from the server's
   *  URL-safe base64). Carried to discovery and to the DS. */
  attestation: Uint8Array;
  /** The same attestation as the server encoded it (URL-safe, no pad), kept
   *  for the "publish owed" record. */
  attestationB64Url: string;
  /** The session pair at the NEW generation. The caller's old pair is
   *  superseded by the same transaction: install this one at once. */
  session: SessionPair;
}

/** Result of `AuthClient.recallTakeBack`: the key reset's, plus the
 *  fresh-auth grant the recall answers with. */
export interface RecallResult extends KeyResetResult {
  freshGrant: FreshGrant | null;
}

/** The OLD password typed on the take-the-account-back form is wrong: the
 *  OPAQUE login did not finish on this device, and no finish was sent. */
export class RecallWrongPasswordError extends Error {
  override name = "RecallWrongPasswordError";
  constructor() {
    super("the password from before the reset is wrong");
  }
}

/** A precondition was violated by the caller — e.g. an empty
 *  password. Distinct from a network error so the UI can surface
 *  it as a form validation message. */
export class AuthClientArgError extends Error {
  override name = "AuthClientArgError";
}

/** A request to the auth service did not return 2xx. The original
 *  status code and (truncated) body are exposed for diagnostics. */
/** Account statement kinds (`lakko-attest::account_statement`). */
export const ACCOUNT_STATEMENT_KIND_RECALL_STATUS = 1;
export const ACCOUNT_STATEMENT_KIND_RECALL = 2;
export const ACCOUNT_STATEMENT_KIND_SESSION_UPGRADE = 4;
export const ACCOUNT_STATEMENT_KIND_RECALL_START = 5;

/** A `recall/finish` request built and signed, kept until it is answered
 *  (ADR-0074 T5 item 25). */
export interface RecallPendingFinish {
  /** The request as last sent (or to be sent). Resent byte for byte. */
  request: RecallFinishRequest;
  /** The same request with the statement signed at the server's time. */
  readonly resign: (signAtMs: number) => RecallFinishRequest;
  /** Zero the old seed copy the signer holds. */
  readonly dispose: () => void;
}

/** The server's time out of a `statement-stale` auth answer. */
export function authStaleTimeOf(err: unknown): number | null {
  return err instanceof AuthHttpError ? staleServerTimeMs(err.status, err.body) : null;
}

export class AuthHttpError extends Error {
  override name = "AuthHttpError";
  constructor(
    /** HTTP status code from the server. */
    public readonly status: number,
    /** Endpoint path — useful in error toasts. */
    public readonly endpoint: string,
    /** Truncated server body (max 512 chars to keep stack traces
     *  tractable). */
    public readonly body: string,
    /** Seconds from a `Retry-After` header, when the server sent one
     *  (ADR-0054 S4: every auth limit answers 429 with it). */
    public readonly retryAfterSecs: number | null = null,
  ) {
    super(`auth ${endpoint} → ${status}: ${body}`);
  }

  /** The machine-readable code of a `{"error": "<code>", ...}` body (the
   *  ADR-0054 limit shape), or `null` when the body has none. */
  get errorCode(): string | null {
    return parseLimitBody(this.body).error;
  }

  /** `retry_after_secs` from the body, else the `Retry-After` header. */
  get retryAfter(): number | null {
    return parseLimitBody(this.body).retryAfterSecs ?? this.retryAfterSecs;
  }
}

/**
 * ADR-0099 section 4: the account is suspended. Auth answers `403
 * {"error":"account-suspended"}`; a route that authenticated the person
 * (a sign-in, a recovery, the refresh) adds the restricted session, which
 * `landing` carries. The credentials never reach `body` or the message: an
 * error may be logged.
 */
export class AccountSuspendedError extends AuthHttpError {
  override name = "AccountSuspendedError";
  constructor(
    endpoint: string,
    /** The restricted session of a landing route's refusal, or `null` for a
     *  bearer route's (no credentials). */
    public readonly landing: SuspendedLanding | null,
  ) {
    super(403, endpoint, '{"error":"account-suspended"}');
  }
}

/** The `SuspendedLanding` an error carries, or `null`. */
export function suspendedLandingFrom(err: unknown): SuspendedLanding | null {
  return err instanceof AccountSuspendedError ? err.landing : null;
}

/**
 * The error of a refused response. A `403 account-suspended` is read whole
 * (its credentials do not fit the 512-character truncation) and becomes an
 * {@link AccountSuspendedError}; anything else is an `AuthHttpError` with the
 * truncated body, as before.
 */
export async function authHttpErrorFrom(response: Response, endpoint: string): Promise<AuthHttpError> {
  let text: string;
  try {
    text = await response.text();
  } catch {
    text = "<unreadable body>";
  }
  if (response.status === 403 && isSuspendedBody(text)) {
    return new AccountSuspendedError(endpoint, suspendedLandingOf(text));
  }
  return new AuthHttpError(
    response.status,
    endpoint,
    text.length > 512 ? `${text.slice(0, 512)}…` : text,
    retryAfterHeader(response),
  );
}

/** The answer of `GET /signup/open`: `false` only for an explicit
 *  `{"open": false}`; anything else reads as open. */
export function signupOpenFromBody(body: unknown): boolean {
  if (typeof body !== "object" || body === null) return true;
  return (body as { open?: unknown }).open !== false;
}

/** Read `{"error": code, "retry_after_secs": N}` leniently. */
function parseLimitBody(body: string): { error: string | null; retryAfterSecs: number | null } {
  try {
    const parsed = JSON.parse(body) as { error?: unknown; retry_after_secs?: unknown };
    return {
      error: typeof parsed.error === "string" ? parsed.error : null,
      retryAfterSecs:
        typeof parsed.retry_after_secs === "number" && Number.isFinite(parsed.retry_after_secs)
          ? Math.max(0, Math.ceil(parsed.retry_after_secs))
          : null,
    };
  } catch {
    return { error: null, retryAfterSecs: null };
  }
}

/** Seconds from a `Retry-After` header, `null` when absent or unparseable. */
function retryAfterHeader(response: Response): number | null {
  const header = response.headers.get("retry-after");
  if (header === null) return null;
  const secs = Number(header.trim());
  return Number.isFinite(secs) && secs >= 0 ? Math.ceil(secs) : null;
}

/** Network failure (DNS / connectivity / TLS). Distinct from
 *  `AuthHttpError` so the UI can suggest "check connection" rather
 *  than "wrong credentials". */
export class AuthNetworkError extends Error {
  override name = "AuthNetworkError";
  constructor(
    public readonly endpoint: string,
    cause: unknown,
  ) {
    super(`auth ${endpoint} network error: ${String(cause)}`);
  }
}

/** Fetch-based client. Stateless — every method is a one-shot RPC. */
export class AuthClient {
  constructor(public readonly baseUrl: string = DEFAULT_BASE_URL) {}

  // -----------------------------------------------------------------
  // Low-level: 1:1 HTTP endpoints
  // -----------------------------------------------------------------

  /** `GET /signup/open` (ADR-0046 amendment, `docs/ux/beta-account-cap.md`):
   *  whether a new account with a username fits under the beta cap now.
   *  Anonymous. Any failure (no connection, a non-2xx such as an older
   *  server's `404`, a body that is not `{"open": boolean}`) reads as open:
   *  the first login-handle bind decides later. */
  async signupOpen(): Promise<boolean> {
    let response: Response;
    try {
      response = await fetch(`${this.baseUrl}/signup/open`, { method: "GET" });
    } catch {
      return true;
    }
    if (!response.ok) return true;
    let body: unknown;
    try {
      body = await response.json();
    } catch {
      return true;
    }
    return signupOpenFromBody(body);
  }

  /** `POST /pow/challenge` — used by signup only. */
  async powChallenge(): Promise<PowChallengeResponse> {
    return this.post<undefined, PowChallengeResponse>("/pow/challenge", undefined);
  }

  /** `POST /signup/start`. */
  async signupStart(req: SignupStartRequest): Promise<SignupStartResponse> {
    return this.post("/signup/start", req);
  }

  /** `POST /signup/finish`. */
  async signupFinish(req: SignupFinishRequest): Promise<SignupFinishResponse> {
    return this.post("/signup/finish", req);
  }

  /** `POST /login/start`. */
  async loginStart(req: LoginStartRequest): Promise<LoginStartResponse> {
    return this.post("/login/start", req);
  }

  /** `POST /login/finish`. */
  async loginFinish(req: LoginFinishRequest): Promise<LoginFinishResponse> {
    return this.post("/login/finish", req);
  }

  /** `POST /recovery/login/start`. */
  async recoveryLoginStart(req: LoginStartRequest): Promise<LoginStartResponse> {
    return this.post("/recovery/login/start", req);
  }

  /** `POST /recovery/login/finish`. */
  async recoveryLoginFinish(
    req: LoginFinishRequest,
  ): Promise<LoginFinishResponse> {
    return this.post("/recovery/login/finish", req);
  }

  /** `POST /password/reset/start`. */
  async passwordResetStart(
    req: PasswordResetStartRequest,
  ): Promise<PasswordResetStartResponse> {
    return this.post("/password/reset/start", req);
  }

  /** `POST /password/reset/finish`. */
  async passwordResetFinish(
    req: PasswordResetFinishRequest,
  ): Promise<PasswordResetFinishResponse> {
    return this.post("/password/reset/finish", req);
  }

  // -----------------------------------------------------------------
  // Device authorization — ADR-0018 factor (a), "another-device"
  // recovery
  // -----------------------------------------------------------------

  /**
   * `POST /device/authorize/start` — mint a single-use 32-byte
   * device-authorization grant for this account (ADR-0018 factor
   * (a), scanner side).
   *
   * Bearer-authenticated (the EXISTING device proves it already
   * holds a session) (no proof of work since ADR-0054 S4; limited server-side). Returns the URL-safe base64
   * (no pad) of the RAW grant token — the server stores only its
   * SHA-256. The caller seals it (together with the phrase seed)
   * to the fresh device over the E2EE pairing channel; it never
   * travels in plaintext past this response.
   */
  async deviceAuthorizeStart(sessionToken: string, freshGrant: string): Promise<string> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    // Adding a device needs the fresh-auth grant (`fresh-auth.ts`); without
    // a usable one the server answers `403 fresh-auth-required`.
    const resp = await this.post<
      { fresh_grant: string },
      DeviceAuthorizeStartResponse
    >("/device/authorize/start", { fresh_grant: freshGrant }, sessionToken);
    return resp.grant_token_b64;
  }

  /**
   * `POST /device/authorize/redeem` — atomically consume a
   * single-use device grant for a session-token triple (ADR-0018
   * factor (a), fresh-device side).
   *
   * Anonymous (the fresh device has no token yet) (no proof of work since ADR-0054 S4; limited server-side). A
   * missing / expired / already-consumed grant fails as an
   * enumeration-safe 401 (`AuthHttpError`), indistinguishable from
   * a wrong credential by design.
   *
   * ADR-0056: the body is the grant alone. The server finds the account
   * from the grant's hash, and this response is where a fresh device first
   * learns its account id, after proving the grant.
   *
   * @param grantTokenB64  URL-safe base64 (no pad) of the raw
   *                       32-byte grant token received over the
   *                       rendezvous channel.
   */
  async deviceAuthorizeRedeem(
    grantTokenB64: string,
    deviceIdB64?: string,
  ): Promise<DeviceAuthorizeRedeemResult> {
    if (grantTokenB64.length === 0) {
      throw new AuthClientArgError("grantTokenB64 must be non-empty");
    }
    const resp = await this.post<
      DeviceAuthorizeRedeemRequest,
      DeviceAuthorizeRedeemResponse
    >("/device/authorize/redeem", {
      grant_token_b64: grantTokenB64,
      ...deviceField(deviceIdB64),
    });
    const accountId = base64UrlDecodeNoPad(resp.account_id_b64);
    if (accountId.length !== 16) {
      throw new AuthClientArgError("redeem response carried a malformed account id");
    }
    return {
      accountId,
      session: sessionPairOf(resp),
    };
  }

  // -----------------------------------------------------------------
  // Genesis attestation — ADR-0036, G-30 finding 2 (anti-squatting)
  // -----------------------------------------------------------------

  /**
   * `POST /auth/genesis/attest` — mint an auth-attested genesis
   * attestation for this account's client-generated identity pubkey
   * (ADR-0036, closes the G-30 finding-2 squatting residual).
   *
   * Bearer-authenticated (no proof of work): holding a valid session token *is*
   * proof the caller OPAQUE-authenticated as `account_id`, which is the
   * entire security basis — only the account holder can mint a genesis
   * attestation for their own key, so a squatter cannot fabricate one
   * for a victim's account. The server derives `username_hash` from the
   * AUTHENTICATED account id (never from the body) and signs a
   * `(account_id, username_hash, pubkey, expiry)` binding with the
   * shared attestation key.
   *
   * Returns the raw 153-byte `GenesisAttestation` bytes (decoded from the
   * server's URL-safe-no-pad base64) so the caller threads them straight
   * into the discovery identity-bundle upload alongside the ADR-0019
   * genesis self-signature.
   *
   * The body carries the identity proof (F3, `identity-proof.ts`) by
   * `identity` over `accountId`.
   *
   * @param sessionToken the live bearer token proving the OPAQUE auth.
   * @param identity     the account identity whose 32-byte Ed25519 key is
   *                     recorded.
   * @param accountId    the signed-in account's 16-byte id.
   */
  async genesisAttest(
    sessionToken: string,
    identity: WasmIdentity,
    accountId: Uint8Array,
  ): Promise<Uint8Array> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const pubkey = identity.signaturePublicKey;
    if (pubkey.length !== 32) {
      throw new AuthClientArgError("pubkey must be 32 bytes (Ed25519)");
    }
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    const wasm = await loadWasm();
    const resp = await this.post<GenesisAttestRequest, GenesisAttestResponse>(
      "/auth/genesis/attest",
      { pubkey_b64: base64UrlEncodeNoPad(pubkey), identity_proof_b64: identityProofB64(wasm, identity, accountId) },
      sessionToken,
    );
    return base64UrlDecodeNoPad(resp.genesis_attest_b64);
  }

  // -----------------------------------------------------------------
  // TOTP — ADR-0018 factor (c), authenticator-code recovery
  // -----------------------------------------------------------------

  /**
   * `POST /totp/enroll/start` — mint + store an (unconfirmed) TOTP
   * secret for this account (ADR-0018 factor (c), enrollment step 1).
   *
   * Bearer-authenticated (the already-logged-in account (no proof of work since ADR-0054 S4; limited server-side).
   * Returns the `otpauth://` provisioning URI (QR) + the base32
   * secret (manual entry). The enrollment only becomes usable for
   * recovery after `totpEnrollFinish` confirms it with a live code.
   *
   * A 409 `AuthHttpError` means a CONFIRMED authenticator already
   * exists (the server refuses a silent swap); a 503 means the
   * server has TOTP disabled (`LAKKO_TOTP_ENC_KEY` unset).
   */
  async totpEnrollStart(sessionToken: string): Promise<TotpEnrollStartResult> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const resp = await this.post<undefined, TotpEnrollStartResponse>(
      "/totp/enroll/start",
      undefined,
      sessionToken,
    );
    return { otpauthUri: resp.otpauth_uri, secretB32: resp.secret_b32 };
  }

  /**
   * `POST /totp/enroll/finish` — confirm the enrollment with the
   * first code the authenticator produced (ADR-0018 factor (c),
   * enrollment step 2).
   *
   * Bearer-authenticated, no proof of work.
   * A 401 `AuthHttpError` means the code did not verify (wrong app
   * entry / clock skew beyond ±30 s); the pending enrollment stays
   * pending, so the user can simply retry with the next code.
   */
  async totpEnrollFinish(sessionToken: string, code: string): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const normalized = normalizeTotpCode(code);
    if (normalized === null) {
      throw new AuthClientArgError("code must be 6 digits");
    }
    await this.post<TotpEnrollFinishRequest, unknown>(
      "/totp/enroll/finish",
      { code: normalized },
      sessionToken,
    );
  }

  /**
   * `POST /totp/recover` — recover ACCESS with an authenticator
   * code (ADR-0018 factor (c), recovery side).
   *
   * Anonymous (the caller lost their device (no proof of work since ADR-0054 S4; limited server-side). The server is
   * enumeration-safe by design: unknown account, unconfirmed
   * enrollment, wrong code, and replayed code all surface the SAME
   * generic 401 `AuthHttpError`; 429 signals the
   * 5-failures-in-15-minutes lockout; 503 means TOTP is disabled
   * server-side.
   *
   * TOTP grants ACCESS only — the result carries no key material.
   * Identity acquisition is the caller's job (ADR-0017 state
   * machine).
   */
  async totpRecover(
    handle: string,
    code: string,
    deviceIdB64?: string,
  ): Promise<TotpRecoverResult> {
    requireHandle(handle);
    const normalized = normalizeTotpCode(code);
    if (normalized === null) {
      throw new AuthClientArgError("code must be 6 digits");
    }
    const resp = await this.post<TotpRecoverRequest, TotpRecoverResponse>(
      "/totp/recover",
      { handle, code: normalized, ...deviceField(deviceIdB64) },
    );
    return {
      accountId: base64UrlDecodeNoPad(resp.account_id_b64),
      session: sessionPairOf(resp),
    };
  }

  // -----------------------------------------------------------------
  // High-level: full signup / login / recover flows
  // -----------------------------------------------------------------

  /**
   * Drive the full signup flow:
   *
   * ```text
   *   POST /pow/challenge   →  seed          (auth's, the only one left)
   *   client: solve the proof of work (off the main thread, cancellable)
   *   client: OPAQUE password start [+ recovery start]
   *   POST /signup/start    →  password_response [+ recovery_response]
   *   client: OPAQUE finish (both flows)
   *   POST /signup/finish   →  ok            (no proof: bound to the start)
   * ```
   *
   * ADR-0054 S4: account creation is the one request that still pays a
   * proof of work, once, heavily. `/signup/finish` carries none; it must
   * follow a start for the same account within the server's window, and
   * answers `410` when it does not (a start that expired while the device was
   * busy). That restarts the flow from the start, once.
   *
   * When `options.enrolRecovery !== false` (default), this method
   * also generates a fresh BIP39 12-word mnemonic, enrols it as
   * a parallel OPAQUE envelope under `KIND_RECOVERY` server-side,
   * and returns the phrase in `result.recoveryPhrase` for the UI
   * to display **once**. Both envelopes commit atomically inside
   * `/signup/finish` (server transaction).
   *
   * `options.signal` cancels the proof of work (the worker is terminated)
   * and stops before any further request; the abort surfaces as a
   * `DOMException` named `AbortError`. The server holds nothing until the
   * proof is spent, so a cancelled signup leaves no half-created account.
   *
   * Throws `AuthClientArgError` on programmer error (empty
   * password). `AuthHttpError` on a non-2xx response and
   * `AuthNetworkError` on a transport failure.
   */
  async signup(
    password: Uint8Array,
    options: SignupOptions = {},
  ): Promise<SignupResult> {
    if (password.length === 0) {
      throw new AuthClientArgError("password must be non-empty");
    }
    const enrolRecovery = options.enrolRecovery !== false;
    const signal = options.signal;

    const wasm = await loadWasm();
    const accountId = generateAccountId();
    const accountIdB64 = base64Encode(accountId);

    // The recovery phrase is chosen once: a restarted start enrols the
    // same one, so the phrase the user is shown is the one on the server.
    let recoveryPhrase: { readonly phrase: string; readonly seed: Uint8Array } | null = null;
    if (enrolRecovery) {
      const mnemonic = wasm.RecoveryMnemonic.generate();
      recoveryPhrase = { phrase: mnemonic.phrase, seed: mnemonic.toSeed() };
    }

    for (let attempt = 0; ; attempt += 1) {
      throwIfAborted(signal);
      // ------------- start
      const challenge = await this.powChallenge();
      throwIfAborted(signal);
      const pow = await solvePow(challenge.seed, challenge.effort_bits, signal);
      throwIfAborted(signal);

      const passwordReg = wasm.OpaqueRegister.startPassword(password);
      const recoveryReg =
        recoveryPhrase !== null ? wasm.OpaqueRegister.startRecovery(recoveryPhrase.seed) : null;

      const startResp = await this.signupStart({
        pow,
        account_id: accountIdB64,
        password_request: passwordReg.requestB64,
        ...(recoveryReg !== null ? { recovery_request: recoveryReg.requestB64 } : {}),
      });

      if (recoveryReg !== null && startResp.recovery_response === undefined) {
        // The server should always echo back a recovery_response
        // when we set recovery_request. Treat the missing field as
        // a server-side wiring bug rather than crashing later in
        // OPAQUE finish.
        throw new AuthHttpError(
          200,
          "/signup/start",
          "missing recovery_response despite recovery_request being set",
        );
      }

      // ------------- finish (OPAQUE math)
      const passwordOut = passwordReg.finish(password, startResp.password_response);

      // The recovery export key has a different domain (KIND_RECOVERY)
      // and is only useful when the user re-authenticates via the
      // mnemonic. The PASSWORD export key is what the device root
      // key gets derived from for day-to-day use, so we throw the
      // recovery-side export key away here.
      const recoveryUploadB64: string | undefined =
        recoveryReg !== null && recoveryPhrase !== null && startResp.recovery_response !== undefined
          ? recoveryReg.finish(recoveryPhrase.seed, startResp.recovery_response).uploadB64
          : undefined;

      let finishResp: SignupFinishResponse;
      try {
        finishResp = await this.signupFinish({
          account_id: accountIdB64,
          password_upload: passwordOut.uploadB64,
          ...(recoveryUploadB64 !== undefined ? { recovery_upload: recoveryUploadB64 } : {}),
        });
      } catch (err) {
        // The start expired (or was never recorded): restart from the start.
        if (attempt === 0 && err instanceof AuthHttpError && err.status === 410) continue;
        throw err;
      }
      if (!finishResp.ok) {
        throw new AuthHttpError(
          200,
          "/signup/finish",
          "server reported ok=false",
        );
      }

      return {
        accountId,
        exportKey: passwordOut.exportKey,
        ...(recoveryPhrase !== null ? { recoveryPhrase: recoveryPhrase.phrase } : {}),
        session: sessionPairOf(finishResp),
        freshGrant: freshGrantOf(finishResp),
      };
    }
  }

  /**
   * Drive the full password-login flow:
   *
   * ```text
   *   client: OPAQUE start
   *   POST /login/start    →  session_id, response
   *   client: OPAQUE finish
   *   POST /login/finish   →  ok, account_id
   * ```
   *
   * ADR-0054 S5: the start names the account by its login handle
   * (`login-handle.ts`); the account id is learned from the finish, after
   * the password proved itself.
   *
   * Wrong passwords surface as an `AuthHttpError` with status
   * 401 — by design, indistinguishable from "unknown account".
   */
  async login(
    handle: string,
    password: Uint8Array,
    /** ADR-0074 section 0.4: this device's id, when it is already known
     *  for the account being signed in to (`session-device.ts`). */
    deviceIdB64?: string,
  ): Promise<LoginResult> {
    if (password.length === 0) {
      throw new AuthClientArgError("password must be non-empty");
    }
    requireHandle(handle);

    const wasm = await loadWasm();

    const login = wasm.OpaqueLogin.startPassword(password);
    const startResp = await this.loginStart({
      handle,
      request: login.requestB64,
    });

    const out = login.finish(password, startResp.response);

    const finishResp = await this.loginFinish({
      session_id: startResp.session_id,
      finalize: out.finalizeB64,
      ...deviceField(deviceIdB64),
    });
    if (!finishResp.ok) {
      throw new AuthHttpError(200, "/login/finish", "server reported ok=false");
    }

    return {
      accountId: accountIdOfFinish(finishResp.account_id, "/login/finish"),
      sessionKey: out.sessionKey,
      exportKey: out.exportKey,
      session: sessionPairOf(finishResp),
      freshGrant: freshGrantOf(finishResp),
    };
  }

  /**
   * Sign in with the recovery credential, replacing nothing:
   *
   * ```text
   *   client: OPAQUE recovery start (seed)
   *   POST /recovery/login/start   ->  session_id, response
   *   client: OPAQUE recovery finish
   *   POST /recovery/login/finish  ->  ok, account_id, session
   * ```
   *
   * `seed` is the 64-byte BIP39 seed of the recovery phrase. The password
   * envelope is untouched, which is the difference from {@link recover}.
   * Used by the local three-word check (maintainer decision 2026-10-02): a
   * device that holds the phrase and saw three of its words typed
   * correctly signs in with the seed it already has.
   *
   * A seed that is not the account's recovery credential, and a name
   * nobody holds, answer the same `401`.
   */
  async recoveryLogin(
    handle: string,
    seed: Uint8Array,
    deviceIdB64?: string,
  ): Promise<TotpRecoverResult> {
    requireHandle(handle);
    if (seed.length === 0) {
      throw new AuthClientArgError("seed must be non-empty");
    }
    const wasm = await loadWasm();
    const login = wasm.OpaqueLogin.startRecovery(seed);
    const startResp = await this.recoveryLoginStart({
      handle,
      request: login.requestB64,
    });
    let out;
    try {
      out = login.finish(seed, startResp.response);
    } catch {
      // The seed is not this account's recovery credential: the exchange
      // cannot be finished on this side. Reported as the `401` the server
      // would give, so callers see one refusal whichever side noticed.
      throw new AuthHttpError(401, "/recovery/login/finish", "recovery credential did not match");
    }
    const finishResp = await this.recoveryLoginFinish({
      session_id: startResp.session_id,
      finalize: out.finalizeB64,
      ...deviceField(deviceIdB64),
    });
    if (!finishResp.ok) {
      throw new AuthHttpError(200, "/recovery/login/finish", "server reported ok=false");
    }
    return {
      accountId: accountIdOfFinish(finishResp.account_id, "/recovery/login/finish"),
      session: sessionPairOf(finishResp),
    };
  }

  /**
   * `POST /auth/session/refresh` — trade the refresh credential for a new
   * pair (ADR-0073 section 3.2). Anonymous: the credential is the
   * authorisation, sent in the body, never as a bearer. Answers the pair, or
   * `AuthHttpError` 401 with `errorCode` `key-generation-superseded` (the
   * keys were reset elsewhere) or `refresh-invalid` (bad, expired or
   * unknown). The session token provider is the only caller.
   */
  async sessionRefresh(refreshToken: string): Promise<SessionPair> {
    if (refreshToken.length === 0) {
      throw new AuthClientArgError("refreshToken must be non-empty");
    }
    const resp = await this.post<SessionRefreshRequest, SessionRefreshResponse>(
      "/auth/session/refresh",
      { refresh_token: refreshToken },
    );
    return sessionPairOf(resp);
  }

  /**
   * `POST /auth/session/bind-device` — replace a session that names no
   * device with one naming `deviceIdB64` (ADR-0074 section 0.4, T1 points 8
   * and 9). Bearer; the body carries the refresh credential of the same
   * session, since the route issues a new 30-day one. Idempotent for a
   * session that already names this device (a fresh pair).
   *
   * Throws `AuthHttpError`: `400` (malformed or all-zero id),
   * `401 refresh-invalid`, `401 key-generation-superseded`,
   * `401 device-revoked`, `409 device-already-bound` (the session names
   * another device). The caller drops the pair it sent.
   */
  async sessionBindDevice(
    sessionToken: string,
    refreshToken: string,
    deviceIdB64: string,
  ): Promise<SessionPair> {
    if (sessionToken.length === 0 || refreshToken.length === 0) {
      throw new AuthClientArgError("session credentials must be non-empty");
    }
    const resp = await this.post<SessionBindDeviceRequest, SessionBindDeviceResponse>(
      "/auth/session/bind-device",
      { device_id_b64: deviceIdB64, refresh_token: refreshToken },
      sessionToken,
    );
    return sessionPairOf(resp);
  }

  /**
   * `POST /auth/session/logout` — sign this device's session out at the
   * server (ADR-0074 section 2.1): afterwards its bearer gets a plain `401`
   * and its refresh credential `401 refresh-invalid`. Bearer, no body,
   * `204`. Sent with the token exactly as given (no refresh, no retry): a
   * logout that fails is retried from the owed marker, not here.
   */
  async sessionLogout(sessionToken: string): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const endpoint = "/auth/session/logout";
    let response: Response;
    try {
      response = await fetch(`${this.baseUrl}${endpoint}`, {
        method: "POST",
        headers: { authorization: `Bearer ${sessionToken}` },
        // The page may be navigating away right after a logout.
        keepalive: true,
      });
    } catch (err) {
      throw new AuthNetworkError(endpoint, err);
    }
    if (!response.ok) {
      throw new AuthHttpError(response.status, endpoint, await safeReadBody(response));
    }
  }

  /**
   * `POST /account/devices/revoke` — sign another device of the account out
   * and mark it removed (ADR-0074 section 2.1): from then on auth answers
   * its bearer and its refresh with `401 device-revoked`, and it wipes
   * itself. Bearer. `204`.
   *
   * `statement` is the 138-byte `device-revoke` account statement for
   * `deviceId` (`signDeviceRevokeStatement`), valid for 120 s: sign it right
   * before this call.
   *
   * Throws `AuthHttpError`: `403 fresh-auth-required` (no usable grant;
   * checked first), `400` (bad id, the all-zero id, the caller's own
   * device), `403 account-key-required` (auth has no recorded account key,
   * or the statement did not verify), `429 too-many-attempts`.
   */
  async revokeDevice(
    sessionToken: string,
    freshGrant: string,
    deviceId: Uint8Array,
    statement: Uint8Array,
  ): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    if (deviceId.length !== 32) {
      throw new AuthClientArgError("deviceId must be 32 bytes");
    }
    const endpoint = "/account/devices/revoke";
    const body: DeviceRevokeRequest = {
      fresh_grant: freshGrant,
      device_id_b64: base64UrlEncodeNoPad(deviceId),
      statement_b64: base64UrlEncodeNoPad(statement),
    };
    let response: Response;
    try {
      response = await sessionFetch(`${this.baseUrl}${endpoint}`, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          authorization: `Bearer ${sessionToken}`,
        },
        body: JSON.stringify(body),
      });
    } catch (err) {
      throw new AuthNetworkError(endpoint, err);
    }
    if (!response.ok) {
      throw new AuthHttpError(
        response.status,
        endpoint,
        await safeReadBody(response),
        retryAfterHeader(response),
      );
    }
  }

  /**
   * Reset the signed-in account's keys (ADR-0073 section 2): `seed` is the
   * 64-byte BIP39 seed of the NEW recovery phrase, `newSignaturePubkey` the
   * 32-byte Ed25519 key of the identity derived from it.
   *
   * ```text
   *   client: OPAQUE recovery registration start (seed)
   *   POST /account/keys/reset/start   { fresh_grant, recovery_request }  -> { recovery_response }
   *   client: OPAQUE recovery registration finish
   *   POST /account/keys/reset/finish  { fresh_grant, recovery_upload, new_signature_pubkey_b64 }
   *                                    -> { key_reset_attest_b64, <session pair> }
   * ```
   *
   * The start stores nothing; the finish is one transaction (the generation
   * bump, the credential replace, the passkey / vault / grant deletions, the
   * attestation, the new pair), so a failure anywhere changes nothing and
   * the caller discards the generated phrase. On success the caller's old
   * pair is already superseded: install the returned one before any other
   * request.
   *
   * `freshGrant` is the fresh-auth grant (`fresh-auth.ts`), sent on
   * both calls; one grant serves both, and a successful finish spends it.
   *
   * Throws `AuthHttpError` (`401` no session or superseded,
   * `403 fresh-auth-required` no usable grant, `400` malformed,
   * `429 too-many-attempts` with `retryAfter`, `503 rate-limiter-unavailable`
   * or `key-reset-disabled`) or `AuthNetworkError`.
   */
  async resetAccountKeys(
    sessionToken: string,
    freshGrant: string,
    seed: Uint8Array,
    newSignaturePubkey: Uint8Array,
    /** The account's 16-byte id, which the identity proof (F3) names. */
    accountId: Uint8Array,
  ): Promise<KeyResetResult> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    if (freshGrant.length === 0) {
      throw new AuthClientArgError("freshGrant must be non-empty");
    }
    if (seed.length === 0) {
      throw new AuthClientArgError("seed must be non-empty");
    }
    if (newSignaturePubkey.length !== 32) {
      throw new AuthClientArgError("newSignaturePubkey must be 32 bytes (Ed25519)");
    }
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    const wasm = await loadWasm();
    // The proof by the key being recorded (F3); refused before anything is
    // sent when the seed's key is not `newSignaturePubkey`.
    const identityProof = identityProofOfSeedB64(wasm, seed, accountId, newSignaturePubkey);
    const reg = wasm.OpaqueRegister.startRecovery(seed);
    const start = await this.post<KeyResetStartRequest, KeyResetStartResponse>(
      "/account/keys/reset/start",
      { fresh_grant: freshGrant, recovery_request: reg.requestB64 },
      sessionToken,
    );
    const upload = reg.finish(seed, start.recovery_response).uploadB64;
    const finish = await this.post<KeyResetFinishRequest, KeyResetFinishResponse>(
      "/account/keys/reset/finish",
      {
        fresh_grant: freshGrant,
        recovery_upload: upload,
        new_signature_pubkey_b64: base64UrlEncodeNoPad(newSignaturePubkey),
        identity_proof_b64: identityProof,
      },
      sessionToken,
    );
    const attestation = base64UrlDecodeNoPad(finish.key_reset_attest_b64);
    if (attestation.length !== KEY_RESET_ATTESTATION_LEN) {
      throw new AuthHttpError(
        200,
        "/account/keys/reset/finish",
        `key_reset_attest_b64 is ${attestation.length} bytes, expected ${KEY_RESET_ATTESTATION_LEN}`,
      );
    }
    return {
      attestation,
      attestationB64Url: finish.key_reset_attest_b64,
      session: sessionPairOf(finish),
    };
  }

  /**
   * Rotate the signed-in account's keys, keeping the other devices (ADR-0074
   * section 3.4, "T5 as built" items 6-13):
   *
   * ```text
   *   POST /account/keys/rotate/start   { fresh_grant, recovery_request }  -> { recovery_response }
   *   POST /account/keys/rotate/finish  { fresh_grant, recovery_upload, new_signature_pubkey_b64,
   *                                       supersession_proof_b64, revoke_device_ids_b64[] }
   *                                     -> { key_reset_attest_b64, <session pair> }
   * ```
   *
   * `proof` is the 145-byte supersession proof signed by the OLD account key
   * for `newSignaturePubkey`; auth verifies it against the key it recorded,
   * so a session that holds the password but not the account key cannot
   * rotate. Passkeys, the password and TOTP are kept; the passkey vaults are
   * deleted. The answer carries no fresh-auth grant.
   *
   * Throws `AuthHttpError` (`403 fresh-auth-required`, `400`, `403
   * account-key-required`, `429 too-many-attempts`) or `AuthNetworkError`.
   */
  async rotateAccountKeys(args: {
    readonly sessionToken: string;
    readonly freshGrant: string;
    /** The 64-byte BIP39 seed of the NEW phrase. */
    readonly newSeed: Uint8Array;
    readonly newSignaturePubkey: Uint8Array;
    /** The account's 16-byte id, which the identity proof (F3) names. */
    readonly accountId: Uint8Array;
    readonly proof: Uint8Array;
    /** Devices whose removal raced the commit (at most 8). */
    readonly revokeDeviceIds: readonly Uint8Array[];
    /** ADR-0075 section 3: the person is saving a recovery phrase; the
     *  account becomes phrase-protected with this rotation. */
    readonly phraseSaved?: boolean;
  }): Promise<KeyResetResult> {
    if (args.sessionToken.length === 0) throw new AuthClientArgError("sessionToken must be non-empty");
    if (args.freshGrant.length === 0) throw new AuthClientArgError("freshGrant must be non-empty");
    if (args.newSeed.length === 0) throw new AuthClientArgError("newSeed must be non-empty");
    if (args.newSignaturePubkey.length !== 32) {
      throw new AuthClientArgError("newSignaturePubkey must be 32 bytes (Ed25519)");
    }
    if (args.revokeDeviceIds.length > 8) {
      throw new AuthClientArgError("at most 8 device ids ride a rotation");
    }
    if (args.accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    const wasm = await loadWasm();
    const identityProof = identityProofOfSeedB64(wasm, args.newSeed, args.accountId, args.newSignaturePubkey);
    const reg = wasm.OpaqueRegister.startRecovery(args.newSeed);
    const start = await this.post<KeyResetStartRequest, KeyResetStartResponse>(
      "/account/keys/rotate/start",
      { fresh_grant: args.freshGrant, recovery_request: reg.requestB64 },
      args.sessionToken,
    );
    const upload = reg.finish(args.newSeed, start.recovery_response).uploadB64;
    const finish = await this.post<KeyRotateFinishRequest, KeyResetFinishResponse>(
      "/account/keys/rotate/finish",
      {
        fresh_grant: args.freshGrant,
        recovery_upload: upload,
        new_signature_pubkey_b64: base64UrlEncodeNoPad(args.newSignaturePubkey),
        identity_proof_b64: identityProof,
        supersession_proof_b64: base64UrlEncodeNoPad(args.proof),
        revoke_device_ids_b64: args.revokeDeviceIds.map((id) => base64UrlEncodeNoPad(id)),
        ...(args.phraseSaved === true ? { phrase_saved: true } : {}),
      },
      args.sessionToken,
    );
    const attestation = base64UrlDecodeNoPad(finish.key_reset_attest_b64);
    if (attestation.length !== KEY_RESET_ATTESTATION_LEN) {
      throw new AuthHttpError(
        200,
        "/account/keys/rotate/finish",
        `key_reset_attest_b64 is ${attestation.length} bytes, expected ${KEY_RESET_ATTESTATION_LEN}`,
      );
    }
    return {
      attestation,
      attestationB64Url: finish.key_reset_attest_b64,
      session: sessionPairOf(finish),
    };
  }

  /**
   * `POST /auth/session/refresh` with a `key_statement_b64` (ADR-0074 section
   * 3.3, "T5 as built" items 15-17): a refresh credential of a LOWER
   * generation is exchanged for a pair at the current one when the kind-4
   * account statement over it verifies under the account's recorded key.
   * Only a device that holds the new account key can produce it.
   *
   * `sign(signAtMs)` returns the statement signed now (`null`) or at the
   * server's time; a `statement-stale` answer is re-signed once.
   *
   * Throws `AuthHttpError` 401 (`key-generation-superseded`: this key is not
   * the account's; `device-revoked`; `refresh-invalid`) or `AuthNetworkError`.
   */
  async sessionRefreshUpgrade(
    refreshToken: string,
    sign: (signAtMs: number | null) => Uint8Array,
  ): Promise<SessionPair> {
    if (refreshToken.length === 0) throw new AuthClientArgError("refreshToken must be non-empty");
    const resp = await sendWithStaleRetry(
      (signAtMs) =>
        this.post<SessionRefreshUpgradeRequest, SessionRefreshResponse>("/auth/session/refresh", {
          refresh_token: refreshToken,
          key_statement_b64: base64UrlEncodeNoPad(sign(signAtMs)),
        }),
      authStaleTimeOf,
    );
    return sessionPairOf(resp);
  }

  /**
   * `POST /account/recall/status` (ADR-0074 part 1): whether the account
   * can be taken back from this device, and until when. Anonymous;
   * `statement` is the `recall-status` account statement signed by the OLD
   * account key. Returns the parsed JSON body as it arrived
   * (`recall.ts` reads it). Throws `AuthHttpError` (`401 recall-refused`:
   * not possible any more; `429`) or `AuthNetworkError`.
   */
  async recallStatus(
    sign: (signAtMs: number | null) => Uint8Array,
    /** ADR-0075 section 8: name the account by login handle (base64url, no
     *  pad). The statement `sign` returns must then carry the first 16
     *  bytes of the handle in its account field. */
    handle?: string,
  ): Promise<unknown> {
    // T5 item 24: a device with a wrong clock is told the server's time and
    // signs again; it must not read "no anchor" and destroy its record.
    return sendWithStaleRetry(
      (signAtMs) =>
        this.post<RecallStatusRequest, unknown>("/account/recall/status", {
          statement_b64: base64UrlEncodeNoPad(sign(signAtMs)),
          ...(handle === undefined ? {} : { handle }),
        }),
      authStaleTimeOf,
    );
  }

  /**
   * Take the account back from a device of before the reset (ADR-0074
   * part 1, "T3 as built"):
   *
   * ```text
   *   client: OPAQUE login start (old password), registration starts (new
   *           password, new recovery credential), the recall-start statement
   *   POST /account/recall/start    ->  the three responses, a server nonce
   *   client: OPAQUE login finish. A failure here is a wrong OLD password:
   *           `RecallWrongPasswordError`, and finish is NOT called.
   *   client: both registration finishes, the recall statement
   *   POST /account/recall/finish   ->  the key reset's answer + a grant
   * ```
   *
   * Every accepted start is one password attempt at the server. The result
   * is the key reset's: the caller continues exactly as after
   * `resetAccountKeys`.
   *
   * Throws {@link RecallWrongPasswordError}, `AuthHttpError` (`401
   * recall-refused`, `401 wrong-password` at finish, `429
   * too-many-attempts`, `400`) or `AuthNetworkError`.
   */
  async recallTakeBack(args: {
    readonly accountId: Uint8Array;
    /** The OLD account identity's 32-byte Ed25519 seed (the recall record). */
    readonly oldAccountSeed: Uint8Array;
    readonly anchorSeq: number;
    readonly oldPassword: Uint8Array;
    readonly newPassword: Uint8Array;
    /** The 64-byte BIP39 seed of the NEW phrase. */
    readonly newSeed: Uint8Array;
    readonly newSignaturePubkey: Uint8Array;
  }): Promise<RecallResult> {
    if (args.oldPassword.length === 0 || args.newPassword.length === 0) {
      throw new AuthClientArgError("passwords must be non-empty");
    }
    if (args.newSeed.length === 0) {
      throw new AuthClientArgError("newSeed must be non-empty");
    }
    if (args.newSignaturePubkey.length !== 32) {
      throw new AuthClientArgError("newSignaturePubkey must be 32 bytes (Ed25519)");
    }
    const prepared = await this.recallPrepare(args);
    return this.recallFinish(prepared);
  }

  /**
   * The first half of {@link recallTakeBack}: `recall/start`, the local
   * OPAQUE finishes, and the finish request built and signed but NOT sent.
   * The caller keeps the returned value (and the phrase it generated) until
   * {@link recallFinish} has answered: a finish whose answer was lost is
   * asked again with the same request (ADR-0074 T5 item 25).
   *
   * A `statement-stale` answer to start is re-signed at the server's time
   * and sent once more (item 24); nothing was counted for it.
   */
  async recallPrepare(args: {
    readonly accountId: Uint8Array;
    readonly oldAccountSeed: Uint8Array;
    readonly anchorSeq: number;
    readonly oldPassword: Uint8Array;
    /** ADR-0075 section 8, the phrase arm: the 64-byte seed of the recovery
     *  phrase as it was before the reset. When given, the login is made from
     *  it against the anchored recovery envelope and `oldPassword` is not
     *  used. */
    readonly oldPhraseSeed?: Uint8Array;
    readonly newPassword: Uint8Array;
    readonly newSeed: Uint8Array;
    readonly newSignaturePubkey: Uint8Array;
  }): Promise<RecallPendingFinish> {
    const byPhrase = args.oldPhraseSeed !== undefined;
    if ((!byPhrase && args.oldPassword.length === 0) || args.newPassword.length === 0) {
      throw new AuthClientArgError("passwords must be non-empty");
    }
    if (byPhrase && args.oldPhraseSeed.length === 0) {
      throw new AuthClientArgError("oldPhraseSeed must be non-empty");
    }
    if (args.newSeed.length === 0) {
      throw new AuthClientArgError("newSeed must be non-empty");
    }
    if (args.newSignaturePubkey.length !== 32) {
      throw new AuthClientArgError("newSignaturePubkey must be 32 bytes (Ed25519)");
    }
    const wasm = await loadWasm();
    // The proof by the NEW key (F3), made before anything is sent; it rides
    // the finish, so a resent finish carries the same bytes.
    const identityProof = identityProofOfSeedB64(wasm, args.newSeed, args.accountId, args.newSignaturePubkey);
    const accountIdB64Url = base64UrlEncodeNoPad(args.accountId);
    // The secret the login is made from: the old password, or (the phrase
    // arm) the old phrase's seed.
    const loginSecret = args.oldPhraseSeed ?? args.oldPassword;
    const login = byPhrase
      ? wasm.OpaqueLogin.startRecovery(loginSecret)
      : wasm.OpaqueLogin.startPassword(loginSecret);
    const passwordReg = wasm.OpaqueRegister.startPassword(args.newPassword);
    const recoveryReg = wasm.OpaqueRegister.startRecovery(args.newSeed);
    const loginRequest = login.requestB64;
    const start = await sendWithStaleRetry(
      (signAtMs) =>
        this.post<RecallStartRequest, RecallStartResponse>("/account/recall/start", {
          account_id_b64: accountIdB64Url,
          anchor_seq: args.anchorSeq,
          statement_b64: base64UrlEncodeNoPad(
            signAtMs === null
              ? wasm.signRecallStartStatement(
                  args.oldAccountSeed,
                  args.accountId,
                  BigInt(args.anchorSeq),
                  loginRequest,
                )
              : wasm.signAccountStatementAt(
                  args.oldAccountSeed,
                  ACCOUNT_STATEMENT_KIND_RECALL_START,
                  args.accountId,
                  wasm.recallStartStatementBody(BigInt(args.anchorSeq), loginRequest),
                  BigInt(signAtMs),
                ),
          ),
          login_request: loginRequest,
          ...(byPhrase ? { proof: "phrase" as const } : {}),
          password_request: passwordReg.requestB64,
          recovery_request: recoveryReg.requestB64,
        }),
      authStaleTimeOf,
    );
    let loginFinish: string;
    try {
      loginFinish = login.finish(loginSecret, start.login_response).finalizeB64;
    } catch {
      // The AKE did not agree on a key: the OLD password is wrong. Nothing
      // more is sent; the attempt was counted at start.
      throw new RecallWrongPasswordError();
    }
    const passwordUpload = passwordReg.finish(args.newPassword, start.password_response).uploadB64;
    const recoveryUpload = recoveryReg.finish(args.newSeed, start.recovery_response).uploadB64;
    const serverNonce = base64UrlDecodeNoPad(start.server_nonce_b64);
    const base = {
      session_id: start.session_id,
      account_id_b64: accountIdB64Url,
      anchor_seq: args.anchorSeq,
      login_finish: loginFinish,
      password_upload: passwordUpload,
      recovery_upload: recoveryUpload,
      new_signature_pubkey_b64: base64UrlEncodeNoPad(args.newSignaturePubkey),
      identity_proof_b64: identityProof,
    };
    const oldAccountSeed = args.oldAccountSeed.slice();
    const accountId = args.accountId.slice();
    const newSignaturePubkey = args.newSignaturePubkey.slice();
    const sign = (signAtMs: number | null): string =>
      base64UrlEncodeNoPad(
        signAtMs === null
          ? wasm.signRecallStatement(
              oldAccountSeed,
              accountId,
              serverNonce,
              newSignaturePubkey,
              passwordUpload,
              recoveryUpload,
            )
          : wasm.signAccountStatementAt(
              oldAccountSeed,
              ACCOUNT_STATEMENT_KIND_RECALL,
              accountId,
              wasm.recallStatementBody(serverNonce, newSignaturePubkey, passwordUpload, recoveryUpload),
              BigInt(signAtMs),
            ),
      );
    return {
      request: { ...base, statement_b64: sign(null) },
      resign: (signAtMs) => ({ ...base, statement_b64: sign(signAtMs) }),
      dispose: () => oldAccountSeed.fill(0),
    };
  }

  /**
   * The second half: `recall/finish` with the prepared request. No answer
   * (or a `5xx`) resends the BYTE-IDENTICAL request, bounded
   * (`statement-stale.ts`); the server answers a repeated identical finish
   * `200` again with the same attestation for the attempt's ten minutes. A
   * `statement-stale` answer re-signs at the server's time once; the
   * re-signed request then replaces `pending.request`, so a later call
   * resends what was last sent. May be called again with the same `pending`
   * after it threw for a lost answer.
   */
  async recallFinish(pending: RecallPendingFinish): Promise<RecallResult> {
    const finish = await sendWithStaleRetry((signAtMs) => {
      if (signAtMs !== null) pending.request = pending.resign(signAtMs);
      const request = pending.request;
      return sendRecallFinish(
        () => this.post<RecallFinishRequest, RecallFinishResponse>("/account/recall/finish", request),
        (err) =>
          err instanceof AuthNetworkError ? null : err instanceof AuthHttpError ? err.status : undefined,
        (ms) => new Promise<void>((resolve) => setTimeout(resolve, ms)),
      );
    }, authStaleTimeOf);
    const attestation = base64UrlDecodeNoPad(finish.key_reset_attest_b64);
    if (attestation.length !== KEY_RESET_ATTESTATION_LEN) {
      throw new AuthHttpError(
        200,
        "/account/recall/finish",
        `key_reset_attest_b64 is ${attestation.length} bytes, expected ${KEY_RESET_ATTESTATION_LEN}`,
      );
    }
    return {
      attestation,
      attestationB64Url: finish.key_reset_attest_b64,
      session: sessionPairOf(finish),
      freshGrant: freshGrantOf(finish),
    };
  }

  /**
   * Drive the full recover-from-mnemonic flow:
   *
   * ```text
   *   client: OPAQUE recovery start (seed)
   *   client: OPAQUE password start (newPassword)
   *   POST /password/reset/start   →  session_id, recovery_response,
   *                                    new_password_response
   *   client: OPAQUE recovery finish + OPAQUE password finish
   *   POST /password/reset/finish  →  ok, account_id
   * ```
   *
   * Drives `lakko-svc-auth`'s `/password/reset/{start,finish}`
   * endpoint pair (1.16c3c3a). The server proves recovery
   * ownership via the mnemonic and atomically replaces the
   * `KIND_PASSWORD` envelope with the new one — the
   * `KIND_RECOVERY` envelope stays intact, so the same 12 words
   * work for future resets.
   *
   * The recovery flow pays no proof of work (ADR-0054 S4: the start is
   * limited per named account and per address instead). Wrong-mnemonic
   * attempts fail at the AKE
   * finalization with the same indistinguishable response as
   * `/recovery/login/finish`.
   *
   * @param handle       The login handle (ADR-0054 S5), base64url of the
   *                     32 bytes `loginHandle(username)` returns. The
   *                     account id comes back in the finish.
   * @param mnemonicSeed The 64-byte BIP39 seed derived from the
   *                     user's typed phrase via
   *                     `RecoveryMnemonic.fromPhrase(phrase).toSeed()`.
   *                     Caller is responsible for surfacing
   *                     mnemonic-parsing errors before reaching
   *                     this method.
   * @param newPassword  The replacement password as raw UTF-8
   *                     bytes.
   */
  async recover(
    handle: string,
    mnemonicSeed: Uint8Array,
    newPassword: Uint8Array,
    deviceIdB64?: string,
  ): Promise<RecoverResult> {
    requireHandle(handle);
    if (mnemonicSeed.length === 0) {
      throw new AuthClientArgError("mnemonicSeed must be non-empty");
    }
    if (newPassword.length === 0) {
      throw new AuthClientArgError("newPassword must be non-empty");
    }

    const wasm = await loadWasm();

    // Two parallel OPAQUE flows: recovery login proves the user
    // knows the mnemonic; password registration enrols the new
    // password envelope. Both run independently — neither
    // depends on the other's intermediate state.
    const recoveryLogin = wasm.OpaqueLogin.startRecovery(mnemonicSeed);
    const newPasswordReg = wasm.OpaqueRegister.startPassword(newPassword);

    const startResp = await this.passwordResetStart({
      handle,
      recovery_request: recoveryLogin.requestB64,
      new_password_request: newPasswordReg.requestB64,
    });

    // Both finish steps. A wrong mnemonic seed here surfaces as
    // an OPAQUE-side error at `recoveryLogin.finish` (the AKE
    // can't agree on a session key); we surface it to the caller
    // unchanged so the form can render "invalid recovery
    // phrase" without distinguishing further.
    const recoveryOut = recoveryLogin.finish(
      mnemonicSeed,
      startResp.recovery_response,
    );
    const passwordOut = newPasswordReg.finish(
      newPassword,
      startResp.new_password_response,
    );

    const finishResp = await this.passwordResetFinish({
      session_id: startResp.session_id,
      recovery_finalize: recoveryOut.finalizeB64,
      new_password_upload: passwordOut.uploadB64,
      ...deviceField(deviceIdB64),
    });
    if (!finishResp.ok) {
      throw new AuthHttpError(
        200,
        "/password/reset/finish",
        "server reported ok=false",
      );
    }

    return {
      accountId: accountIdOfFinish(finishResp.account_id, "/password/reset/finish"),
      exportKey: passwordOut.exportKey,
      session: sessionPairOf(finishResp),
    };
  }

  // -----------------------------------------------------------------
  // Internal HTTP helper
  // -----------------------------------------------------------------

  // -----------------------------------------------------------------
  // The passkey vault — ADR-0072 decision 6
  // -----------------------------------------------------------------

  /** `PUT /vault/passkey` — store the phrase entropy sealed under one of
   *  this account's passkeys, so that passkey can restore the keys on a new
   *  device (ADR-0072 decision 6, amended 2026-11-11: every passkey that can).
   *  Bearer-authenticated. */
  async passkeyVaultPut(
    sessionToken: string,
    credentialId: Uint8Array,
    sealedEntropy: Uint8Array,
  ): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    await this.request<PasskeyVaultPutRequest, unknown>(
      "PUT",
      "/vault/passkey",
      {
        credential_id_b64: base64UrlEncodeNoPad(credentialId),
        sealed_entropy_b64: base64Encode(sealedEntropy),
      },
      sessionToken,
    );
  }

  /** `POST /vault/passkey/get` — the blob for one credential, or `null`.
   *  Bearer-authenticated. */
  async passkeyVaultGet(sessionToken: string, credentialId: Uint8Array): Promise<Uint8Array | null> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const resp = await this.post<PasskeyVaultGetRequest, PasskeyVaultGetResponse>(
      "/vault/passkey/get",
      { credential_id_b64: base64UrlEncodeNoPad(credentialId) },
      sessionToken,
    );
    return resp.sealed_entropy_b64 === null ? null : base64Decode(resp.sealed_entropy_b64);
  }

  /** `DELETE /vault/passkey` — "Stop restoring keys": remove the blob stored
   *  for one credential. `credentialIdB64` is the base64url (no pad) id as
   *  `/webauthn/credentials` lists it. Returns whether a blob existed.
   *  Bearer-authenticated. */
  async passkeyVaultDelete(sessionToken: string, credentialIdB64: string): Promise<boolean> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const resp = await this.request<PasskeyVaultGetRequest, PasskeyVaultDeleteResponse>(
      "DELETE",
      "/vault/passkey",
      { credential_id_b64: credentialIdB64 },
      sessionToken,
    );
    return resp.deleted;
  }

  /** `GET /vault/passkeys` — the credential ids (base64url, no pad) that
   *  have keys stored for them. Bearer-authenticated. */
  async passkeyVaultList(sessionToken: string): Promise<string[]> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const resp = await this.get<PasskeyVaultListResponse>("/vault/passkeys", sessionToken);
    return resp.credential_ids_b64;
  }

  // -----------------------------------------------------------------
  // Device requests — ADR-0049 section 2 as amended 2026-09-28: the
  // device link. The old device chooses a six-digit code and the new
  // device types it; the two run SPAKE2 over this relay, which sees no
  // code and cannot open anything it carries.
  // -----------------------------------------------------------------

  /** `POST /device/request` — the fresh device asks to be confirmed,
   *  addressed by the login handle of the typed username (ADR-0056), never
   *  by an account id. Anonymous, limited server-side (ADR-0054 S4). A 200
   *  says nothing about whether the name exists. Nothing else rides on it:
   *  the pairing payload travels sealed, later, inside the exchange. */
  async deviceRequestCreate(
    loginHandle: Uint8Array,
  ): Promise<{ requestId: string; expiresInSecs: number }> {
    if (loginHandle.length !== 32) {
      throw new AuthClientArgError("loginHandle must be 32 bytes");
    }
    const resp = await this.post<DeviceRequestCreateRequest, DeviceRequestCreateResponse>(
      "/device/request",
      { handle: base64UrlEncodeNoPad(loginHandle) },
    );
    return { requestId: resp.request_id_b64, expiresInSecs: resp.expires_in_secs };
  }

  /** `POST /device/request/status` — where the fresh device's request
   *  stands. The request id rides in the body, never in a path. */
  async deviceRequestStatus(requestId: string): Promise<DeviceRequestStatus> {
    const resp = await this.post<DeviceRequestStatusRequest, DeviceRequestStatusResponse>(
      "/device/request/status",
      { request_id_b64: requestId },
    );
    return {
      pending: resp.pending,
      pakeB: optionalBytes(resp.pake_b_b64),
      outcome: resp.outcome ?? null,
      confirmB: optionalBytes(resp.confirm_b_b64),
    };
  }

  /** `POST /device/request/payload` — the fresh device's half of the
   *  exchange, once. `409 device-request-not-ready` before the old device
   *  answered, `409 device-request-already-used` after a first post. */
  async deviceRequestPayload(
    requestId: string,
    sealed: { pakeA: Uint8Array; sealedPayload: Uint8Array; confirmA: Uint8Array },
  ): Promise<void> {
    await this.post<DeviceRequestPayloadRequest, unknown>("/device/request/payload", {
      request_id_b64: requestId,
      pake_a_b64: base64Encode(sealed.pakeA),
      sealed_payload_b64: base64Encode(sealed.sealedPayload),
      confirm_a_b64: base64Encode(sealed.confirmA),
    });
  }

  /** `GET /device/requests` — the open request for the signed-in account
   *  with every value posted so far, or `null`. Bearer-authenticated. */
  async deviceRequestsList(sessionToken: string): Promise<PendingDeviceRequest | null> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const resp = await this.get<DeviceRequestsListResponse>("/device/requests", sessionToken);
    if (resp.request === null) return null;
    const r = resp.request;
    return {
      requestId: r.request_id_b64,
      createdAtMs: r.created_at_ms,
      pakeB: optionalBytes(r.pake_b_b64),
      pakeA: optionalBytes(r.pake_a_b64),
      sealedPayload: optionalBytes(r.sealed_payload_b64),
      confirmA: optionalBytes(r.confirm_a_b64),
      outcome: r.outcome ?? null,
    };
  }

  /** `POST /device/requests/{id}/pake` — the old device answers, once.
   *  `409 device-request-already-answered` when another device of the
   *  account answered first. Bearer-authenticated. */
  async deviceRequestAnswer(
    sessionToken: string,
    requestId: string,
    pakeB: Uint8Array,
    freshGrant: string,
  ): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    await this.post<DeviceRequestAnswerRequest, unknown>(
      `/device/requests/${encodeURIComponent(requestId)}/pake`,
      { fresh_grant: freshGrant, pake_b_b64: base64Encode(pakeB) },
      sessionToken,
    );
  }

  /** `POST /device/requests/{id}/result` — the old device records the
   *  outcome, once: `accepted` with its key confirmation, or `wrong_code`.
   *  Bearer-authenticated. */
  async deviceRequestResult(
    sessionToken: string,
    requestId: string,
    result: { outcome: "accepted"; confirmB: Uint8Array } | { outcome: "wrong_code" },
    freshGrant: string,
  ): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const body: DeviceRequestResultRequest =
      result.outcome === "accepted"
        ? { fresh_grant: freshGrant, outcome: "accepted", confirm_b_b64: base64Encode(result.confirmB) }
        : { fresh_grant: freshGrant, outcome: "wrong_code" };
    await this.post<DeviceRequestResultRequest, unknown>(
      `/device/requests/${encodeURIComponent(requestId)}/result`,
      body,
      sessionToken,
    );
  }

  /** `DELETE /device/requests/{id}` — decline ("It is not me").
   *  Bearer-authenticated. */
  async deviceRequestDelete(sessionToken: string, requestId: string): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    await this.request<undefined, unknown>(
      "DELETE",
      `/device/requests/${encodeURIComponent(requestId)}`,
      undefined,
      sessionToken,
    );
  }

  // -----------------------------------------------------------------
  // Login handle — ADR-0054 S5, sign in by name
  // -----------------------------------------------------------------

  /** `PUT /auth/login-handle` — bind this account to `handle` (base64url,
   *  32 bytes), replacing its previous one. Idempotent for the same handle.
   *  `409 handle-taken` (another account holds it) surfaces as an
   *  `AuthHttpError` with status 409; `503 login-by-name-disabled` as 503. */
  async bindLoginHandle(
    sessionToken: string,
    handle: string,
    /** ADR-0076, a bind that CHANGES the handle (a rename, or its
     *  rollback): the fresh-auth grant, the handle held now, and the
     *  recovery email to re-key (omitted: the server drops it). */
    change?: {
      readonly freshGrant: string;
      readonly previousHandle: string;
      readonly recoveryEmail?: {
        readonly address: string;
        /** Standard base64; both absent when a phrase is saved. */
        readonly vaultSalt?: string;
        readonly sealedEntropy?: string;
      };
    },
    /** ADR-0097: a held supporter place (`x-lakko-anon-token` header
     *  value) for a first bind past the beta cap. */
    supporterPlace?: string,
  ): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    requireHandle(handle);
    const body: LoginHandlePutRequest = { handle };
    if (change !== undefined) {
      requireHandle(change.previousHandle);
      body.fresh_grant = change.freshGrant;
      body.previous_handle = change.previousHandle;
      if (change.recoveryEmail !== undefined) {
        body.recovery_email = {
          address: change.recoveryEmail.address,
          ...(change.recoveryEmail.vaultSalt !== undefined && change.recoveryEmail.sealedEntropy !== undefined
            ? { vault_salt: change.recoveryEmail.vaultSalt, sealed_entropy: change.recoveryEmail.sealedEntropy }
            : {}),
        };
      }
    }
    await this.request<LoginHandlePutRequest, LoginHandlePutResponse>(
      "PUT",
      "/auth/login-handle",
      body,
      sessionToken,
      supporterPlace !== undefined ? { "x-lakko-anon-token": supporterPlace } : undefined,
    );
  }

  /** `POST /auth/login-handle/check` (ADR-0076): whether `handle` is the
   *  one this account holds (`same`), the account holds another (`other`),
   *  or it holds none (`none`). */
  async checkLoginHandle(sessionToken: string, handle: string): Promise<"same" | "other" | "none"> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    requireHandle(handle);
    const endpoint = "/auth/login-handle/check";
    const resp = await this.post<{ handle: string }, { state?: unknown }>(endpoint, { handle }, sessionToken);
    if (resp.state === "same" || resp.state === "other" || resp.state === "none") return resp.state;
    throw new AuthHttpError(200, endpoint, "unknown handle state");
  }

  /** `DELETE /auth/login-handle` — remove this account's handle. Returns
   *  whether one was bound. */
  async releaseLoginHandle(sessionToken: string): Promise<boolean> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const resp = await this.request<undefined, LoginHandleDeleteResponse>(
      "DELETE",
      "/auth/login-handle",
      undefined,
      sessionToken,
    );
    return resp.removed;
  }

  private async request<Req, Res>(
    method: "PUT" | "DELETE",
    endpoint: string,
    body: Req,
    bearer: string,
    extraHeaders?: Readonly<Record<string, string>>,
  ): Promise<Res> {
    const url = `${this.baseUrl}${endpoint}`;
    const headers: Record<string, string> = { ...extraHeaders, authorization: `Bearer ${bearer}` };
    if (body !== undefined) {
      headers["content-type"] = "application/json";
    }
    let response: Response;
    try {
      response = await sessionFetch(url, {
        method,
        headers,
        body: body === undefined ? null : JSON.stringify(body),
      });
    } catch (err) {
      throw new AuthNetworkError(endpoint, err);
    }
    if (!response.ok) {
      throw await authHttpErrorFrom(response, endpoint);
    }
    return (await response.json()) as Res;
  }

  private async get<Res>(endpoint: string, bearer: string): Promise<Res> {
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await sessionFetch(url, {
        method: "GET",
        headers: { authorization: `Bearer ${bearer}` },
      });
    } catch (err) {
      throw new AuthNetworkError(endpoint, err);
    }
    if (!response.ok) {
      throw await authHttpErrorFrom(response, endpoint);
    }
    return (await response.json()) as Res;
  }

  private async post<Req, Res>(
    endpoint: string,
    body: Req,
    bearer?: string,
  ): Promise<Res> {
    const url = `${this.baseUrl}${endpoint}`;
    const headers: Record<string, string> = {};
    if (body !== undefined) {
      headers["content-type"] = "application/json";
    }
    if (bearer !== undefined) {
      headers["authorization"] = `Bearer ${bearer}`;
    }
    let response: Response;
    try {
      // Through the session-aware fetch: a bearer the provider manages is
      // refreshed ahead of expiry and retried once on a 401 (ADR-0073
      // section 3.3); an anonymous request passes through untouched.
      response = await sessionFetch(url, {
        method: "POST",
        headers,
        body: body === undefined ? null : JSON.stringify(body),
      });
    } catch (err) {
      throw new AuthNetworkError(endpoint, err);
    }

    if (!response.ok) {
      throw await authHttpErrorFrom(response, endpoint);
    }

    // Some endpoints return small JSON, others return `{ ok: true }`.
    // Both are JSON; force the parse so a non-JSON 200 is loud.
    return (await response.json()) as Res;
  }
}

/** Where a fresh device's request stands (`POST /device/request/status`). */
export interface DeviceRequestStatus {
  /** `false` after a decline or expiry (indistinguishable by design). */
  readonly pending: boolean;
  /** The old device's SPAKE2 message, once it answered. */
  readonly pakeB: Uint8Array | null;
  /** The old device's decision, once taken. */
  readonly outcome: DeviceRequestOutcome | null;
  /** The old device's key confirmation, with `accepted`. */
  readonly confirmB: Uint8Array | null;
}

/** The open request as the old device sees it (`GET /device/requests`). */
export interface PendingDeviceRequest {
  readonly requestId: string;
  readonly createdAtMs: number;
  readonly pakeB: Uint8Array | null;
  readonly pakeA: Uint8Array | null;
  readonly sealedPayload: Uint8Array | null;
  readonly confirmA: Uint8Array | null;
  readonly outcome: DeviceRequestOutcome | null;
}

function optionalBytes(b64: string | null | undefined): Uint8Array | null {
  return b64 === null || b64 === undefined ? null : base64Decode(b64);
}

/** A login handle on the wire: base64url without padding of 32 bytes. */
const HANDLE_WIRE = /^[A-Za-z0-9_-]{43}$/;

/** The optional `device_id_b64` field of a landing request (ADR-0074
 *  section 0.4): present only when the caller knows this device's id. */
function deviceField(deviceIdB64: string | undefined): { device_id_b64?: string } {
  return deviceIdB64 === undefined ? {} : { device_id_b64: deviceIdB64 };
}

function requireHandle(handle: string): void {
  if (!HANDLE_WIRE.test(handle)) {
    throw new AuthClientArgError("handle must be base64url (no pad) of 32 bytes");
  }
}

/** The 16-byte account id a finish response carries (standard base64). */
function accountIdOfFinish(b64: string, endpoint: string): Uint8Array {
  let bytes: Uint8Array;
  try {
    bytes = base64Decode(b64);
  } catch {
    throw new AuthHttpError(200, endpoint, "account_id is not base64");
  }
  if (bytes.length !== 16) {
    throw new AuthHttpError(200, endpoint, "account_id is not 16 bytes");
  }
  return bytes;
}

/** Throw the standard `AbortError` when `signal` has fired. */
function throwIfAborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted === true) {
    throw new DOMException("signup cancelled", "AbortError");
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
