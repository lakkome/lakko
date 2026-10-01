// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which name a recover-from-words flow targets.
 *
 * Since ADR-0054 S5 the recovery routes are addressed by the login handle of
 * a NAME, never by an account id, and nothing is looked up before a session:
 * the account id comes back in the finish. So the only question left is
 * which name: the typed one, else the account cached on this device (the
 * form lets a returning user leave the field empty). A cached guest account
 * has no name, and neither does an empty field on a fresh device: then there
 * is nothing to address, and the form asks for the username.
 *
 * Pure, so the rule is pinned by a unit test (G-29).
 */

import { KeyedError } from "../l10n/keyed-error";

/** Minimal shape of a locally-cached `(accountId, username)` record. */
export interface CachedAccount {
  readonly accountIdB64: string;
  readonly username: string;
}

/**
 * The name to recover.
 *
 * @throws Error asking for the username when neither a typed name nor a
 *   cached, named account is available.
 */
export function resolveRecoverUsername(args: {
  readonly cached: CachedAccount | null;
  readonly username: string | undefined;
}): string {
  const typed = args.username?.trim() ?? "";
  if (typed.length > 0) return typed;
  const cached = args.cached?.username.trim() ?? "";
  if (cached.length > 0) return cached;
  throw new KeyedError("recovery.username_required_to_recover");
}
