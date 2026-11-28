// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The recovery-email and account-notice routes of `services/auth`
 * (ADR-0075 sections 6, 7 and 9), one method per route.
 *
 * Encodings: `handle`, `challenge_id` and `ticket` are URL-safe base64
 * without padding; the OPAQUE messages are standard base64. A refusal is
 * `{"error": code}` and throws `AuthHttpError`; no answer throws
 * `AuthNetworkError`.
 *
 * Amended 2026-11-26 (ADR-0075, second amendment): the email vault is gone.
 * No route here takes or returns keys: a mailed code confirms the address,
 * gives a new password, and nothing else.
 *
 * The address and the codes pass through here on their way to the server
 * and are never logged or kept.
 */
import { base64UrlDecodeNoPad } from "../account-id";
import { AuthHttpError, AuthNetworkError, DEFAULT_BASE_URL, authHttpErrorFrom } from "../client";
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

export interface RecoverVerified {
  readonly ticket: string;
  readonly totpRequired: boolean;
}

/** The session `recover/password/finish` lands. It mints no fresh grant. */
export interface EmailPasswordResult {
  readonly accountId: Uint8Array;
  readonly session: SessionPair;
  /** The export key of the password just set (64 bytes). On a device that
   *  holds the account's keys it is what the password's seal and the
   *  owned-channels password lock are made from (ADR-0101 sections 7 and
   *  10); a secret, memory only. */
  readonly exportKey: Uint8Array;
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
      // ADR-0099: a suspended account's refusal is read whole.
      if (response.status === 403) throw await authHttpErrorFrom(response, endpoint);
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

  /** Bearer `POST /recovery-email/enrol/start`. */
  async enrolStart(args: {
    readonly sessionToken: string;
    readonly freshGrant: string;
    readonly handle: string;
    readonly address: string;
    readonly lang: string;
  }): Promise<CodeSent> {
    const endpoint = "/recovery-email/enrol/start";
    const res = await this.send(
      "POST",
      endpoint,
      { fresh_grant: args.freshGrant, handle: args.handle, address: args.address, lang: args.lang },
      args.sessionToken,
    );
    return {
      challengeId: str(res["challenge_id"], endpoint, "challenge_id"),
      resendAfterSecs: secs(res["resend_after_secs"]),
      expiresInSecs: secs(res["expires_in_secs"]),
    };
  }

  /** Bearer `POST /recovery-email/enrol/finish`. */
  async enrolFinish(args: {
    readonly sessionToken: string;
    readonly challengeId: string;
    readonly code: string;
  }): Promise<{ readonly enabled: boolean }> {
    const res = await this.send(
      "POST",
      "/recovery-email/enrol/finish",
      { challenge_id: args.challengeId, code: args.code },
      args.sessionToken,
    );
    return { enabled: res["enabled"] === true };
  }

  /** Bearer `DELETE /recovery-email`. `409 last-factor` when it is the
   *  account's last second way in (`factors.ts`). */
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

  /**
   * The OPAQUE registration of the new password under the
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
    const finished = reg.finish(
      args.newPassword,
      str(start["new_password_response"], "/recovery-email/recover/password/start", "new_password_response"),
    );
    const upload = finished.uploadB64;
    const exportKey = finished.exportKey;
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
    return { accountId, session: sessionPairOf(finish as Partial<SessionPairWire>), exportKey };
  }

  /** Bearer `GET /account/notices?after=<id>`, or `?since_ms=<unix ms>` for
   *  a device that has no cursor yet. */
  async notices(sessionToken: string, query: NoticeQuery): Promise<NoticePage> {
    const endpoint = "/account/notices";
    const param = `after=${encodeURIComponent(String(query.after))}`;
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
