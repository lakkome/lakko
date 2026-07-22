// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Per-channel persistence for the monotone publish counter
 * (`message_counter`) — the value that keeps the channel-post AEAD
 * `(key, nonce)` unique.
 *
 * ## Why this must survive a reload (the bug this closes)
 *
 * `lakko_channels::post::compose_post` HKDF-derives the AEAD key +
 * nonce from `(seed, channel_id, epoch_id, sender_pubkey,
 * message_counter)` and REQUIRES `message_counter` to be strictly
 * monotone within `(channel_id, epoch_id, sender_pubkey)` — the crate
 * cannot enforce it because "the counter lives in client storage
 * between sessions". The channel content `seed` is STABLE across a
 * page reload (it re-derives from the persisted PublisherGroup MLS
 * exporter), so if the counter restarts at 0 after a reload the SAME
 * `(key, nonce)` seals a DIFFERENT plaintext → ChaCha20 keystream
 * reuse (catastrophic). Before this store the web counter was
 * in-memory only, so every reload rewound it. Persisting it at rest
 * makes it survive the reload.
 *
 * Because `epoch_id` is folded into the HKDF, a FOREVER-monotone
 * per-channel counter is safe — it never needs to reset per epoch.
 * So this store keeps one durable, only-ever-increasing value per
 * channel.
 *
 * ## Lifetime == the PublisherGroup seed's lifetime
 *
 * The floor is safe to drop ONLY when the seed source (the persistent
 * OpenMLS PublisherGroup) is destroyed — i.e. a full device wipe
 * (`forgetDevice`). An un-follow or a soft `logout()` leaves the
 * PublisherGroup in the OpenMLS IndexedDB provider intact, so
 * re-opening the same channel id re-derives the SAME seed; dropping
 * the counter in those cases would rewind it to 0 under a live seed
 * and re-open the reuse hole. Callers therefore keep the persisted
 * counter across un-follow / re-follow and only `clearAllCounters()`
 * on a full wipe.
 *
 * ## Why plain `localStorage` (not device-key-encrypted IDB)
 *
 * The counter is NOT key material — it is carried in the clear inside
 * every post's on-wire header (`message_counter`), so confidentiality
 * buys nothing. This is exactly the class the sibling
 * `rotation-state-store.ts` documents (non-secret operational
 * metadata; invariant #7 governs key material only), and it lives
 * next to the channel list + the subscriber-key store already in
 * `localStorage`. The threat this fix targets is a benign relaunch,
 * not a same-origin write attacker — and such an attacker already has
 * a bigger win available (the subscriber X25519 secret half sits in
 * plaintext `localStorage` right beside it), so device-key-encrypting
 * only the counter would be inconsistent and provide no marginal
 * defence.
 *
 * The counter is a `bigint` (`u64`); `JSON` cannot serialise a
 * `bigint`, so it is stored as a plain decimal string and re-parsed
 * on load.
 */

/** One key per channel; versioned so a future shape change can
 *  migrate rather than silently mis-parse. */
const KEY_PREFIX = "lakko.channels.publish-counter.v1.";

/**
 * Load the persisted publish counter for a channel, or `null` when
 * absent / corrupt / storage-unavailable. A corrupt entry is treated
 * as absent so the caller falls to its `load-else-0` default rather
 * than throwing.
 *
 * The `^\d+$` gate rejects an empty / whitespace / signed / junk value
 * (`BigInt("")` and `BigInt(" ")` are `0n`, which would silently
 * rewind a real counter) — only a well-formed non-negative decimal is
 * accepted.
 */
export function loadCounter(channelIdHex: string): bigint | null {
  if (typeof localStorage === "undefined") return null;
  const raw = localStorage.getItem(KEY_PREFIX + channelIdHex);
  if (raw === null) return null;
  if (!/^\d+$/.test(raw)) return null;
  try {
    return BigInt(raw);
  } catch {
    return null;
  }
}

/**
 * Persist the publish counter for a channel. Best-effort — a storage
 * failure (quota, private-mode) is swallowed rather than crashing the
 * publish path.
 *
 * Monotonicity is the caller's contract: the only writer is
 * `nextCounter`, which always writes `previous + 1` where `previous`
 * is seeded from this store, so the persisted value only ever climbs.
 */
export function saveCounter(channelIdHex: string, counter: bigint): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(KEY_PREFIX + channelIdHex, counter.toString());
  } catch {
    // ignore — see the doc comment above.
  }
}

/**
 * Load-else-0: the counter to seed the in-memory map with on
 * `setKeys`. Prefer the live in-memory value (already `>=` the
 * persisted floor); fall back to the persisted floor across a reload;
 * else 0 for a genuinely new channel. Never lowers a value, so
 * re-keying an existing channel cannot rewind the counter under a
 * still-live seed. Pure given the `localStorage` backend.
 */
export function seedCounter(
  inMemory: bigint | undefined,
  channelIdHex: string,
): bigint {
  return inMemory ?? loadCounter(channelIdHex) ?? 0n;
}

/**
 * Issue the counter for the NEXT post and advance the floor with
 * write-through. Returns `value` (the pre-increment counter the caller
 * composes the post with) after persisting `value + 1`.
 *
 * The persist happens BEFORE this returns, so a crash / reload right
 * after a post can never re-issue `value` — burning a value is safe,
 * re-issuing one re-uses the AEAD nonce (catastrophic). Pure given the
 * `localStorage` backend.
 */
export function issueAndAdvance(
  inMemory: bigint | undefined,
  channelIdHex: string,
): { readonly value: bigint; readonly advanced: bigint } {
  const value = inMemory ?? loadCounter(channelIdHex) ?? 0n;
  const advanced = value + 1n;
  saveCounter(channelIdHex, advanced);
  return { value, advanced };
}

/**
 * Load every persisted per-channel counter into a fresh map. Used to
 * seed the in-memory `counterByChannel` signal at app start so the
 * first publish after a reload resumes the floor instead of rewinding
 * to 0.
 */
export function loadAllCounters(): Map<string, bigint> {
  const out = new Map<string, bigint>();
  if (typeof localStorage === "undefined") return out;
  for (let i = 0; i < localStorage.length; i += 1) {
    const key = localStorage.key(i);
    if (key === null || !key.startsWith(KEY_PREFIX)) continue;
    const channelIdHex = key.slice(KEY_PREFIX.length);
    const value = loadCounter(channelIdHex);
    if (value !== null) out.set(channelIdHex, value);
  }
  return out;
}

/**
 * Erase every persisted publish counter. Safe ONLY on a full device
 * wipe (`forgetDevice`) that also destroys the PublisherGroup seed
 * source — never on an un-follow or a soft `logout()` that leaves the
 * seed live (that would rewind the counter and re-open the AEAD reuse
 * hole).
 */
export function clearAllCounters(): void {
  if (typeof localStorage === "undefined") return;
  // Collect first — removing during a `length`/`key(i)` walk shifts
  // indices and would skip entries.
  const doomed: string[] = [];
  for (let i = 0; i < localStorage.length; i += 1) {
    const key = localStorage.key(i);
    if (key !== null && key.startsWith(KEY_PREFIX)) doomed.push(key);
  }
  for (const key of doomed) {
    try {
      localStorage.removeItem(key);
    } catch {
      // ignore — best-effort, see `saveCounter`.
    }
  }
}
