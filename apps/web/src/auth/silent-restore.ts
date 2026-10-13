// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a page load does with the secrets this browser already holds.
 *
 * ## The defect this replaces
 *
 * A reload used to land in `session` whenever a device key was present, but
 * with the in-memory identity secrets and the canonical MLS identity both
 * `null` — only an explicit signup / login / recover filled them. Chat and
 * Contacts then rendered a "re-enter your password" panel that linked to
 * `/login`, and `/login` sits behind `RequireAnonymous`, which bounces a
 * `session` user straight back home. The only way out was Log out.
 *
 * Everything needed to rebuild the session without a password is already
 * persisted under the non-extractable device key: the session pair, the
 * ADR-0017 AccountIdentity (phrase) seed and the MLS snapshot. The ADR-0018
 * warm recovery landing already rebuilds a session from exactly those, and
 * the service worker already unwraps the seed without a password for the
 * ADR-0041 background decrypt. So a reload restores the same way.
 *
 * ## The rule
 *
 * Restore only when EVERY piece is present; otherwise land in `anon` (with the
 * cached username when there is one, so `/login` pre-fills it). There is no
 * third, half-signed-in outcome: a `session` state without secrets is exactly
 * the loop being removed.
 *
 * ADR-0073: the piece that must be live is the REFRESH credential, not the
 * access token. An access token is fifteen minutes long and is expected to be
 * dead on most reloads; the restore refreshes with the credential first
 * (`AuthContext.restoreSessionOnLoad`), and offline it opens on local data
 * with whatever the access token still allows.
 *
 * ## The trade-off, stated plainly
 *
 * The at-rest barrier on a reload becomes "the device key is present in this
 * browser profile" rather than "the user typed a password". That is the
 * barrier `export-key-store.ts` already accepted for passkey sign-in (a
 * device-key compromise already yields the cached export key), and the one
 * the service worker's background decrypt already relies on for the seed.
 * `logout()` still ends it — it deletes the persisted pair, and no credential
 * means no restore — and `forgetDevice()` wipes every wrapped store.
 */

import type { SessionPair } from "./session-pair";

/** Everything the probe read from storage. Pure data, no handles. */
export interface StoredSecretsProbe {
  /** The plaintext `(accountId, username)` marker from localStorage. */
  readonly cachedAccount: {
    readonly accountIdB64: string;
    readonly username: string;
  } | null;
  /** A non-extractable device root key exists for the cached account. */
  readonly deviceKeyPresent: boolean;
  /** The persisted session pair, or `null` when absent, undecryptable or of
   *  a shape this build does not read (the store already drops a row whose
   *  refresh credential expired). */
  readonly sessionPair: SessionPair | null;
  /** The AccountIdentity seed decrypted under the device key. `false` when
   *  absent or undecryptable — the two are handled identically. */
  readonly identitySeedPresent: boolean;
  /** ADR-0074 section 2.3: a logout on this device has not reached the
   *  server yet (`logout-owed.ts`). The stored pair is then only what the
   *  retry signs out with, never a session to restore. Absent = `false`. */
  readonly logoutOwed?: boolean;
  readonly nowMs: number;
}

export type SilentRestoreDecision =
  | { readonly kind: "anon" }
  | { readonly kind: "anon-cached"; readonly username: string }
  | {
      readonly kind: "restore";
      readonly accountIdB64: string;
      readonly username: string;
      readonly sessionPair: SessionPair;
    };

export function decideSilentRestore(
  probe: StoredSecretsProbe,
): SilentRestoreDecision {
  const cached = probe.cachedAccount;
  if (cached === null) return { kind: "anon" };
  const fallback: SilentRestoreDecision = {
    kind: "anon-cached",
    username: cached.username,
  };
  if (!probe.deviceKeyPresent) return fallback;
  // The person logged out; only the server has not heard yet.
  if (probe.logoutOwed === true) return fallback;
  const pair = probe.sessionPair;
  // No pair: either a logout deleted it (logout intent must survive a
  // reload) or the credential expired. Both mean a password (or passkey)
  // is required.
  if (pair === null) return fallback;
  // The store already evicts a row whose credential expired; re-checked
  // here so a caller that hands in a raw row still cannot restore on a dead
  // credential. The ACCESS token may be expired: the restore refreshes.
  if (pair.refreshExpiresAtMs <= probe.nowMs) return fallback;
  // ADR-0017: without the seed the canonical identity cannot be rebuilt, and
  // it must never be re-derived from anything else. The explicit login path
  // owns the `needs-recovery-phrase` fail-safe; a reload does not start it.
  if (!probe.identitySeedPresent) return fallback;
  return {
    kind: "restore",
    accountIdB64: cached.accountIdB64,
    username: cached.username,
    sessionPair: pair,
  };
}
