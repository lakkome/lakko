// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A suspended account, seen by its owner (ADR-0089 step 3, UX
 * `reporting-and-moderation.md` section 4). The pure rules; the fetch, the
 * provider and the screen use them.
 *
 * While Lakko's moderators suspend an account, auth answers every route that
 * would hand this device a session with `403 account-suspended` and, in its
 * place, a restricted session: an `s1` credential that opens exactly the
 * routes a suspended person may use (read the suspension, send one appeal,
 * delete the account, sign out, and the passkey ceremony for the deletion),
 * and an ordinary refresh credential. The refresh route keeps answering the
 * same `403` while the suspension stands and a normal pair once it ends.
 *
 * The device then runs in "suspended mode": it keeps both credentials (as it
 * keeps the pair today), shows the suspended screen, and, after "Read my
 * chats", opens the app with nothing that would send.
 */

import { base64Encode, base64UrlDecodeNoPad } from "./account-id";
import { freshGrantOf, type FreshGrant } from "./fresh-auth";

/** The `error` code of every suspension refusal. */
export const CODE_ACCOUNT_SUSPENDED = "account-suspended";

/** How often suspended mode asks whether the suspension has ended. Chosen
 *  (UX section 4), not measured. */
export const SUSPENDED_REFRESH_INTERVAL_MS = 10 * 60_000;

/** The longest appeal, in characters (the server seals at most this many). */
export const APPEAL_MAX_CHARS = 1000;

/** What a suspended device holds instead of a session pair. */
export interface RestrictedSession {
  /** The `s1` restricted session: the bearer of the suspended routes. */
  readonly restrictedToken: string;
  /** Its expiry, unix milliseconds (as long as an access token). */
  readonly restrictedExpiresAtMs: number;
  /** An ordinary refresh credential. */
  readonly refreshToken: string;
  /** Its expiry, unix milliseconds. */
  readonly refreshExpiresAtMs: number;
  /** The key generation both were issued under. */
  readonly keyGeneration: bigint;
}

/** A landing route's `403 account-suspended` with its credentials. */
export interface SuspendedLanding {
  /** Standard base64 of the 16-byte account id, the form every store of
   *  this client keys on. */
  readonly accountIdB64: string;
  readonly session: RestrictedSession;
  /** The fresh-auth grant the password and passkey sign-ins carry, so a
   *  suspended account without a passkey can still delete itself. */
  readonly freshGrant: FreshGrant | null;
}

