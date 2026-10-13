// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A session names a device (ADR-0074 section 0.4, sprint T2).
 *
 * The `v3` access token and the `r2` refresh credential carry a device id:
 * this device's conversation leaf signature key, the same 32 bytes that key
 * its row in discovery's directory. A session that names no device cannot
 * publish a KeyPackage and cannot be revoked per device, so the client
 * asserts the id wherever it can:
 *
 *   - at a landing, when the device identity of the account being signed in
 *     to is already known on this browser ({@link landingDeviceId});
 *   - afterwards, through `POST /auth/session/bind-device`, when the session
 *     landed before the device identity existed (a first sign-in on a fresh
 *     browser, a signup) ({@link bindDecision}, {@link bindRefusal}).
 *
 * The decisions are pure functions so they can be tested without a server.
 * The one piece of state is the device id this browser last installed for
 * an account, kept in plain storage: it is a public value (the directory
 * publishes it) and it is what lets a landing assert the id before any
 * device-key-wrapped store has been opened.
 */
import type { MarkerStorage } from "./phrase-gate";

/** Bytes of a device id. */
export const DEVICE_ID_LEN = 32;

/** URL-safe base64 without padding: the wire form of `device_id_b64`. */
export function deviceIdB64Url(deviceId: Uint8Array): string {
  if (deviceId.length !== DEVICE_ID_LEN) {
    throw new Error(`device id must be ${DEVICE_ID_LEN} bytes`);
  }
  let binary = "";
  for (const byte of deviceId) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** The id a session that names no device carries: 32 zero bytes. */
export const UNBOUND_DEVICE_ID_B64URL = "A".repeat(43);

/** Whether `value` has the shape of a device id on the wire. */
export function isDeviceIdB64Url(value: string): boolean {
  return /^[A-Za-z0-9_-]{43}$/.test(value);
}

// ---------------------------------------------------------------------------
// The id this browser holds for an account
// ---------------------------------------------------------------------------

function knownKey(accountIdB64: string): string {
  return `lakko:device-id:${accountIdB64}`;
}

/** Record the device id installed for `accountIdB64` on this browser. */
export function writeKnownDeviceId(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  deviceIdB64: string,
): void {
  try {
    storage?.setItem(knownKey(accountIdB64), deviceIdB64);
  } catch {
    // Storage blocked: the next landing binds afterwards instead.
  }
}

/** The recorded id, or `null` (never recorded, unreadable, or malformed). */
export function readKnownDeviceId(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
): string | null {
  try {
    const value = storage?.getItem(knownKey(accountIdB64)) ?? null;
    return value !== null && isDeviceIdB64Url(value) && value !== UNBOUND_DEVICE_ID_B64URL
      ? value
      : null;
  } catch {
    return null;
  }
}

/** Forget the id (the account's local data was wiped: the device seed is
 *  gone, and the next setup mints a new one). */
export function clearKnownDeviceId(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(knownKey(accountIdB64));
  } catch {
    // nothing to clear
  }
}

// ---------------------------------------------------------------------------
// What a landing sends
// ---------------------------------------------------------------------------

/** The account this browser remembers (the plain `(account, username)`
 *  marker), or `null`. */
export interface CachedAccountView {
  readonly accountIdB64: string;
  readonly username: string;
}

/** What the landing knows about the account it is signing in to. */
export interface LandingTarget {
  /** The account id, when the flow already has it (a proof sign-in of the
   *  live session, a passkey sign-in with an allow-list). */
  readonly accountIdB64?: string | undefined;
  /** The name the person typed, when the flow signs in by name. */
  readonly username?: string | undefined;
}

function sameName(a: string, b: string): boolean {
  const x = a.trim().toLowerCase();
  return x.length > 0 && x === b.trim().toLowerCase();
}

/**
 * The `device_id_b64` a landing asserts, or `undefined` to send none (the
 * session lands unbound and is bound afterwards).
 *
 * An id is sent only when this browser knows the device identity of the
 * account being signed in to: the target names the cached account (by id,
 * or by the cached username when the flow signs in by name) and an id was
 * recorded for it. Anything else sends nothing: asserting the id of one
 * account's device identity for another account's session would bind that
 * session to a key this browser will not use for it.
 */
export function landingDeviceId(input: {
  readonly cached: CachedAccountView | null;
  readonly target: LandingTarget;
  /** The id recorded for an account on this browser, or `null`. */
  readonly knownDeviceId: (accountIdB64: string) => string | null;
}): string | undefined {
  const { cached, target } = input;
  if (target.accountIdB64 !== undefined) {
    // The id decides; a cached marker for another account says nothing.
    return input.knownDeviceId(target.accountIdB64) ?? undefined;
  }
  if (cached === null || target.username === undefined) return undefined;
  if (!sameName(cached.username, target.username)) return undefined;
  return input.knownDeviceId(cached.accountIdB64) ?? undefined;
}

// ---------------------------------------------------------------------------
// Binding afterwards
// ---------------------------------------------------------------------------

