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
import { base64Encode, generateAccountId } from "./account-id";
import { solvePow } from "./pow";
import type {
  LoginFinishRequest,
  LoginFinishResponse,
  LoginStartRequest,
  LoginStartResponse,
  PowChallengeResponse,
  SignupFinishRequest,
  SignupFinishResponse,
  SignupStartRequest,
  SignupStartResponse,
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

  // -----------------------------------------------------------------
  // High-level: full signup / login flows
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
    };
  }

  // -----------------------------------------------------------------
  // Internal HTTP helper
  // -----------------------------------------------------------------

  private async post<Req, Res>(
    endpoint: string,
    body: Req,
  ): Promise<Res> {
    const url = `${this.baseUrl}${endpoint}`;
    let response: Response;
    try {
      response = await fetch(url, {
        method: "POST",
        headers: body === undefined ? {} : { "content-type": "application/json" },
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
