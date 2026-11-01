// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recovery-email and account-notice routes of `services/auth`
 * (ADR-0075 sections 6, 7 and 9), one method per route.
 *
 * Encodings: `handle`, `challenge_id` and `ticket` are URL-safe base64
 * without padding; `vault_salt` (16 bytes), `sealed_entropy` (44 bytes) and
 * the OPAQUE messages are standard base64. A refusal is `{"error": code}`
 * and throws `AuthHttpError`; no answer throws `AuthNetworkError`.
 *
 * The address and the codes pass through here on their way to the server
 * and are never logged or kept.
 */
import { base64UrlDecodeNoPad } from "../account-id";
import { AuthHttpError, AuthNetworkError, DEFAULT_BASE_URL } from "../client";
import { sessionFetch } from "../session-fetch";
import { sessionPairOf, type SessionPair, type SessionPairWire } from "../session-pair";
import { loadWasm } from "../../wasm";
import {
  parseRecoveryEmailStatus,
  type RecoveryEmailStatus,
} from "./account-protection";
import { parseNoticePage, type NoticePage, type NoticeQuery } from "./notices";

type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

/** The answer of both start routes that send a letter. */
export interface CodeSent {
  readonly challengeId: string;
  readonly resendAfterSecs: number;
  readonly expiresInSecs: number;
}

export interface EnrolStarted extends CodeSent {
  /** Standard base64 of the 16-byte salt the email vault is sealed with. */
  readonly vaultSalt: string;
}

export interface RecoverVerified {
  readonly ticket: string;
  readonly totpRequired: boolean;
}

/** What `recover/open` released. */
export type RecoverOpened =
  | { readonly mode: "vault"; readonly vaultSalt: string; readonly sealedEntropy: string }
  | { readonly mode: "password" };

/** What `GET /account/phrase` answers. */
export interface PhraseStatus {
  /** The account's owner saved a recovery phrase. */
  readonly saved: boolean;
  /** Saving one takes a key rotation: the phrase was ever in an email vault. */
  readonly rotationNeeded: boolean;
}

/** The session `recover/password/finish` lands. It mints no fresh grant. */
export interface EmailPasswordResult {
  readonly accountId: Uint8Array;
  readonly session: SessionPair;
}

function str(value: unknown, endpoint: string, field: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new AuthHttpError(200, endpoint, `${field} is missing`);
  }
  return value;
}

function secs(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) ? Math.max(0, Math.ceil(value)) : 0;
}

function retryAfterHeader(response: Response): number | null {
  const raw = response.headers.get("retry-after");
  if (raw === null) return null;
  const n = Number(raw);
  return Number.isFinite(n) ? Math.max(0, Math.ceil(n)) : null;
}

async function safeBody(response: Response): Promise<string> {
  try {
    const text = await response.text();
    return text.length > 512 ? text.slice(0, 512) : text;
  } catch {
    return "";
  }
}

export class RecoveryEmailClient {
  constructor(
    private readonly baseUrl: string = DEFAULT_BASE_URL,
    private readonly fetchImpl: FetchLike = (input, init) => sessionFetch(input, init),
  ) {}

  private async send(
    method: "GET" | "POST" | "PUT" | "DELETE",
    endpoint: string,
    body: unknown,
    bearer: string | null,
  ): Promise<Record<string, unknown>> {
    const headers: Record<string, string> = {};
    if (body !== undefined) headers["content-type"] = "application/json";
    if (bearer !== null) headers["authorization"] = `Bearer ${bearer}`;
    let response: Response;
    try {
      response = await this.fetchImpl(`${this.baseUrl}${endpoint}`, {
        method,
        headers,
        body: body === undefined ? null : JSON.stringify(body),
      });
    } catch (err) {
      throw new AuthNetworkError(endpoint, err);
    }
    if (!response.ok) {
      throw new AuthHttpError(response.status, endpoint, await safeBody(response), retryAfterHeader(response));
    }
    if (response.status === 204) return {};
    const parsed: unknown = await response.json();
    return typeof parsed === "object" && parsed !== null ? (parsed as Record<string, unknown>) : {};
  }

  /** Anonymous `GET /recovery-email/available`. */
  async available(): Promise<boolean> {
    const res = await this.send("GET", "/recovery-email/available", undefined, null);
    return res["available"] === true;
  }