/**
 * The device segment of a `v3.` access token or an `r2.` refresh credential
 * (`<ver>.<account>.<generation>.<device>.<issued>.<expires>.<tag>`), or
 * `null` when the string has no such shape.
 *
 * A HINT, never trusted: it only saves a `bind-device` call for a session
 * that already names this device. The server decides what a credential
 * names; a wrong hint costs one request, or is corrected by the
 * `device-mismatch` answer of the KeyPackage publish.
 */
export function tokenDeviceSegment(token: string): string | null {
  const parts = token.split(".");
  if (parts.length !== 7) return null;
  if (parts[0] !== "v3" && parts[0] !== "r2") return null;
  const device = parts[3];
  return device !== undefined && isDeviceIdB64Url(device) ? device : null;
}

/** What to do about the held session and this device's id. */
export type BindDecision =
  /** The session already names this device. */
  | "bound"
  /** Call `POST /auth/session/bind-device`. */
  | "bind";

/**
 * Whether the held access token needs binding to `ownDeviceIdB64`. `force`
 * skips the hint (discovery answered `device-mismatch`, so the hint was
 * wrong or the token is of a shape this client cannot read).
 */
export function bindDecision(input: {
  readonly sessionToken: string;
  readonly ownDeviceIdB64: string;
  readonly force?: boolean;
}): BindDecision {
  if (input.force === true) return "bind";
  return tokenDeviceSegment(input.sessionToken) === input.ownDeviceIdB64 ? "bound" : "bind";
}

/** What a refused `bind-device` means. */
export type BindRefusal =
  /** `401 device-revoked`: this device was removed. Sign out and wipe. */
  | "device-revoked"
  /** `401 key-generation-superseded`: the keys were reset elsewhere. */
  | "superseded"
  /** `409 device-already-bound`: the session names another device id than
   *  the one this browser now holds. Only a new sign-in can name this one. */
  | "sign-in-again"
  /** `401 refresh-invalid`, or any other `401`: the session ended. */
  | "session-ended"
  /** Anything else (a `400`, a `5xx`, a limit): leave the session as it is
   *  and try again at the next landing. */
  | "later";

export function bindRefusal(status: number, errorCode: string | null): BindRefusal {
  if (status === 401) {
    if (errorCode === "device-revoked") return "device-revoked";
    if (errorCode === "key-generation-superseded") return "superseded";
    return "session-ended";
  }
  if (status === 409 && errorCode === "device-already-bound") return "sign-in-again";
  return "later";
}

// ---------------------------------------------------------------------------
// The recorded identity key (ADR-0074 section 0.1)
// ---------------------------------------------------------------------------

function genesisKey(accountIdB64: string): string {
  return `lakko:genesis-recorded:${accountIdB64}`;
}

/**
 * The account key (standard base64) `POST /auth/genesis/attest` last
 * answered for on this browser, or `null`. Auth records the key on the
 * first call and attests only that key afterwards; without a recorded key
 * no device can be removed, so the client calls the mint on every landing
 * until it has succeeded once for the key it holds.
 */
export function readGenesisRecorded(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
): string | null {
  try {
    return storage?.getItem(genesisKey(accountIdB64)) ?? null;
  } catch {
    return null;
  }
}

export function writeGenesisRecorded(
  storage: MarkerStorage | undefined,
  accountIdB64: string,
  accountKeyB64: string,
): void {
  try {
    storage?.setItem(genesisKey(accountIdB64), accountKeyB64);
  } catch {
    // Storage blocked: the mint is repeated at the next landing (idempotent).
  }
}

export function clearGenesisRecorded(storage: MarkerStorage | undefined, accountIdB64: string): void {
  try {
    storage?.removeItem(genesisKey(accountIdB64));
  } catch {
    // nothing to clear
  }
}

/**
 * Whether this landing calls the genesis mint: always, until it succeeded
 * once FOR THE KEY THIS DEVICE HOLDS. The mark is keyed on the key, so a
 * key reset (a new account key) makes the next landing mint again.
 */
export function genesisMintNeeded(recordedKeyB64: string | null, ownAccountKeyB64: string): boolean {
  return recordedKeyB64 !== ownAccountKeyB64;
}

/** What one mint attempt means for the mark. */
export type GenesisMintOutcome =
  /** Auth attested this key: it is the recorded one. Write the mark. */
  | "recorded"
  /** `409 identity-key-mismatch`: auth holds ANOTHER key for the account.
   *  Nothing is marked; the landing rule (the directory) decides whether
   *  this device's keys are stale. */
  | "mismatch"
  /** Anything else: try again at the next landing. */
  | "retry";

export function genesisMintOutcome(result: {
  readonly ok: boolean;
  readonly status?: number;
  readonly errorCode?: string | null;
}): GenesisMintOutcome {
  if (result.ok) return "recorded";
  if (result.status === 409 && result.errorCode === "identity-key-mismatch") return "mismatch";
  return "retry";
}
