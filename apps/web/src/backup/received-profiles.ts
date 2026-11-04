// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a backup keeps of the OTHER people this device knows (lakko-backup
 * payload v8): the newest profile envelope it holds from each, filed under
 * the person key the profile store uses, so a device restored with no other
 * device alive shows every sender with a name (route map W8, 2026-11-04).
 *
 * Writing: the people are listed own contacts first, then everyone else by
 * how recently they wrote in a conversation this device holds, then the
 * rest; the account's own keys never. The core keeps the head of the list
 * that fits its bounds (`fit_received_profiles`), so the order is what
 * survives a full list.
 *
 * Restoring: each entry is offered to the profile runtime exactly as a
 * pairing bundle's profiles are (`devices/carried-state.ts`), which files it
 * through the store's ordinary receive gate: strictly newer per key, a
 * newer profile already held stays.
 *
 * Until the profile runtime has filed them, the restored entries are also
 * kept in memory for the account ({@link restoredProfilesFor}), so an upload
 * that runs before the store has them still carries them.
 *
 * Pure apart from {@link offerRestoredProfiles} and that memory.
 */
import { registerAccountMemory } from "../auth/account-memory";
import type { MessageLogPayload } from "../chat/message-log-store";
import { carriedProfilesOf, offerCarriedNames } from "../devices/carried-state";
import type { CarriedProfile } from "../devices/state-bundle";
import type { ProfileDecoder } from "../profile/profile-envelope";

/** When each author last wrote, from the message log (`authorKeyB64` ->
 *  newest `createdAt`, UNIX ms). Rows with no author or no parseable time
 *  are skipped. */
export function lastWrittenByAuthor(log: Pick<MessageLogPayload, "groups"> | null): Map<string, number> {
  const out = new Map<string, number>();
  for (const group of Object.values(log?.groups ?? {})) {
    for (const m of group.messages ?? []) {
      const author = m.authorKeyB64;
      if (m.direction !== "incoming" || typeof author !== "string" || author.length === 0) continue;
      const at = Date.parse(m.createdAt);
      if (!Number.isFinite(at)) continue;
      const held = out.get(author);
      if (held === undefined || at > held) out.set(author, at);
    }
  }
  return out;
}

/** The order a backup lists people in: own contacts first, then by how
 *  recently they wrote (newest first), then the rest in the order given; the
 *  account's own keys are left out. Stable. */
export function orderReceivedProfiles(
  profiles: readonly CarriedProfile[],
  contactKeysB64: ReadonlySet<string>,
  lastWrittenMs: ReadonlyMap<string, number>,
  ownKeysB64: ReadonlySet<string>,
): CarriedProfile[] {
  const indexed = profiles
    .map((p, i) => ({ p, i }))
    .filter(({ p }) => !ownKeysB64.has(p.personKeyB64));
  const rank = (key: string): number => (contactKeysB64.has(key) ? 0 : 1);
  indexed.sort((a, b) => {
    const r = rank(a.p.personKeyB64) - rank(b.p.personKeyB64);
    if (r !== 0) return r;
    const ta = lastWrittenMs.get(a.p.personKeyB64) ?? -Infinity;
    const tb = lastWrittenMs.get(b.p.personKeyB64) ?? -Infinity;
    if (ta !== tb) return tb > ta ? 1 : -1;
    return a.i - b.i;
  });
  return indexed.map(({ p }) => p);
}

const restored = new Map<string, readonly CarriedProfile[]>();

registerAccountMemory("restored-received-profiles", (ids) => {
  for (const id of ids) restored.delete(id);
});

/** The profiles a restore of `accountIdB64` brought, this session. */
export function restoredProfilesFor(accountIdB64: string): readonly CarriedProfile[] {
  return restored.get(accountIdB64) ?? [];
}

/** `fromStore`, then every restored entry for a key it lacks: what an upload
 *  carries while the store may not have filed the restore yet. Pure. */
export function withRestoredProfiles(
  fromStore: readonly CarriedProfile[],
  restoredEntries: readonly CarriedProfile[],
): CarriedProfile[] {
  const have = new Set(fromStore.map((p) => p.personKeyB64));
  return [...fromStore, ...restoredEntries.filter((p) => !have.has(p.personKeyB64))];
}

/** Hand the restored profiles to the profile runtime (taken once the account
 *  and its own keys are bound, filed through the ordinary receive gate).
 *  Answers how many decoded. Never throws. */
export function offerRestoredProfiles(
  accountIdB64: string,
  profiles: readonly CarriedProfile[],
  decoder: ProfileDecoder,
): number {
  restored.set(accountIdB64, [...profiles]);
  try {
    const people = carriedProfilesOf({ profiles }, decoder, () => false);
    offerCarriedNames(accountIdB64, new Map(), people);
    return people.size;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("backup restore: names not applied", err instanceof Error ? err.name : "");
    return 0;
  }
}
