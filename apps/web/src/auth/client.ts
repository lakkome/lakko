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
import {
  base64Decode,
  base64Encode,
  base64UrlDecodeNoPad,
  base64UrlEncodeNoPad,
  generateAccountId,
} from "./account-id";
import { solvePow } from "./pow";
import { normalizeTotpCode } from "./totp-code";
import type {
  ContactKind,
  DeviceAuthorizeRedeemRequest,
  DeviceAuthorizeRedeemResponse,
  DeviceAuthorizeStartResponse,
  GenesisAttestRequest,
  GenesisAttestResponse,
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
  RecoveryContactEnrollFinishRequest,
  RecoveryContactEnrollStartRequest,
  RecoveryContactRecoverFinishRequest,
  RecoveryContactRecoverFinishResponse,
  RecoveryContactRecoverStartRequest,
  RecoveryContactSentResponse,
  SignupFinishRequest,
  SignupFinishResponse,
  SignupStartRequest,
  SignupStartResponse,
  TotpEnrollFinishRequest,
  TotpEnrollStartResponse,
  TotpRecoverRequest,
  TotpRecoverResponse,
  WordVaultEnrollRequest,
  WordVaultRecoverFinishRequest,
  WordVaultRecoverFinishResponse,
  WordVaultRecoverStartRequest,
  WordVaultRecoverStartResponse,
  WordVaultStatusResponse,
  PasskeyVaultGetRequest,
  PasskeyVaultGetResponse,
  PasskeyVaultPutRequest,
  DeviceRequestCreateRequest,
  DeviceRequestCreateResponse,
  DeviceRequestStatusRequest,
  DeviceRequestStatusResponse,
  DeviceRequestsListResponse,
} from "./wire";

/** Default base URL for the dev environment. Vite's proxy maps
 *  `/api/auth/*` → `http://127.0.0.1:8081/*`. Production deployments
 *  route the same path at the reverse-proxy (nginx) layer, so the
 *  app code uses one URL across environments. */
export const DEFAULT_BASE_URL = "/api/auth";

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
  /** The 24-word recovery phrase, present when signup was called
   *  with `enrolRecovery: true` (the default). The UI MUST
   *  display this exactly once and ask the user to record it
   *  somewhere safe — losing it means losing the only way back
   *  into the account if the password is forgotten. */
  recoveryPhrase?: string;
  /** Phase 1.5b: HMAC-SHA256 bearer token. Thread through every
   *  protected `lakko-svc-discovery` write via the
   *  `Authorization: Bearer <token>` header. */
  sessionToken: string;
  /** Token expiry as a unix-millisecond timestamp. */
  sessionExpiresAtMs: number;
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
  /** Phase 1.5b: HMAC-SHA256 bearer token. */
  sessionToken: string;
  /** Token expiry as a unix-millisecond timestamp. */
  sessionExpiresAtMs: number;
}

/** Result of `AuthClient.deviceAuthorizeRedeem` (ADR-0018 factor (a)).
 *  The same `(account_id, session_token, expiry)` triple every other
 *  login flow returns. */
export interface DeviceAuthorizeRedeemResult {
  /** Raw 16-byte account id, echoed back by the server. */
  accountId: Uint8Array;
  sessionToken: string;
  sessionExpiresAtMs: number;
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
  /** Raw 16-byte account id, echoed back by the server. */
  accountId: Uint8Array;
  sessionToken: string;
  sessionExpiresAtMs: number;
}

/** Result of `AuthClient.recoveryContactRecoverFinish` (ADR-0018
 *  factor (d)). The same `(account_id, session_token, expiry)` triple
 *  every other login flow returns — ACCESS only, no identity
 *  material. */
export interface RecoveryContactRecoverResult {
  /** Raw 16-byte account id, echoed back by the server. */
  accountId: Uint8Array;
  sessionToken: string;
  sessionExpiresAtMs: number;
}

/** Result of `AuthClient.recover`. */
export interface RecoverResult {
  /** The account whose password was just rotated. */
  accountId: Uint8Array;
  /** OPAQUE export key for the **new** password. Use this to
   *  derive a fresh device root key — the previous device key
   *  (if any) is no longer reachable from the new password. */
  exportKey: Uint8Array;
  /** Phase 1.5b: fresh bearer token for the rotated password. */
  sessionToken: string;
  /** Token expiry as a unix-millisecond timestamp. */
  sessionExpiresAtMs: number;
}

/** A precondition was violated by the caller — e.g. an empty
 *  password. Distinct from a network error so the UI can surface
 *  it as a form validation message. */
export class AuthClientArgError extends Error {
  override name = "AuthClientArgError";
}

