// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0073 R.2 / UX `identity-recovery.md` 16.3 - under which key the
 * profile that names a person is filed.
 *
 * A contact's accepted key change (a reset, or a proven rotation) re-keys
 * their contact row; the retired keys stay as aliases of the row key
 * (`loadKeyAliases`: retired key -> current key). A profile is filed under
 * the key that SENT it, so after the change the person's only profile is,
 * for a while, the one their old key sent: their new identity has not sent
 * one yet (a reset device knows nobody until a conversation reaches it).
 *
 * The lookup therefore goes both ways along the alias:
 *
 * | asked about          | order tried                                                 |
 * |----------------------|-------------------------------------------------------------|
 * | the current key      | that key, then the retired keys of the row, newest first    |
 * | a retired key        | that key, then the current key                              |
 * | a key with no alias  | that key                                                    |
 *
 * "Newest" is the profile's own stamp. Before this, a roster naming the new
 * key (the successor of a one-to-one) read `Member` while the person's name
 * was held under the key they had a minute earlier (seen live 2026-11-25).
 * Only keys this device accepted as the same person are ever aliased, so
 * this names nobody new.
 *
 * Pure: no store, no Solid.
 */

/**
 * The key whose profile names `keyB64`, or `undefined` when no key along its
 * alias holds one. `stampOf` answers the held profile's stamp, `undefined`
 * when none is held for that key.
 */
export function profileKeyFor(
  keyB64: string,
  aliases: ReadonlyMap<string, string>,
  stampOf: (keyB64: string) => bigint | undefined,
): string | undefined {
  if (stampOf(keyB64) !== undefined) return keyB64;
  const canonical = aliases.get(keyB64) ?? keyB64;
  if (canonical !== keyB64 && stampOf(canonical) !== undefined) return canonical;
  let best: string | undefined;
  let bestStamp: bigint | undefined;
  for (const [retired, current] of aliases) {
    if (current !== canonical || retired === keyB64) continue;
    const stamp = stampOf(retired);
    if (stamp === undefined) continue;
    if (bestStamp === undefined || stamp > bestStamp) {
      best = retired;
      bestStamp = stamp;
    }
  }
  return best;
}
