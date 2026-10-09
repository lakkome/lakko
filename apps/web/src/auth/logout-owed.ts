// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Log out at the server too (ADR-0066 section 2.1 / 2.3; UX 18.2, "Log
 * out").
 *
 * `POST /auth/session/logout` raises a floor for this device at auth, so a
 * copy of the credentials this device held cannot keep using the account.
 * The local sign-out never waits for it: offline, the device logs out
 * locally, keeps a small marker, and tells the server once more the next
 * time the app opens. Nothing is shown for any of this.
 *
 * What the marker is: one plain flag per account. The credentials the retry
 * needs stay where they already were, in the device-key-wrapped session
 * store; while the flag is set that row is NOT a session to restore
 * (`decideSilentRestore` reads the flag), it is only what the retry signs
 * out with, and it is deleted after the one retry whatever its outcome.
 */
import type { MarkerStorage } from "./phrase-gate";
import type { SessionPair } from "./session-pair";

function markerKey(accountIdB64: string): string {
  return `lakko:logout-owed:${accountIdB64}`;
}

export function markLogoutOwed(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.setItem(markerKey(accountIdB64), "1");
  } catch {
    // Storage blocked: the logout is tried once, now, and not again.
  }
}

export function isLogoutOwed(storage: MarkerStorage | undefined, accountIdB64: string): boolean {
  try {
    return storage?.getItem(markerKey(accountIdB64)) === "1";
  } catch {
    return false;
  }
}

export function clearLogoutOwed(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(markerKey(accountIdB64));
  } catch {
    // nothing to clear
  }
}

/** How the logout request gets a bearer the server still accepts. */
export type LogoutPlan =
  /** The access token is still valid: send it. */
  | "use-token"
  /** The access token expired: refresh once, then log out with the new one. */
  | "refresh-first"
  /** Nothing usable is held (no pair, or the refresh credential expired):
   *  every credential this device held is already dead. */
  | "nothing";

export function logoutPlan(
  pair: Pick<SessionPair, "sessionExpiresAtMs" | "refreshExpiresAtMs"> | null,
  nowMs: number,
): LogoutPlan {
  if (pair === null) return "nothing";
  if (pair.sessionExpiresAtMs > nowMs) return "use-token";
  return pair.refreshExpiresAtMs > nowMs ? "refresh-first" : "nothing";
}

/** What one request of the logout (the refresh, or the logout itself) came
 *  to. `status` is `null` when no answer arrived. */
export type LogoutAttempt =
  /** The server holds the floor, or the credential was already refused
   *  (any `401`: expired, logged out, removed, superseded): nothing is left
   *  to sign out. */
  | "done"
  /** No answer, or an answer that is not a verdict (`5xx`, a limit): the
   *  logout is still owed. */
  | "owed";

export function logoutAttempt(status: number | null): LogoutAttempt {
  if (status === null) return "owed";
  if (status === 204 || status === 200 || status === 401) return "done";
  return "owed";
}

/** The ports of {@link runServerLogout}. Each rejects with `{ status }`
 *  semantics folded by the caller: resolve with the HTTP status, or `null`
 *  when no answer arrived. */
export interface ServerLogoutPorts {
  /** `POST /auth/session/refresh`; the new access token, or the refusal's
   *  status, or `null` for no answer. */
  readonly refresh: (refreshToken: string) => Promise<{ token: string } | { status: number | null }>;
  /** `POST /auth/session/logout`; the status, or `null` for no answer. */
  readonly logout: (sessionToken: string) => Promise<number | null>;
  readonly now: () => number;
}

/** Sign `pair` out at the server. Never throws. */
export async function runServerLogout(
  pair: SessionPair | null,
  ports: ServerLogoutPorts,
): Promise<LogoutAttempt> {
  const plan = logoutPlan(pair, ports.now());
  if (plan === "nothing" || pair === null) return "done";
  let token = pair.sessionToken;
  if (plan === "refresh-first") {
    const refreshed = await ports.refresh(pair.refreshToken);
    if (!("token" in refreshed)) return logoutAttempt(refreshed.status);
    token = refreshed.token;
  }
  return logoutAttempt(await ports.logout(token));
}
