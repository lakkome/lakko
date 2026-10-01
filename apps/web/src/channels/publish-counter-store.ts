// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Per-(account, channel) persistence for the monotone publish counter
 * (`message_counter`) — the value that keeps the channel-post AEAD
 * `(key, nonce)` unique.
 *
 * ## Why this must survive a reload
 *
 * `lakko_channels::post::compose_post` HKDF-derives the AEAD key +
 * nonce from `(seed, channel_id, epoch_id, sender_pubkey,
 * message_counter)` and REQUIRES `message_counter` to be strictly
 * monotone within `(channel_id, epoch_id, sender_pubkey)` — the crate
 * cannot enforce it because the counter lives in client storage
 * between sessions. The channel content `seed` is stable across a page
 * reload (it re-derives from the persisted PublisherGroup MLS exporter)
 * and so is the sender key (its signing seed is persisted in the
 * encrypted channel log), so a counter that restarted at 0 after a
 * reload would seal a DIFFERENT plaintext under the SAME `(key, nonce)`
 * — ChaCha20 keystream reuse. Persisting it at rest closes that.
 *
 * Because `epoch_id` is folded into the HKDF, a forever-monotone
 * per-channel counter is safe — it never needs to reset per epoch.
 *
 * ## Lifetime == the publisher signing key's lifetime
 *
 * `sender_pubkey` is in the KDF too, and the publisher signing key is
 * random per device and channel (`ChannelSigningKey.generate`), stored
 * only in this account's encrypted channel log, never derived from the
 * recovery phrase and never carried in a backup, a pairing bundle or a
 * DeviceGroup sync. So the one dangerous shape is the counter rewinding
 * while the SAME signing key is still stored. The counter therefore
 * lives exactly as long as the key that needs it:
 *
 * - it is account data, one row per (account, channel):
 *   `lakko.channels.publish-counter.v2.<accountIdB64>.<channelIdHex>`;
 * - Log out and an un-follow keep it (the signing seed may still be
 *   stored, so dropping the floor could rewind it under a live key);
 * - Forget device and Delete account remove it
 *   ({@link clearPublishCountersForAccount}), and only after the channel
 *   log that holds the signing seeds is gone (`wipeChannelsForAccount`):
 *   the next post then signs with a NEW sender key, so a counter
 *   restarting from 0 cannot collide.
 *
 * The retired v1 row (`lakko.channels.publish-counter.v1.<channelIdHex>`)
 * was device-wide, so a second account signed in on the same browser
 * started from the first account's floor and Forget device left it
 * behind. It is never read; Forget device drops it.
 *
 * Which account a call means: the trailing `owner` argument, otherwise
 * the signed-in account bound through `bindContactsAccount`
 * (`contacts/local-store.ts`), the binding `rotation-state-store.ts`
 * uses. With no account a read is `null`, a save is a no-op, and
 * {@link issueAndAdvance} refuses (a counter that cannot be persisted is
 * never handed out).
 *
 * ## Why plain `localStorage` (not device-key-encrypted IDB)
 *
 * The counter is NOT key material — it is carried in the clear inside
 * every post's on-wire header (`message_counter`), so confidentiality
 * buys nothing. This is the class the sibling `rotation-state-store.ts`
 * documents (non-secret operational metadata; invariant #7 governs key
 * material only). The threat this store targets is a benign relaunch,
 * not a same-origin write attacker.
 *
 * The counter is a `bigint` (`u64`); `JSON` cannot serialise a
 * `bigint`, so it is stored as a plain decimal string and re-parsed on
 * load.
 */

import { boundContactsAccount } from "../contacts/local-store";

/** One row per (account, channel); versioned so a future shape change
 *  can migrate rather than silently mis-parse. */
const KEY_PREFIX = "lakko.channels.publish-counter.v2.";
/** The retired device-wide rows (`<prefix><channelIdHex>`). Never read;
 *  dropped by Forget device. */
const RETIRED_PREFIX = "lakko.channels.publish-counter.v1.";

function accountOf(owner: string | undefined): string | null {
  const account = owner ?? boundContactsAccount();
  return account === null || account.length === 0 ? null : account;
}

function rowPrefix(account: string): string {
  return `${KEY_PREFIX}${account}.`;
}

function parseCounter(raw: string | null): bigint | null {
  if (raw === null) return null;
  // `BigInt("")` and `BigInt(" ")` are `0n`, which would silently rewind a
  // real counter — only a well-formed non-negative decimal is accepted.
  if (!/^\d+$/.test(raw)) return null;
  try {
    return BigInt(raw);
  } catch {
    return null;
  }
}

