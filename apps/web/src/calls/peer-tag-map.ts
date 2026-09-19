// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0021 — the per-call `peer_tag -> person` map a group-call roster names
 * its tiles from.
 *
 * The SFU sends no names. It sends, per peer, a tag derived over the
 * JWT-verified account id and the call id, and the client re-derives the same
 * tag over every account id IT holds and matches. Two sources hold one:
 *
 *   * an accepted contact, whose `accountIdHex` the contact flow learned;
 *   * a received profile (ADR-0048 v2), whose account id the sender CLAIMED
 *     in the same envelope as their name.
 *
 * Both are filed under the person's signature key, so the map goes
 * `tag -> personKeyB64` and the caller names the person through the one
 * resolver every screen uses. The name is NOT baked in here: a profile or a
 * rename can land while the call is up, and a rebuilt map plus a fresh
 * resolve is what lets the tile follow.
 *
 * ## The collision rule
 *
 * A tag is 8 bytes of a keyed hash, so a collision between two DIFFERENT
 * people is not expected, but a claimed account id is only a claim: a member
 * who puts someone else's account id in their profile makes their key derive
 * that person's tag. When two distinct keys derive one tag, the tag names
 * nobody, and the tile falls back to the pseudonym — an unnamed tile over a
 * misnamed one. The same person reached through both sources (a contact who
 * also sent a profile) is one key, not a collision.
 */

/** One person's account id, keyed on the signature key it is filed under. */
export interface PeerTagBinding {
  readonly personKeyB64: string;
  readonly accountIdHex: string;
}

/** The built map. `collided` tags are never looked up. */
export interface PeerTagMap {
  readonly tagToPerson: ReadonlyMap<string, string>;
  readonly collided: ReadonlySet<string>;
}

/**
 * Build the map for one call. `tagOf` is the core's `sfuPeerTag` bound to the
 * call id; it is called once per distinct `(personKey, accountId)` pair.
 */
export function buildPeerTagMap(
  bindings: Iterable<PeerTagBinding>,
  tagOf: (accountIdHex: string) => string,
): PeerTagMap {
  const tagToPerson = new Map<string, string>();
  const collided = new Set<string>();
  const seen = new Set<string>();
  for (const b of bindings) {
    // The same pair from both sources is one binding.
    const pair = `${b.personKeyB64}|${b.accountIdHex}`;
    if (seen.has(pair)) continue;
    seen.add(pair);
    const tag = tagOf(b.accountIdHex);
    const held = tagToPerson.get(tag);
    if (held === undefined) tagToPerson.set(tag, b.personKeyB64);
    else if (held !== b.personKeyB64) collided.add(tag);
  }
  return { tagToPerson, collided };
}

/** The person a tag names, or `null` when unknown or collided. */
export function personForPeerTag(map: PeerTagMap, peerTag: string | undefined): string | null {
  if (peerTag === undefined || map.collided.has(peerTag)) return null;
  return map.tagToPerson.get(peerTag) ?? null;
}
