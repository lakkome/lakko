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
 *      `/pow/challenge` → `/signup/start` → `/pow/challenge` →
 *      `/signup/finish` (and the equivalent for login). They
 *      drive the OPAQUE WASM classes (`OpaqueRegister` /
 *      `OpaqueLogin`) and the PoW solver.
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
  DeviceAuthorizeStartRequest,
  DeviceAuthorizeStartResponse,
  GenesisAttestRequest,
  GenesisAttestResponse,
  LoginFinishRequest,
  LoginFinishResponse,
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
  TotpEnrollStartRequest,
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
  ) {
    super(`auth ${endpoint} → ${status}: ${body}`);
  }
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

  /** `POST /pow/challenge`. */
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
   * holds a session) + PoW-gated. Returns the URL-safe base64
   * (no pad) of the RAW grant token — the server stores only its
   * SHA-256. The caller seals it (together with the phrase seed)
   * to the fresh device over the E2EE pairing channel; it never
   * travels in plaintext past this response.
   */
  async deviceAuthorizeStart(sessionToken: string): Promise<string> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);
    const resp = await this.post<
      DeviceAuthorizeStartRequest,
      DeviceAuthorizeStartResponse
    >("/device/authorize/start", { pow }, sessionToken);
    return resp.grant_token_b64;
  }

  /**
   * `POST /device/authorize/redeem` — atomically consume a
   * single-use device grant for a session-token triple (ADR-0018
   * factor (a), fresh-device side).
   *
   * Anonymous (the fresh device has no token yet) + PoW-gated. A
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
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);
    const resp = await this.post<
      DeviceAuthorizeRedeemRequest,
      DeviceAuthorizeRedeemResponse
    >("/device/authorize/redeem", {
      pow,
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
   * Bearer-authenticated (NO PoW): holding a valid session token *is*
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
   * Bearer-authenticated (the already-logged-in account) + PoW.
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
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);
    const resp = await this.post<TotpEnrollStartRequest, TotpEnrollStartResponse>(
      "/totp/enroll/start",
      { pow },
      sessionToken,
    );
    return { otpauthUri: resp.otpauth_uri, secretB32: resp.secret_b32 };
  }

  /**
   * `POST /totp/enroll/finish` — confirm the enrollment with the
   * first code the authenticator produced (ADR-0018 factor (c),
   * enrollment step 2).
   *
   * Bearer-authenticated, NO PoW — the server does not gate finish.
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
   * Anonymous (the caller lost their device) + PoW. The server is
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
    accountId: Uint8Array,
    code: string,
  ): Promise<TotpRecoverResult> {
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    const normalized = normalizeTotpCode(code);
    if (normalized === null) {
      throw new AuthClientArgError("code must be 6 digits");
    }
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);
    const resp = await this.post<TotpRecoverRequest, TotpRecoverResponse>(
      "/totp/recover",
      {
        pow,
        account_id_b64: base64UrlEncodeNoPad(accountId),
        code: normalized,
      },
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
   * token rides the `Authorization` header) + PoW. The enrollment
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
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);
    await this.post<
      RecoveryContactEnrollStartRequest,
      RecoveryContactSentResponse
    >(
      "/recovery-contact/enroll/start",
      { pow, kind, contact: contact.trim() },
      sessionToken,
    );
  }

  /**
   * `POST /recovery-contact/enroll/finish` — confirm the contact
   * with the code that was delivered (ADR-0018 factor (d),
   * enrollment step 2).
   *
   * Bearer-authenticated, NO PoW — the server does not gate finish.
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
   * Anonymous (the caller lost their device) + PoW. The server is
   * ENUMERATION-SAFE by design: it returns the same `{ sent: true }`
   * whether or not the supplied contact matches a confirmed row, so
   * a 2xx here reveals nothing about account / contact existence — it
   * just means "advance to the code step". A 503 means the factor is
   * disabled server-side; a 400 means a malformed contact.
   */
  async recoveryContactRecoverStart(
    accountId: Uint8Array,
    kind: ContactKind,
    contact: string,
  ): Promise<void> {
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    if (contact.trim().length === 0) {
      throw new AuthClientArgError("contact must be non-empty");
    }
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);
    await this.post<
      RecoveryContactRecoverStartRequest,
      RecoveryContactSentResponse
    >("/recovery-contact/recover/start", {
      pow,
      account_id_b64: base64UrlEncodeNoPad(accountId),
      kind,
      contact: contact.trim(),
    });
  }

  /**
   * `POST /recovery-contact/recover/finish` — recover ACCESS with
   * the code that was delivered (ADR-0018 factor (d), recovery
   * step 2).
   *
   * Anonymous + PoW. Enumeration-safe: unknown account, no live
   * challenge, wrong code and replayed code all surface the SAME
   * generic 401 `AuthHttpError`; 429 signals the attempt-cap
   * lockout; 503 means the factor is disabled server-side.
   *
   * Grants ACCESS only — the result carries no key material.
   * Identity acquisition is the caller's job (ADR-0017 state
   * machine).
   */
  async recoveryContactRecoverFinish(
    accountId: Uint8Array,
    kind: ContactKind,
    code: string,
  ): Promise<RecoveryContactRecoverResult> {
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    const normalized = normalizeTotpCode(code);
    if (normalized === null) {
      throw new AuthClientArgError("code must be 6 digits");
    }
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);
    const resp = await this.post<
      RecoveryContactRecoverFinishRequest,
      RecoveryContactRecoverFinishResponse
    >("/recovery-contact/recover/finish", {
      pow,
      account_id_b64: base64UrlEncodeNoPad(accountId),
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
   *   POST /pow/challenge   →  seed₁
   *   client: solve PoW + OPAQUE password start [+ recovery start]
   *   POST /signup/start    →  password_response [+ recovery_response]
   *   client: OPAQUE finish (both flows)
   *   POST /pow/challenge   →  seed₂
   *   client: solve PoW
   *   POST /signup/finish   →  ok
   * ```
   *
   * When `options.enrolRecovery !== false` (default), this method
   * also generates a fresh BIP39 24-word mnemonic, enrols it as
   * a parallel OPAQUE envelope under `KIND_RECOVERY` server-side,
   * and returns the phrase in `result.recoveryPhrase` for the UI
   * to display **once**. Both envelopes commit atomically inside
   * `/signup/finish` (server transaction).
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

    const wasm = await loadWasm();
    const accountId = generateAccountId();
    const accountIdB64 = base64Encode(accountId);

    // ------------- start
    const challenge1 = await this.powChallenge();
    const pow1 = await solvePow(challenge1.seed, challenge1.effort_bits);

    // Password OPAQUE start.
    const passwordReg = wasm.OpaqueRegister.startPassword(password);

    // Recovery side: generate a fresh mnemonic + derive the
    // BIP39 64-byte seed; that seed is the OPAQUE secret for the
    // KIND_RECOVERY envelope. We hold the seed + the
    // OPAQUE state in a typed bundle so the conditional flow
    // doesn't need awkward null re-checks later.
    interface RecoveryBundle {
      readonly mnemonicPhrase: string;
      readonly seed: Uint8Array;
      readonly opaque: ReturnType<typeof wasm.OpaqueRegister.startRecovery>;
    }
    let recovery: RecoveryBundle | null = null;
    if (enrolRecovery) {
      const mnemonic = wasm.RecoveryMnemonic.generate();
      const seed = mnemonic.toSeed();
      recovery = {
        mnemonicPhrase: mnemonic.phrase,
        seed,
        opaque: wasm.OpaqueRegister.startRecovery(seed),
      };
    }

    const startResp = await this.signupStart({
      pow: pow1,
      account_id: accountIdB64,
      password_request: passwordReg.requestB64,
      ...(recovery !== null
        ? { recovery_request: recovery.opaque.requestB64 }
        : {}),
    });

    if (recovery !== null && startResp.recovery_response === undefined) {
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
      recovery !== null && startResp.recovery_response !== undefined
        ? recovery.opaque.finish(recovery.seed, startResp.recovery_response).uploadB64
        : undefined;

    const challenge2 = await this.powChallenge();
    const pow2 = await solvePow(challenge2.seed, challenge2.effort_bits);

    const finishResp = await this.signupFinish({
      pow: pow2,
      account_id: accountIdB64,
      password_upload: passwordOut.uploadB64,
      ...(recoveryUploadB64 !== undefined
        ? { recovery_upload: recoveryUploadB64 }
        : {}),
    });
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
      ...(recovery !== null ? { recoveryPhrase: recovery.mnemonicPhrase } : {}),
      sessionToken: finishResp.session_token,
      sessionExpiresAtMs: finishResp.session_expires_at_ms,
    };
  }

  /**
   * Drive the full password-login flow:
   *
   * ```text
   *   POST /pow/challenge  →  seed
   *   client: solve PoW + OPAQUE start
   *   POST /login/start    →  session_id, response
   *   client: OPAQUE finish
   *   POST /login/finish   →  ok, account_id
   * ```
   *
   * `accountId` MUST be the raw 16-byte id the server gave back
   * at signup time. The web client persists it in `localStorage`
   * keyed by username (1.16c3b minimal scope).
   *
   * Wrong passwords surface as an `AuthHttpError` with status
   * 401 — by design, indistinguishable from "unknown account".
   */
  async login(accountId: Uint8Array, password: Uint8Array): Promise<LoginResult> {
    if (password.length === 0) {
      throw new AuthClientArgError("password must be non-empty");
    }
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }

    const wasm = await loadWasm();
    const accountIdB64 = base64Encode(accountId);

    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    const login = wasm.OpaqueLogin.startPassword(password);
    const startResp = await this.loginStart({
      pow,
      account_id: accountIdB64,
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
      accountId,
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
   *   POST /pow/challenge          →  seed
   *   client: solve PoW
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
   * Note that the recovery flow only requires **one** PoW solve
   * (gating `/start`), unlike signup which needs two (one per
   * server-side write). Wrong-mnemonic attempts fail at the AKE
   * finalization with the same indistinguishable response as
   * `/recovery/login/finish`.
   *
   * @param accountId    Raw 16-byte UUIDv7. The recovery flow can
   *                     only target an account whose id is known
   *                     locally; until 1.16d's discovery service
   *                     lands, that means the account that
   *                     signed up on this device.
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
    accountId: Uint8Array,
    mnemonicSeed: Uint8Array,
    newPassword: Uint8Array,
  ): Promise<RecoverResult> {
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    if (mnemonicSeed.length === 0) {
      throw new AuthClientArgError("mnemonicSeed must be non-empty");
    }
    if (newPassword.length === 0) {
      throw new AuthClientArgError("newPassword must be non-empty");
    }

    const wasm = await loadWasm();
    const accountIdB64 = base64Encode(accountId);

    // Single PoW solve — the server only gates /start.
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);

    // Two parallel OPAQUE flows: recovery login proves the user
    // knows the mnemonic; password registration enrols the new
    // password envelope. Both run independently — neither
    // depends on the other's intermediate state.
    const recoveryLogin = wasm.OpaqueLogin.startRecovery(mnemonicSeed);
    const newPasswordReg = wasm.OpaqueRegister.startPassword(newPassword);

    const startResp = await this.passwordResetStart({
      pow,
      account_id: accountIdB64,
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
      accountId,
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
   *  Bearer-authenticated, no PoW. */
  async wordVaultEnroll(sessionToken: string, req: WordVaultEnrollRequest): Promise<void> {
    if (sessionToken.length === 0) {
      throw new AuthClientArgError("sessionToken must be non-empty");
    }
    await this.post<WordVaultEnrollRequest, unknown>("/vault/enroll", req, sessionToken);
  }

  /** `POST /vault/recover/start` — the positions, salt and Argon2 cost to
   *  answer with. Anonymous + PoW; an unknown account gets stable decoys, so
   *  a 200 says nothing about whether the account exists. */
  async wordVaultRecoverStart(accountId: Uint8Array): Promise<WordVaultRecoverStartResponse> {
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);
    return this.post<WordVaultRecoverStartRequest, WordVaultRecoverStartResponse>(
      "/vault/recover/start",
      { pow, account_id_b64: base64UrlEncodeNoPad(accountId) },
    );
  }

  /** `POST /vault/recover/finish` — submit the guess. Anonymous + PoW. A
   *  generic 401 `AuthHttpError` means a wrong answer, an unknown account or
   *  a destroyed vault (indistinguishable by design); 429 is the
   *  5-in-15-minutes lockout; 503 means the vault is disabled server-side.
   *  Returns the sealed entropy the wasm `WordVaultAnswer.open` unseals. */
  async wordVaultRecoverFinish(accountId: Uint8Array, verifier: Uint8Array): Promise<Uint8Array> {
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    if (verifier.length !== 32) {
      throw new AuthClientArgError("verifier must be 32 bytes");
    }
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);
    const resp = await this.post<WordVaultRecoverFinishRequest, WordVaultRecoverFinishResponse>(
      "/vault/recover/finish",
      {
        pow,
        account_id_b64: base64UrlEncodeNoPad(accountId),
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
   *  Anonymous + PoW. A 200 says nothing about whether the account exists. */
  async deviceRequestCreate(
    accountId: Uint8Array,
    pairingPayload: Uint8Array,
  ): Promise<{ requestId: string; expiresInSecs: number }> {
    if (accountId.length !== 16) {
      throw new AuthClientArgError("accountId must be 16 bytes");
    }
    const challenge = await this.powChallenge();
    const pow = await solvePow(challenge.seed, challenge.effort_bits);
    const resp = await this.post<DeviceRequestCreateRequest, DeviceRequestCreateResponse>(
      "/device/request",
      {
        pow,
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
      throw new AuthHttpError(response.status, endpoint, text);
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
      throw new AuthHttpError(response.status, endpoint, text);
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
      throw new AuthHttpError(response.status, endpoint, text);
    }

    // Some endpoints return small JSON, others return `{ ok: true }`.
    // Both are JSON; force the parse so a non-JSON 200 is loud.
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