function isTimestamp(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

function generationOf(value: unknown): bigint | null {
  try {
    if (typeof value === "bigint") return value >= 1n ? value : null;
    if (typeof value === "number" && Number.isInteger(value) && value >= 1) return BigInt(value);
    if (typeof value === "string" && /^[0-9]+$/.test(value)) {
      const g = BigInt(value);
      return g >= 1n ? g : null;
    }
  } catch {
    return null;
  }
  return null;
}

/** The parsed JSON of a body, or `null`. */
function jsonOf(body: unknown): Record<string, unknown> | null {
  let value: unknown = body;
  if (typeof body === "string") {
    try {
      value = JSON.parse(body);
    } catch {
      return null;
    }
  }
  return value !== null && typeof value === "object" ? (value as Record<string, unknown>) : null;
}

/** `true` when a body is the suspension refusal (with or without
 *  credentials). */
export function isSuspendedBody(body: unknown): boolean {
  return jsonOf(body)?.error === CODE_ACCOUNT_SUSPENDED;
}

/**
 * The credentials of a `403 account-suspended` body, or `null` when the body
 * is not one, carries none (a bearer route's refusal), or carries a field of
 * the wrong shape. A half set of credentials is never used: a device that
 * cannot hold both cannot ask whether the suspension ended.
 */
export function suspendedLandingOf(body: unknown): SuspendedLanding | null {
  const b = jsonOf(body);
  if (b === null || b.error !== CODE_ACCOUNT_SUSPENDED) return null;
  const {
    account_id_b64: accountRaw,
    restricted_token: restrictedToken,
    restricted_expires_at_ms: restrictedExpiresAtMs,
    refresh_token: refreshToken,
    refresh_expires_at_ms: refreshExpiresAtMs,
    key_generation: generation,
  } = b;
  if (typeof accountRaw !== "string" || accountRaw.length === 0) return null;
  let accountId: Uint8Array;
  try {
    // The server writes URL-safe base64 without padding; a standard one
    // reads the same.
    accountId = base64UrlDecodeNoPad(accountRaw.replace(/=+$/, ""));
  } catch {
    return null;
  }
  if (accountId.length !== 16) return null;
  if (typeof restrictedToken !== "string" || !restrictedToken.startsWith("s1.")) return null;
  if (!isTimestamp(restrictedExpiresAtMs)) return null;
  if (typeof refreshToken !== "string" || refreshToken.length === 0) return null;
  if (!isTimestamp(refreshExpiresAtMs)) return null;
  const keyGeneration = generationOf(generation);
  if (keyGeneration === null) return null;
  return {
    accountIdB64: base64Encode(accountId),
    session: { restrictedToken, restrictedExpiresAtMs, refreshToken, refreshExpiresAtMs, keyGeneration },
    freshGrant: freshGrantOf(b as { fresh_grant?: string; fresh_grant_expires_at_ms?: number }),
  };
}

// ---------------------------------------------------------------------------
// GET /account/suspension
// ---------------------------------------------------------------------------

/** Where the appeal of this suspension stands. */
export type AppealState = "none" | "sent" | "declined";

/** What the screen shows. */
export interface SuspensionDetails {
  /** The reason byte (ADR-0089), kept for diagnostics only. */
  readonly reason: number;
  /** The reason's name as the server writes it (`child-safety`, ...). */
  readonly reasonName: string;
  /** The end, unix milliseconds, or `null` for no end date. */
  readonly untilMs: number | null;
  readonly appeal: AppealState;
}

/** The answer of `GET /account/suspension`. */
export type SuspensionStatus =
  | { readonly suspended: false }
  | { readonly suspended: true; readonly details: SuspensionDetails };

/** Read the answer, or `null` for a body of the wrong shape. */
export function suspensionStatusOf(body: unknown): SuspensionStatus | null {
  const b = jsonOf(body);
  if (b === null) return null;
  if (b.suspended === false) return { suspended: false };
  if (b.suspended !== true) return null;
  const reason = typeof b.reason === "number" && Number.isInteger(b.reason) ? b.reason : 0;
  const reasonName = typeof b.reason_name === "string" ? b.reason_name : "other";
  const untilMs = isTimestamp(b.until_ms) ? b.until_ms : null;
  if (b.until_ms !== null && b.until_ms !== undefined && untilMs === null) return null;
  const appeal: AppealState =
    b.appeal === "sent" ? "sent" : b.appeal === "declined" ? "declined" : "none";
  return { suspended: true, details: { reason, reasonName, untilMs, appeal } };
}

/** The reason names the report sheet offers, in its catalog keys. */
const REASON_NAMES: readonly string[] = [
  "spam",
  "harassment",
  "hate",
  "violence",
  "sexual",
  "child-safety",
  "impersonation",
  "other",
];

/**
 * The catalog key of a reason name: the report sheet's own text
 * (`child-safety` -> `report.reason.child_safety`). A name this build does
 * not know (the server's `unknown`, a reason added later) reads as
 * "Something else", never as a raw identifier.
 */
export function reasonCatalogKey(reasonName: string): string {
  const name = REASON_NAMES.includes(reasonName) ? reasonName : "other";
  return `report.reason.${name.replace(/-/g, "_")}`;
}

/** Whether the screen offers `Appeal`: once per suspension. */
export function appealOffered(appeal: AppealState): boolean {
  return appeal === "none";
}

/** One button of the suspended screen. */
export type SuspendedScreenAction = "appeal" | "read" | "delete" | "sign-out";

/**
 * The suspended screen's buttons, in order (UX section 4). `appeal` is the
 * suspension's appeal state, `null` until the details are read. A device
 * that holds the account's chats offers "Read my chats"; one that holds none
 * cannot reach Settings, so "Delete account" takes that place.
 */
export function suspendedScreenActions(
  canRead: boolean,
  appeal: AppealState | null,
): SuspendedScreenAction[] {
  const actions: SuspendedScreenAction[] = [];
  if (appeal !== null && appealOffered(appeal)) actions.push("appeal");
  actions.push(canRead ? "read" : "delete");
  actions.push("sign-out");
  return actions;
}

/** The line that replaces the Appeal button, or `null` while it is offered. */
export function appealLineKey(appeal: AppealState): string | null {
  if (appeal === "sent") return "suspended.appeal.sent";
  if (appeal === "declined") return "suspended.appeal.declined";
  return null;
}

// ---------------------------------------------------------------------------
// Suspended mode
// ---------------------------------------------------------------------------

/** This device's suspended mode, or `null` outside it. */
export interface SuspensionMode {
  readonly accountIdB64: string;
  readonly username: string;
  /** This device holds the account's keys and chats, so "Read my chats"
   *  can open the app read-only. A device that has none shows the screen
   *  alone. */
  readonly canRead: boolean;
  /** The app is open read-only (after "Read my chats"). */
  readonly reading: boolean;
}

export type SuspensionEvent =
  /** A landing route answered `403 account-suspended`. */
  | {
      readonly kind: "landed";
      readonly accountIdB64: string;
      readonly username: string;
      readonly canRead: boolean;
    }
  /** The refresh route answered `403 account-suspended` (a bearer route of
   *  auth refused the session first, or the periodic check ran). */
  | {
      readonly kind: "refresh-suspended";
      readonly accountIdB64: string;
      readonly username: string;
      readonly canRead: boolean;
    }
  /** The refresh route answered a normal pair: the suspension ended. */
  | { readonly kind: "refresh-pair"; readonly accountIdB64: string }
  /** "Read my chats". */
  | { readonly kind: "read" }
  /** The banner at the top of the chat list: the screen again. */
  | { readonly kind: "show-screen" }
  /** Sign-out, account deletion, a wipe. */
  | { readonly kind: "ended" };

/** The one transition rule of suspended mode. Pure. */
export function nextSuspension(mode: SuspensionMode | null, event: SuspensionEvent): SuspensionMode | null {
  switch (event.kind) {
    case "landed":
      return {
        accountIdB64: event.accountIdB64,
        username: event.username,
        canRead: event.canRead,
        reading: false,
      };
    case "refresh-suspended":
      // Already suspended for this account: stays as it is (a person reading
      // their chats keeps reading). Otherwise the screen comes up.
      if (mode !== null && mode.accountIdB64 === event.accountIdB64) return mode;
      return {
        accountIdB64: event.accountIdB64,
        username: event.username,
        canRead: event.canRead,
        reading: false,
      };
    case "refresh-pair":
      // The suspension ended: silently back to normal.
      return mode !== null && mode.accountIdB64 === event.accountIdB64 ? null : mode;
    case "read":
      return mode !== null && mode.canRead ? { ...mode, reading: true } : mode;
    case "show-screen":
      return mode !== null ? { ...mode, reading: false } : mode;
    case "ended":
      return null;
  }
}

// ---------------------------------------------------------------------------
// The appeal
// ---------------------------------------------------------------------------

/** What sending the appeal came to. */
export type AppealOutcome =
  /** Stored; or the server already holds one (sent once per suspension). */
  | "sent"
  /** The suspension had ended: nothing to appeal. */
  | "not-suspended"
  /** No moderator list, no pinned root, or a list that does not verify. */
  | "unavailable"
  /** No answer, or any other refusal: try again. */
  | "failed";

/** The steps of an appeal, injectable. */
export interface AppealPorts {
  /** The pinned moderator root key, or `null` when this build pins none. */
  readonly rootKey: () => Uint8Array | null;
  /** The signed list, `null` for a server without reporting (503); throws
   *  when there is no answer. */
  readonly fetchList: () => Promise<Uint8Array | null>;
  /** Seal the text to the list (verifies the list under the root first);
   *  throws a `"<code>: <reason>"` error. */
  readonly seal: (text: string, list: Uint8Array, rootKey: Uint8Array) => Uint8Array;
  /** `POST /account/suspension/appeal`; resolves to the status and the
   *  `error` code of the answer, throws when there is no answer. */
  readonly post: (sealed: Uint8Array) => Promise<{ readonly status: number; readonly errorCode: string | null }>;
}

/** Whether `text` can be sent: 1 to 1,000 characters once trimmed. */
export function appealSendable(text: string): boolean {
  const n = [...text.trim()].length;
  return n >= 1 && n <= APPEAL_MAX_CHARS;
}

/** The characters of `text` as the counter shows them. */
export function appealLength(text: string): number {
  return [...text].length;
}

/** Send the appeal once. Never throws: every failure is an outcome. */
export async function sendAppeal(text: string, ports: AppealPorts): Promise<AppealOutcome> {
  if (!appealSendable(text)) return "failed";
  const rootKey = ports.rootKey();
  if (rootKey === null) return "unavailable";
  let list: Uint8Array | null;
  try {
    list = await ports.fetchList();
  } catch {
    return "failed";
  }
  if (list === null) return "unavailable";
  let sealed: Uint8Array;
  try {
    sealed = ports.seal(text, list, rootKey);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    // A list that does not verify under the pinned root is a server this
    // build cannot appeal to; anything else is a failure to retry.
    return message.startsWith("bad-signature") ? "unavailable" : "failed";
  }
  let answer: { readonly status: number; readonly errorCode: string | null };
  try {
    answer = await ports.post(sealed);
  } catch {
    return "failed";
  }
  if (answer.status === 204 || answer.status === 200) return "sent";
  if (answer.status === 409 && answer.errorCode === "appeal-already-sent") return "sent";
  if (answer.status === 409 && answer.errorCode === "not-suspended") return "not-suspended";
  return "failed";
}

/** The catalog key of an appeal failure, or `null` for no line. */
export function appealFailureKey(outcome: AppealOutcome): string | null {
  if (outcome === "unavailable") return "suspended.appeal.unavailable";
  if (outcome === "failed") return "suspended.appeal.failed";
  return null;
}