/**
 * Load the persisted publish counter for a channel of the account, or
 * `null` when absent / corrupt / storage-unavailable / no account. A
 * corrupt entry is treated as absent so the caller falls to its
 * `load-else-0` default rather than throwing.
 */
export function loadCounter(channelIdHex: string, owner?: string): bigint | null {
  const account = accountOf(owner);
  if (account === null || typeof localStorage === "undefined") return null;
  try {
    return parseCounter(localStorage.getItem(rowPrefix(account) + channelIdHex));
  } catch {
    return null;
  }
}

/**
 * Persist the publish counter for a channel of the account. Best-effort
 * — a storage failure (quota, private-mode) is swallowed rather than
 * crashing the publish path; a no-op with no account.
 *
 * Monotonicity is the caller's contract: the only writer is
 * {@link issueAndAdvance}, which always writes `previous + 1` where
 * `previous` is seeded from this store, so the persisted value only
 * ever climbs.
 */
export function saveCounter(channelIdHex: string, counter: bigint, owner?: string): void {
  const account = accountOf(owner);
  if (account === null || typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(rowPrefix(account) + channelIdHex, counter.toString());
  } catch {
    // ignore — see the doc comment above.
  }
}

/**
 * Load-else-0: the counter to seed the in-memory map with on `setKeys`.
 * Prefer the live in-memory value (already `>=` the persisted floor);
 * fall back to the account's persisted floor across a reload; else 0
 * for a genuinely new channel. Never lowers a value.
 */
export function seedCounter(
  inMemory: bigint | undefined,
  channelIdHex: string,
  owner?: string,
): bigint {
  return inMemory ?? loadCounter(channelIdHex, owner) ?? 0n;
}

/**
 * Issue the counter for the NEXT post and advance the account's floor
 * with write-through. Returns `value` (the pre-increment counter the
 * caller composes the post with) after persisting `value + 1`.
 *
 * The persist happens BEFORE this returns, so a crash / reload right
 * after a post can never re-issue `value` — burning a value is safe,
 * re-issuing one re-uses the AEAD nonce. Throws when there is no
 * account to persist under: a value that would not survive a reload is
 * never handed out.
 */
export function issueAndAdvance(
  inMemory: bigint | undefined,
  channelIdHex: string,
  owner?: string,
): { readonly value: bigint; readonly advanced: bigint } {
  const account = accountOf(owner);
  if (account === null) {
    throw new Error("publish counter: no signed-in account to persist under");
  }
  const value = inMemory ?? loadCounter(channelIdHex, account) ?? 0n;
  const advanced = value + 1n;
  saveCounter(channelIdHex, advanced, account);
  return { value, advanced };
}

/**
 * Load every persisted counter of the account into a fresh map, keyed by
 * channel id. Used when the account's channels session starts so the
 * first publish after a reload resumes the floor. Empty with no account.
 */
export function loadAllCounters(owner?: string): Map<string, bigint> {
  const out = new Map<string, bigint>();
  const account = accountOf(owner);
  if (account === null || typeof localStorage === "undefined") return out;
  try {
    const prefix = rowPrefix(account);
    for (let i = 0; i < localStorage.length; i += 1) {
      const key = localStorage.key(i);
      if (key === null || !key.startsWith(prefix)) continue;
      const value = parseCounter(localStorage.getItem(key));
      if (value !== null) out.set(key.slice(prefix.length), value);
    }
  } catch {
    // storage unavailable: nothing to load
  }
  return out;
}

/**
 * Forget device / Delete account: drop every counter of `accountIdB64`
 * and the retired device-wide rows. Another account's counters are left
 * alone. Safe ONLY once the account's publisher signing seeds are gone
 * too (`wipeChannelsForAccount` calls it after deleting the channel
 * log) — never on Log out or an un-follow.
 */
export function clearPublishCountersForAccount(accountIdB64: string): void {
  try {
    if (typeof localStorage === "undefined") return;
    const own = rowPrefix(accountIdB64);
    // Collect first — removing during a `length`/`key(i)` walk shifts
    // indices and would skip entries.
    const doomed: string[] = [];
    for (let i = 0; i < localStorage.length; i += 1) {
      const key = localStorage.key(i);
      if (key !== null && (key.startsWith(own) || key.startsWith(RETIRED_PREFIX))) doomed.push(key);
    }
    for (const key of doomed) localStorage.removeItem(key);
  } catch {
    // storage unavailable: nothing to clear
  }
}