  /** Bearer `GET /recovery-email`: the session's account only. */
  async status(sessionToken: string): Promise<RecoveryEmailStatus> {
    const endpoint = "/recovery-email";
    const status = parseRecoveryEmailStatus(await this.send("GET", endpoint, undefined, sessionToken));
    if (status === null) throw new AuthHttpError(200, endpoint, "not a status body");
    return status;
  }

  /** Bearer `GET /account/phrase`: which way this account saves a phrase. It
   *  answers on a server that sends no mail too, where `/recovery-email` is
   *  `503` (ADR-0075 section 3, amended 2026-11-01). */
  async phraseStatus(sessionToken: string): Promise<PhraseStatus> {
    const endpoint = "/account/phrase";
    const res = await this.send("GET", endpoint, undefined, sessionToken);
    if (typeof res["saved"] !== "boolean" || typeof res["rotation_needed"] !== "boolean") {
      throw new AuthHttpError(200, endpoint, "not a phrase status body");
    }
    return { saved: res["saved"], rotationNeeded: res["rotation_needed"] };
  }

  /** Bearer `POST /account/phrase/saved`: the person checked the words this
   *  device holds. `409 phrase-save-needs-rotation` when the phrase was ever
   *  in an email vault. Idempotent. */
  async markPhraseSaved(sessionToken: string, freshGrant: string): Promise<void> {
    await this.send("POST", "/account/phrase/saved", { fresh_grant: freshGrant }, sessionToken);
  }

  /** Bearer `POST /recovery-email/enrol/start`. */
  async enrolStart(args: {
    readonly sessionToken: string;
    readonly freshGrant: string;
    readonly handle: string;
    readonly address: string;
    readonly lang: string;
  }): Promise<EnrolStarted> {
    const endpoint = "/recovery-email/enrol/start";
    const res = await this.send(
      "POST",
      endpoint,
      { fresh_grant: args.freshGrant, handle: args.handle, address: args.address, lang: args.lang },
      args.sessionToken,
    );
    return {
      challengeId: str(res["challenge_id"], endpoint, "challenge_id"),
      vaultSalt: str(res["vault_salt"], endpoint, "vault_salt"),
      resendAfterSecs: secs(res["resend_after_secs"]),
      expiresInSecs: secs(res["expires_in_secs"]),
    };
  }

  /** Bearer `POST /recovery-email/enrol/finish`. `sealedEntropy` (standard
   *  base64, 44 bytes) only for an account without a saved phrase. */
  async enrolFinish(args: {
    readonly sessionToken: string;
    readonly challengeId: string;
    readonly code: string;
    readonly sealedEntropy: string | null;
  }): Promise<{ readonly enabled: boolean; readonly vault: boolean }> {
    const body: Record<string, string> = { challenge_id: args.challengeId, code: args.code };
    if (args.sealedEntropy !== null) body["sealed_entropy"] = args.sealedEntropy;
    const res = await this.send("POST", "/recovery-email/enrol/finish", body, args.sessionToken);
    return { enabled: res["enabled"] === true, vault: res["vault"] === true };
  }

  /**
   * Bearer `PUT /recovery-email/vault`: put the email vault back after the
   * account's keys changed (the automatic re-seal). No fresh-auth grant. The
   * salt is the client's own (16 random bytes). Every well-formed request
   * spends one of ten a day: never called in a loop.
   */
  async putVault(args: {
    readonly sessionToken: string;
    readonly handle: string;
    readonly address: string;
    /** Standard base64 of 16 bytes. */
    readonly vaultSalt: string;
    /** Standard base64 of the 44-byte blob. */
    readonly sealedEntropy: string;
  }): Promise<void> {
    await this.send(
      "PUT",
      "/recovery-email/vault",
      {
        handle: args.handle,
        address: args.address,
        vault_salt: args.vaultSalt,
        sealed_entropy: args.sealedEntropy,
      },
      args.sessionToken,
    );
  }

  /** Bearer `DELETE /recovery-email`. */
  async remove(sessionToken: string, freshGrant: string): Promise<void> {
    await this.send("DELETE", "/recovery-email", { fresh_grant: freshGrant }, sessionToken);
  }

  /** Anonymous `POST /recovery-email/recover/start`. The same answer for a
   *  matching pair, a wrong pair, an unknown name and no email. */
  async recoverStart(handle: string, address: string, lang: string): Promise<CodeSent> {
    const endpoint = "/recovery-email/recover/start";
    const res = await this.send("POST", endpoint, { handle, address, lang }, null);
    return {
      challengeId: str(res["challenge_id"], endpoint, "challenge_id"),
      resendAfterSecs: secs(res["resend_after_secs"]),
      expiresInSecs: secs(res["expires_in_secs"]),
    };
  }