/** A request to the auth service did not return 2xx. The original
 *  status code and (truncated) body are exposed for diagnostics. */
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
  async deviceAuthorizeStart(sessionToken: string): Promise<string> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const resp = await this.post<
      undefined,
      DeviceAuthorizeStartResponse
    >("/device/authorize/start", undefined, sessionToken);
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
   * @param accountId      Raw 16-byte account id from the pairing QR.
   * @param grantTokenB64  URL-safe base64 (no pad) of the raw
   *                       32-byte grant token received over the
   *                       rendezvous channel.
   */
  async deviceAuthorizeRedeem(
    accountId: Uint8Array,
    grantTokenB64: string,
  ): Promise<DeviceAuthorizeRedeemResult> {
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    if (grantTokenB64.length === 0) {
      throw new AuthClientArgError("grantTokenB64 must be non-empty");
    }
    const resp = await this.post<
      DeviceAuthorizeRedeemRequest,
      DeviceAuthorizeRedeemResponse
    >("/device/authorize/redeem", {
      account_id_b64: base64UrlEncodeNoPad(accountId),
      grant_token_b64: grantTokenB64,
    });
    return {
      accountId: base64UrlDecodeNoPad(resp.account_id_b64),
      sessionToken: resp.session_token,
      sessionExpiresAtMs: resp.session_expires_at_ms,
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
   * @param sessionToken the live bearer token proving the OPAQUE auth.
   * @param pubkey       the client's 32-byte Ed25519 genesis identity key.
   */
  async genesisAttest(
    sessionToken: string,
    pubkey: Uint8Array,
  ): Promise<Uint8Array> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    if (pubkey.length !== 32) {
      throw new AuthClientArgError("pubkey must be 32 bytes (Ed25519)");
    }
    const resp = await this.post<GenesisAttestRequest, GenesisAttestResponse>(
      "/auth/genesis/attest",
      { pubkey_b64: base64UrlEncodeNoPad(pubkey) },
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
  ): Promise<TotpRecoverResult> {
    requireHandle(handle);
    const normalized = normalizeTotpCode(code);
    if (normalized === null) {
      throw new AuthClientArgError("code must be 6 digits");
    }
    const resp = await this.post<TotpRecoverRequest, TotpRecoverResponse>(
      "/totp/recover",
      { handle, code: normalized },
    );
    return {
      accountId: base64UrlDecodeNoPad(resp.account_id_b64),
      sessionToken: resp.session_token,
      sessionExpiresAtMs: resp.session_expires_at_ms,
    };
  }

  // -----------------------------------------------------------------
  // Recovery contact — ADR-0018 factor (d), phone / email OTP
  // recovery
  // -----------------------------------------------------------------

  /**
   * `POST /recovery-contact/enroll/start` — store a phone / email
   * and send a 6-digit confirmation code (ADR-0018 factor (d),
   * enrollment step 1).
   *
   * Bearer-authenticated (the already-logged-in account's session
   * token rides the `Authorization` header (no proof of work since ADR-0054 S4; limited server-side). The enrollment
   * only becomes usable for recovery after
   * `recoveryContactEnrollFinish` confirms the code.
   *
   * A 503 `AuthHttpError` means the server has the factor disabled
   * (`LAKKO_RECOVERY_CONTACT_PEPPER` unset); a 400 means the contact
   * is malformed per the server's normalization.
   */
  async recoveryContactEnrollStart(
    sessionToken: string,
    kind: ContactKind,
    contact: string,
  ): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    if (contact.trim().length === 0) {
      throw new AuthClientArgError("contact must be non-empty");
    }
    await this.post<
      RecoveryContactEnrollStartRequest,
      RecoveryContactSentResponse
    >(
      "/recovery-contact/enroll/start",
      { kind, contact: contact.trim() },
      sessionToken,
    );
  }

  /**
   * `POST /recovery-contact/enroll/finish` — confirm the contact
   * with the code that was delivered (ADR-0018 factor (d),
   * enrollment step 2).
   *
   * Bearer-authenticated, no proof of work.
   * A 401 `AuthHttpError` means the code did not verify (wrong /
   * expired / replayed); the user can retry by re-sending a code.
   */
  async recoveryContactEnrollFinish(
    sessionToken: string,
    kind: ContactKind,
    code: string,
  ): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const normalized = normalizeTotpCode(code);
    if (normalized === null) {
      throw new AuthClientArgError("code must be 6 digits");
    }
    await this.post<RecoveryContactEnrollFinishRequest, unknown>(
      "/recovery-contact/enroll/finish",
      { kind, code: normalized },
      sessionToken,
    );
  }

  /**
   * `POST /recovery-contact/recover/start` — request a fresh
   * recovery code be sent to a previously-confirmed contact
   * (ADR-0018 factor (d), recovery step 1).
   *
   * Anonymous (the caller lost their device (no proof of work since ADR-0054 S4; limited server-side). The server is
   * ENUMERATION-SAFE by design: it returns the same `{ sent: true }`
   * whether or not the supplied contact matches a confirmed row, so
   * a 2xx here reveals nothing about account / contact existence — it
   * just means "advance to the code step". A 503 means the factor is
   * disabled server-side; a 400 means a malformed contact.
   */
  async recoveryContactRecoverStart(
    handle: string,
    kind: ContactKind,
    contact: string,
  ): Promise<void> {
    requireHandle(handle);
    if (contact.trim().length === 0) {
      throw new AuthClientArgError("contact must be non-empty");
    }
    await this.post<
      RecoveryContactRecoverStartRequest,
      RecoveryContactSentResponse
    >("/recovery-contact/recover/start", {
      handle,
      kind,
      contact: contact.trim(),
    });
  }

  /**
   * `POST /recovery-contact/recover/finish` — recover ACCESS with
   * the code that was delivered (ADR-0018 factor (d), recovery
   * step 2).
   *
   * Anonymous, limited server-side (ADR-0054 S4). Enumeration-safe: unknown account, no live
   * challenge, wrong code and replayed code all surface the SAME
   * generic 401 `AuthHttpError`; 429 signals the attempt-cap
   * lockout; 503 means the factor is disabled server-side.
   *
   * Grants ACCESS only — the result carries no key material.
   * Identity acquisition is the caller's job (ADR-0017 state
   * machine).
   */
  async recoveryContactRecoverFinish(
    handle: string,
    kind: ContactKind,
    code: string,
  ): Promise<RecoveryContactRecoverResult> {
    requireHandle(handle);
    const normalized = normalizeTotpCode(code);
    if (normalized === null) {
      throw new AuthClientArgError("code must be 6 digits");
    }
    const resp = await this.post<
      RecoveryContactRecoverFinishRequest,
      RecoveryContactRecoverFinishResponse
    >("/recovery-contact/recover/finish", {
      handle,
      kind,
      code: normalized,
    });
    return {
      accountId: base64UrlDecodeNoPad(resp.account_id_b64),
      sessionToken: resp.session_token,
      sessionExpiresAtMs: resp.session_expires_at_ms,
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
   * also generates a fresh BIP39 24-word mnemonic, enrols it as
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
        sessionToken: finishResp.session_token,
        sessionExpiresAtMs: finishResp.session_expires_at_ms,
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
  async login(handle: string, password: Uint8Array): Promise<LoginResult> {
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
    });
    if (!finishResp.ok) {
      throw new AuthHttpError(200, "/login/finish", "server reported ok=false");
    }

    return {
      accountId: accountIdOfFinish(finishResp.account_id, "/login/finish"),
      sessionKey: out.sessionKey,
      exportKey: out.exportKey,
      sessionToken: finishResp.session_token,
      sessionExpiresAtMs: finishResp.session_expires_at_ms,
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
   * `KIND_RECOVERY` envelope stays intact, so the same 24 words
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
      sessionToken: finishResp.session_token,
      sessionExpiresAtMs: finishResp.session_expires_at_ms,
    };
  }

  // -----------------------------------------------------------------
  // Internal HTTP helper
  // -----------------------------------------------------------------

  // -----------------------------------------------------------------
  // Word vault — ADR-0049, three-word identity recovery
  // -----------------------------------------------------------------

  /** `GET /vault` — whether this account has a word vault and at which
   *  positions. Bearer-authenticated. A 503 means the server has the vault
   *  disabled (`LAKKO_VAULT_ENC_KEY` unset). */
  async wordVaultStatus(sessionToken: string): Promise<WordVaultStatusResponse> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    return this.get<WordVaultStatusResponse>("/vault", sessionToken);
  }

  /** `POST /vault/enroll` — store (or replace) the account's word vault.
   *  Bearer-authenticated, no proof of work. */
  async wordVaultEnroll(sessionToken: string, req: WordVaultEnrollRequest): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    await this.post<WordVaultEnrollRequest, unknown>("/vault/enroll", req, sessionToken);
  }

  /** `POST /vault/recover/start` — the positions, salt and Argon2 cost to
   *  answer with. Anonymous, limited server-side (ADR-0054 S4); an unknown account gets stable decoys, so
   *  a 200 says nothing about whether the account exists. */
  async wordVaultRecoverStart(handle: string): Promise<WordVaultRecoverStartResponse> {
    requireHandle(handle);
    return this.post<WordVaultRecoverStartRequest, WordVaultRecoverStartResponse>(
      "/vault/recover/start",
      { handle },
    );
  }

  /** `POST /vault/recover/finish` — submit the guess. Anonymous, limited server-side (ADR-0054 S4). A
   *  generic 401 `AuthHttpError` means a wrong answer, an unknown account or
   *  a destroyed vault (indistinguishable by design); 429 is the
   *  5-in-15-minutes lockout; 503 means the vault is disabled server-side.
   *  Returns the sealed entropy the wasm `WordVaultAnswer.open` unseals. */
  async wordVaultRecoverFinish(handle: string, verifier: Uint8Array): Promise<Uint8Array> {
    requireHandle(handle);
    if (verifier.length !== 32) {
      throw new AuthClientArgError("verifier must be 32 bytes");
    }
    const resp = await this.post<WordVaultRecoverFinishRequest, WordVaultRecoverFinishResponse>(
      "/vault/recover/finish",
      {
        handle,
        verifier_b64: base64Encode(verifier),
      },
    );
    return base64Decode(resp.sealed_entropy_b64);
  }

  /** `PUT /vault/passkey` — store the entropy sealed under one of this
   *  account's passkeys (ADR-0049 section 3). Bearer-authenticated. */
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

  // -----------------------------------------------------------------
  // Device requests — ADR-0049 section 2, the camera-less add-device
  // confirmation
  // -----------------------------------------------------------------

  /** `POST /device/request` — the fresh device posts its pairing payload.
   *  Anonymous, limited server-side (ADR-0054 S4). A 200 says nothing about whether the account exists. */
  async deviceRequestCreate(
    accountId: Uint8Array,
    pairingPayload: Uint8Array,
  ): Promise<{ requestId: string; expiresInSecs: number }> {
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    const resp = await this.post<DeviceRequestCreateRequest, DeviceRequestCreateResponse>(
      "/device/request",
      {
        account_id_b64: base64UrlEncodeNoPad(accountId),
        pairing_payload_b64: base64Encode(pairingPayload),
      },
    );
    return { requestId: resp.request_id_b64, expiresInSecs: resp.expires_in_secs };
  }

  /** `POST /device/request/status` — is the request still open? */
  async deviceRequestPending(requestId: string): Promise<boolean> {
    const resp = await this.post<DeviceRequestStatusRequest, DeviceRequestStatusResponse>(
      "/device/request/status",
      { request_id_b64: requestId },
    );
    return resp.pending;
  }

  /** `GET /device/requests` — the pending request for the signed-in
   *  account, or `null`. Bearer-authenticated. */
  async deviceRequestsList(
    sessionToken: string,
  ): Promise<{ requestId: string; pairingPayload: Uint8Array; createdAtMs: number } | null> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const resp = await this.get<DeviceRequestsListResponse>("/device/requests", sessionToken);
    if (resp.request === null) return null;
    return {
      requestId: resp.request.request_id_b64,
      pairingPayload: base64Decode(resp.request.pairing_payload_b64),
      createdAtMs: resp.request.created_at_ms,
    };
  }

  /** `DELETE /device/requests/{id}` — decline, or consume after a confirmed
   *  pairing. Bearer-authenticated. */
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
  async bindLoginHandle(sessionToken: string, handle: string): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    requireHandle(handle);
    await this.request<LoginHandlePutRequest, LoginHandlePutResponse>(
      "PUT",
      "/auth/login-handle",
      { handle },
      sessionToken,
    );
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
  ): Promise<Res> {
    const url = `${this.baseUrl}${endpoint}`;
    const headers: Record<string, string> = { authorization: `Bearer ${bearer}` };
    if (body !== undefined) {
      headers["content-type"] = "application/json";
    }
    let response: Response;
    try {
      response = await fetch(url, {
        method,
        headers,
        body: body === undefined ? null : JSON.stringify(body),
      });
    } catch (err) {
      throw new AuthNetworkError(endpoint, err);
    }
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new AuthHttpError(response.status, endpoint, text, retryAfterHeader(response));
    }
    return (await response.json()) as Res;
  }

  private async get<Res>(endpoint: string, bearer: string): Promise<Res> {
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "GET",
        headers: { authorization: `Bearer ${bearer}` },
      });
    } catch (err) {
      throw new AuthNetworkError(endpoint, err);
    }
    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new AuthHttpError(response.status, endpoint, text, retryAfterHeader(response));
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
      response = await fetch(url, {
        method: "POST",
        headers,
        body: body === undefined ? null : JSON.stringify(body),
      });
    } catch (err) {
      throw new AuthNetworkError(endpoint, err);
    }

    if (!response.ok) {
      const text = await safeReadBody(response);
      throw new AuthHttpError(response.status, endpoint, text, retryAfterHeader(response));
    }

    // Some endpoints return small JSON, others return `{ ok: true }`.
    // Both are JSON; force the parse so a non-JSON 200 is loud.
    return (await response.json()) as Res;
  }
}

/** A login handle on the wire: base64url without padding of 32 bytes. */
const HANDLE_WIRE = /^[A-Za-z0-9_-]{43}$/;

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
