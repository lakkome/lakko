// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The link secrets of this account's own `/i/` links, remembered on the
 * device that minted them.
 *
 * Since the sealed-link amendment of ADR-0028 (2026-09-28) a link is
 * `/<k>/<id>#<secret>` and the ledger never sees the secret. The owner's list
 * of active links comes from the ledger (`listInvitesByOwner`), so without a
 * local record a listed link could only be shown without its fragment, which
 * opens nothing. This store maps `(account, ledger id)` to the secret at mint
 * time; a row whose secret this device does not hold (minted on another
 * device, or before the storage was cleared) is shown revocable but not
 * copyable.
 *
 * `localStorage`, wrapped: a read or write that throws (private mode, blocked
 * storage) degrades to "not known here", never to a broken list. The secret is
 * an invite capability, not key material; it is never logged.
 */

const PREFIX = "lakko/invite-link-secrets/v1/";

/** The storage this module reads and writes; injectable for the host tests. */
export interface SecretStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem?(key: string): void;
}

function defaultStorage(): SecretStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

function read(storage: SecretStorage | null, accountKey: string): Record<string, string> {
  if (storage === null) return {};
  try {
    const raw = storage.getItem(PREFIX + accountKey);
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: Record<string, string> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof v === "string") out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

/** Remember the fragment (16 base64url characters) of a link this device
 *  minted under the ledger id `blobIdSlug`. Best effort. */
export function rememberInviteLinkSecret(
  accountKey: string,
  blobIdSlug: string,
  fragment: string,
  storage: SecretStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  const map = read(storage, accountKey);
  map[blobIdSlug] = fragment;
  try {
    storage.setItem(PREFIX + accountKey, JSON.stringify(map));
  } catch {
    /* storage full or blocked: the link is still returned to the caller */
  }
}

/** The fragment of a link this device minted, or `null`. */
export function inviteLinkSecretFor(
  accountKey: string,
  blobIdSlug: string,
  storage: SecretStorage | null = defaultStorage(),
): string | null {
  return read(storage, accountKey)[blobIdSlug] ?? null;
}

/** Forget the fragment of one link (it was revoked). Best effort. */
export function forgetInviteLinkSecret(
  accountKey: string,
  blobIdSlug: string,
  storage: SecretStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  const map = read(storage, accountKey);
  if (!(blobIdSlug in map)) return;
  delete map[blobIdSlug];
  try {
    storage.setItem(PREFIX + accountKey, JSON.stringify(map));
  } catch {
    /* storage blocked: the ledger row is gone either way */
  }
}

/** Drop every link secret this device keeps for the account (Forget device,
 *  Delete account). Best effort. */
export function clearInviteLinkSecrets(
  accountKey: string,
  storage: SecretStorage | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    if (storage.removeItem !== undefined) storage.removeItem(PREFIX + accountKey);
    else storage.setItem(PREFIX + accountKey, "{}");
  } catch {
    /* storage blocked: nothing was kept either */
  }
}