  /** `POST /recovery-email/recover/verify`. */
  async recoverVerify(challengeId: string, code: string): Promise<RecoverVerified> {
    const endpoint = "/recovery-email/recover/verify";
    const res = await this.send("POST", endpoint, { challenge_id: challengeId, code }, null);
    return {
      ticket: str(res["ticket"], endpoint, "ticket"),
      totpRequired: res["totp_required"] === true,
    };
  }

  /** `POST /recovery-email/recover/totp`. */
  async recoverTotp(ticket: string, code: string): Promise<void> {
    await this.send("POST", "/recovery-email/recover/totp", { ticket, code }, null);
  }

  /** `POST /recovery-email/recover/open`. In vault mode the ticket is spent. */
  async recoverOpen(ticket: string): Promise<RecoverOpened> {
    const endpoint = "/recovery-email/recover/open";
    const res = await this.send("POST", endpoint, { ticket }, null);
    if (res["mode"] === "vault") {
      return {
        mode: "vault",
        vaultSalt: str(res["vault_salt"], endpoint, "vault_salt"),
        sealedEntropy: str(res["sealed_entropy"], endpoint, "sealed_entropy"),
      };
    }
    if (res["mode"] === "password") return { mode: "password" };
    throw new AuthHttpError(200, endpoint, "unknown mode");
  }

  /**
   * Password mode: the OPAQUE registration of the new password under the
   * ticket (`recover/password/{start,finish}`), landing a session.
   */
  async recoverPassword(args: {
    readonly ticket: string;
    readonly newPassword: Uint8Array;
    /** URL-safe base64 of this device's id, when it has keys. */
    readonly deviceIdB64?: string;
  }): Promise<EmailPasswordResult> {
    const wasm = await loadWasm();
    const reg = wasm.OpaqueRegister.startPassword(args.newPassword);
    const start = await this.send(
      "POST",
      "/recovery-email/recover/password/start",
      { ticket: args.ticket, new_password_request: reg.requestB64 },
      null,
    );
    const endpoint = "/recovery-email/recover/password/finish";
    const upload = reg.finish(
      args.newPassword,
      str(start["new_password_response"], "/recovery-email/recover/password/start", "new_password_response"),
    ).uploadB64;
    const body: Record<string, string> = { ticket: args.ticket, new_password_upload: upload };
    if (args.deviceIdB64 !== undefined) body["device_id_b64"] = args.deviceIdB64;
    const finish = await this.send("POST", endpoint, body, null);
    let accountId: Uint8Array;
    try {
      accountId = base64UrlDecodeNoPad(str(finish["account_id_b64"], endpoint, "account_id_b64"));
    } catch {
      throw new AuthHttpError(200, endpoint, "account_id_b64 is not base64url");
    }
    if (accountId.length !== 16) throw new AuthHttpError(200, endpoint, "account_id_b64 is not 16 bytes");
    return { accountId, session: sessionPairOf(finish as Partial<SessionPairWire>) };
  }

  /** Bearer `GET /account/notices?after=<id>`, or `?since_ms=<unix ms>` for
   *  a device that has no cursor yet. */
  async notices(sessionToken: string, query: NoticeQuery): Promise<NoticePage> {
    const endpoint = "/account/notices";
    const param =
      query.kind === "after"
        ? `after=${encodeURIComponent(String(query.after))}`
        : `since_ms=${encodeURIComponent(String(query.sinceMs))}`;
    const page = parseNoticePage(
      await this.send("GET", `${endpoint}?${param}`, undefined, sessionToken),
    );
    if (page === null) throw new AuthHttpError(200, endpoint, "not a notice page");
    return page;
  }
}

/** What a thrown error says, for `emailFailure`. */
export function answerOf(err: unknown): {
  readonly status: number | null;
  readonly errorCode: string | null;
  readonly retryAfterSecs: number | null;
} | null {
  if (err instanceof AuthHttpError) {
    return { status: err.status, errorCode: err.errorCode, retryAfterSecs: err.retryAfter };
  }
  if (err instanceof AuthNetworkError) return { status: null, errorCode: null, retryAfterSecs: null };
  return null;
}
