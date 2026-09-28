// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Sign in and recover by name (ADR-0054 S5).
 *
 * Since S3 a username lookup at discovery spends an anonymous token, and a
 * token needs a session, so the pre-session auth routes (login, recovery
 * login, password reset, TOTP recover, word-vault recover, recovery-contact
 * recover) take a **login handle** in place of an account id. The handle is
 * `loginHandle(username)` from the wasm core: normalise, apply the username
 * rule, then a domain-separated BLAKE3 derive-key, 32 bytes, sent as
 * base64url without padding. The account id reaches the client only in a
 * finish response, after a successful proof. No discovery lookup runs before
 * a session.
 *
 * An unknown name meets the server's ordinary wrong-password / wrong-code
 * path (a stand-in account), so a sign-in by name never learns "no such
 * user".
 *
 * ## Claim order
 *
 * The handle (at auth) and the name (at discovery) live in two first-come
 * registries that cannot check each other. An honest client claims a name
 * in one order ({@link claimUsername}): bind the handle at auth FIRST, then
 * register at discovery. An auth `409 handle-taken` stops before discovery;
 * a discovery failure releases the handle again, so a retry is not answered
 * by our own half-finished claim.
 */
import { loadWasm } from "../wasm";
import { USERNAME_REASON, type UsernameErrorCode } from "../profile/rules";
import { base64UrlEncodeNoPad } from "./account-id";
import { AuthHttpError } from "./client";
import { DiscoveryHttpError } from "../discovery/client";

/** Length of a login handle in bytes. */
export const LOGIN_HANDLE_LEN = 32;

/** A login handle, raw and in its wire form. */
export interface LoginHandle {
  readonly bytes: Uint8Array;
  /** base64url without padding: what every name route carries. */
  readonly wire: string;
}

/** The subset of the wasm core this module needs (injectable in tests). */
export interface LoginHandleCore {
  loginHandle(username: string): Uint8Array;
  /** The canonical form of a name the rule accepts (`@Alice` -> `alice`). */
  validateUsername(username: string): string;
}

const USERNAME_CODES: ReadonlySet<string> = new Set([
  "empty",
  "tooShort",
  "tooLong",
  "invalidCharacters",
]);

/** A typed name the username rule refuses. `message` is the rule's copy. */
export class LoginHandleUsernameError extends Error {
  override name = "LoginHandleUsernameError";
  constructor(public readonly usernameError: UsernameErrorCode | "unknown") {
    super(USERNAME_REASON[usernameError]);
  }
}

/**
 * The handle of `username` through `core`. A name the rule refuses throws
 * {@link LoginHandleUsernameError} carrying the existing reason copy.
 */
export function loginHandleWith(core: LoginHandleCore, username: string): LoginHandle {
  let bytes: Uint8Array;
  try {
    bytes = core.loginHandle(username);
  } catch (err) {
    const raw =
      typeof err === "object" && err !== null
        ? (err as { usernameError?: unknown }).usernameError
        : undefined;
    const code =
      typeof raw === "string" && USERNAME_CODES.has(raw) ? (raw as UsernameErrorCode) : "unknown";
    throw new LoginHandleUsernameError(code);
  }
  if (bytes.length !== LOGIN_HANDLE_LEN) {
    throw new Error(`login handle must be ${LOGIN_HANDLE_LEN} bytes, got ${bytes.length}`);
  }
  return { bytes, wire: base64UrlEncodeNoPad(bytes) };
}

/** The handle of `username`, from the app's wasm core. */
export async function loginHandleFor(username: string): Promise<LoginHandle> {
  const wasm = await loadWasm();
  return loginHandleWith(wasm, username);
}

/** A typed name resolved for a name route: its handle and canonical form. */
export interface NameTarget {
  readonly handle: LoginHandle;
  /** The canonical username, kept for display and the device cache. */
  readonly username: string;
}

/** Resolve a typed name to its handle and canonical form, locally. Throws
 *  {@link LoginHandleUsernameError} for a name the rule refuses. */
export function nameTargetWith(core: LoginHandleCore, username: string): NameTarget {
  const handle = loginHandleWith(core, username);
  return { handle, username: core.validateUsername(username) };
}

/** {@link nameTargetWith} over the app's wasm core. */
export async function nameTargetFor(username: string): Promise<NameTarget> {
  const wasm = await loadWasm();
  return nameTargetWith(wasm, username);
}

/**
 * Finish a sign-in or recovery by name: `run` performs the name route(s)
 * and returns the session it produced; then the handle is re-bound to the
 * account (`PUT /auth/login-handle`, idempotent), which keeps a device that
 * bound before a rename in step. The re-bind is best-effort: a failure is
 * logged (never the handle, never the token) and does not undo the sign-in.
 *
 * Nothing here looks the name up anywhere: the account id is learned from
 * `run`'s finish, after the proof.
 */
export async function accessByName<T extends { readonly sessionToken: string }>(args: {
  readonly target: NameTarget;
  readonly run: (handleWire: string) => Promise<T>;
  readonly bind: (sessionToken: string, handleWire: string) => Promise<void>;
}): Promise<T> {
  const result = await args.run(args.target.handle.wire);
  try {
    await args.bind(result.sessionToken, args.target.handle.wire);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn(
      "login handle re-bind after sign-in failed",
      err instanceof AuthHttpError ? err.status : err instanceof Error ? err.name : "unknown",
    );
  }
  return result;
}

// ---------------------------------------------------------------------------
// Claim order
// ---------------------------------------------------------------------------

/** The name is held by someone else, at auth or at discovery. The message is
 *  the one `signupErrorMessage` recognises as "taken". */
export class UsernameTakenError extends Error {
  override name = "UsernameTakenError";
  constructor(public readonly username: string) {
    super(`username @${username} is already taken, pick another`);
  }
}

/** `true` when `err` is auth's `409 handle-taken`. */
export function isHandleTaken(err: unknown): boolean {
  return err instanceof AuthHttpError && err.status === 409;
}

/**
 * Claim `username` in the load-bearing order: bind the handle at auth, then
 * register the name at discovery.
 *
 * - `bind` answering auth's `409` throws {@link UsernameTakenError} and
 *   discovery is never asked.
 * - `register` failing (taken, or anything else) calls `release` (drop the
 *   handle, or re-bind the previous one on a rename) before rethrowing; a
 *   discovery `409` is rethrown as {@link UsernameTakenError}.
 *
 * Returns what `register` returned.
 */
export async function claimUsername<T>(args: {
  readonly username: string;
  readonly bind: () => Promise<void>;
  readonly register: () => Promise<T>;
  readonly release: () => Promise<void>;
}): Promise<T> {
  try {
    await args.bind();
  } catch (err) {
    if (isHandleTaken(err)) throw new UsernameTakenError(args.username);
    throw err;
  }
  try {
    return await args.register();
  } catch (err) {
    try {
      await args.release();
    } catch (releaseErr) {
      // eslint-disable-next-line no-console
      console.warn("login handle release after a failed claim failed", releaseErr);
    }
    if (err instanceof DiscoveryHttpError && err.status === 409) {
      throw new UsernameTakenError(args.username);
    }
    throw err;
  }
}
