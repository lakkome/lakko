// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The name on a remote call tile (`docs/ux/calls.md`, "Every tile carries the
 * person's name").
 *
 * A tile always carries the person's name. One rule for both call kinds, in
 * this order:
 *
 *   1. what the one resolver (`profile/resolver.ts`) calls the person behind
 *      the tile: the viewer's rename, the person's profile name, their
 *      verified `@username`. A key fingerprint is not a name and is skipped;
 *   2. the display name the person announced inside the call (link calls),
 *      for the moment before the profile store has filed it;
 *   3. the username the person claims while it is not verified yet, as plain
 *      text without `@`. The `@` form is shown only for a verified username
 *      (ADR-0048); a bare word claims nothing a typed display name could not.
 *
 * `null` only while nothing has reached the device from that person, and the
 * call screen then draws its unnamed state. That state is meant to last
 * moments: everyone has a name or a username to announce, and the link call
 * keeps the announcements that arrive before it can read them
 * ({@link EarlyRelays}).
 *
 * Never a piece of an account id or of a key. The SFU hands every client an
 * `account_pseudonym` (the first characters of the account id); a tile
 * labelled with it reads like something the viewer ought to recognise and
 * names nobody, and it is what a person without a profile name was shown as
 * until 2026-10-02.
 *
 * Pure: no wasm, no storage, no Solid.
 */
import type { ResolvedName } from "../profile/resolver";

/** The inputs of one tile's name. Each is `null` when that source has nothing. */
export interface TileNameInputs {
  /** The one resolver's name for the person behind the tile
   *  ({@link personNameOrNull}). */
  readonly resolved: string | null;
  /** The display name the person announced inside the call (link calls). */
  readonly announcedName: string | null;
  /** The username the person claims, verified or not, without `@`. */
  readonly claimedUsername: string | null;
}

/** The name the sources give, or `null` while the person is still unnamed. */
export function callTileName(inputs: TileNameInputs): string | null {
  for (const candidate of [inputs.resolved, inputs.announcedName, inputs.claimedUsername]) {
    const name = candidate?.trim() ?? "";
    if (name.length > 0) return name;
  }
  return null;
}

/** A resolved person's name for a tile: `null` when the resolver had only the
 *  key fingerprint to offer. */
export function personNameOrNull(resolved: ResolvedName): string | null {
  return resolved.source === "fingerprint" ? null : resolved.name;
}

/**
 * Whether the account a sealed profile claims is the account the SFU admitted
 * the relaying peer as.
 *
 * A profile in a link call comes from a stranger, and a username certificate
 * binds a username to an ACCOUNT ID, not to the key that sent the profile. The
 * ADR-0021 peer tag is derived from the account id in the peer's
 * call-admission token, so a profile is filed in the profile store, and its
 * username may ever read as verified, only when the claimed account id gives
 * that peer's tag. A participant therefore cannot present another account's
 * id together with that account's certificate.
 */
export function profileClaimMatchesPeer(
  claimedAccountIdHex: string | null,
  peerTag: string | undefined,
  tagOf: (accountIdHex: string) => string,
): boolean {
  if (claimedAccountIdHex === null || peerTag === undefined || peerTag.length === 0) return false;
  try {
    return tagOf(claimedAccountIdHex) === peerTag;
  } catch {
    return false;
  }
}

/** How many relays a link call keeps while it cannot read them yet. */
export const EARLY_RELAY_LIMIT = 64;

/**
 * The relays that reached a joiner before its Welcome.
 *
 * A member's sealed profile and the committer's Welcome come from different
 * senders, so nothing orders them: the profile can arrive first, when there
 * is no group to open it with. Dropped, that member's tile stayed unnamed
 * until someone else was admitted. Kept here, bounded, and handed back once
 * the group exists.
 */
export class EarlyRelays<T> {
  private held: T[] = [];

  /** Keep one; past the limit the oldest is dropped. */
  keep(relay: T): void {
    this.held.push(relay);
    if (this.held.length > EARLY_RELAY_LIMIT) this.held.shift();
  }

  /** Everything kept, oldest first; the queue is empty afterwards. */
  drain(): T[] {
    const out = this.held;
    this.held = [];
    return out;
  }
}
