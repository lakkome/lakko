// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Resolve which account a recover-from-words flow should target.
 *
 * Extracted from `AuthContext.recover` (G-29) as a pure, dependency-injected
 * function so the cross-device behaviour — a fresh device recovers given the
 * username + phrase — is locked by a unit test instead of living inlined in a
 * large async method where it can silently regress.
 *
 * The rule (identical on web / iOS / Android `recover`): a local
 * `(accountId, username)` cache hit wins when the typed username is empty or
 * matches it; otherwise a discovery lookup on the typed username binds the
 * account. With neither we cannot target the recover. A lookup miss collapses
 * into the same error a bad phrase yields downstream — we never confirm or
 * deny a username's existence (preserves the no-probing property).
 */

/** Minimal shape of a locally-cached `(accountId, username)` record. */
export interface CachedAccount {
  readonly accountIdB64: string;
  readonly username: string;
}

/** Minimal shape of a discovery account lookup result. */
export interface AccountLookup {
  /** Raw 16-byte account id, or `null` when the username is unregistered. */
  readonly account: Uint8Array | null;
  /** Server-normalised form of the looked-up username. */
  readonly normalisedUsername: string;
}

/** The resolved account the recover targets. */
export interface ResolvedAccount {
  readonly accountIdB64: string;
  readonly resolvedUsername: string;
}

/**
 * Resolve the account to recover.
 *
 * @throws Error `"invalid recovery"` when the typed username is unregistered
 *   (collapsed with the downstream bad-phrase rejection — no username probing).
 * @throws Error a fresh-device prompt when neither a cached account nor a typed
 *   username is available.
 */
export async function resolveRecoverAccount(args: {
  readonly cached: CachedAccount | null;
  readonly username: string | undefined;
  readonly lookup: (username: string) => Promise<AccountLookup>;
  readonly encodeAccountId: (bytes: Uint8Array) => string;
}): Promise<ResolvedAccount> {
  const trimmedUsername = args.username?.trim() ?? "";

  // A local cache hit wins when the typed username is empty or matches it.
  if (
    args.cached !== null &&
    (trimmedUsername.length === 0 || args.cached.username === trimmedUsername)
  ) {
    return {
      accountIdB64: args.cached.accountIdB64,
      resolvedUsername: args.cached.username,
    };
  }

  // Otherwise resolve the typed username via discovery so a FRESH device can
  // recover given username + phrase.
  if (trimmedUsername.length > 0) {
    const result = await args.lookup(trimmedUsername);
    if (result.account === null) {
      // Collapsed with "bad phrase" downstream by the OPAQUE rejection — we
      // don't help probe the username space.
      throw new Error("invalid recovery");
    }
    return {
      accountIdB64: args.encodeAccountId(result.account),
      resolvedUsername: result.normalisedUsername,
    };
  }

  throw new Error(
    "no account on this device — enter your username so we can " +
      "look up the account to recover",
  );
}
