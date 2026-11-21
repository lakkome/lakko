// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The groups this account shares with a person (`docs/ux/who-can-find-me.md`
 * section 6.4, step 2): groups, never one-to-ones, this device is still a
 * current member of, whose people include the person under their key or one
 * they rotated from. Only a current member can write in a group, so a group
 * this device left is not one. Pure; the readers are passed in.
 */
import type { SharedGroup } from "./message-action";
import { getDeviceKey } from "../auth/key-store";
import type { ActiveGroup } from "../chat/ChatContext";
import { summarizeGroupSlot } from "../chat/background-log";
import { conversationKindOf } from "../chat/conversation-kind";
import { readIsActive } from "../chat/membership-ended";
import { loadMessageLog, type PersistedGroup } from "../chat/message-log-store";
import { personKeysOf } from "../chat/person-key";
import { groupIdHex as syncGroupIdHex } from "../devices/sync/state";

export interface GroupLike {
  readonly groupIdB64: string;
}

export function sharedGroupsWith<G extends GroupLike>(input: {
  readonly groups: readonly G[];
  readonly personKeysB64: readonly string[];
  readonly ownKeyB64: string | null;
  readonly isOneToOne: (g: G) => boolean;
  /** `false` once this device's membership ended. */
  readonly isActive: (g: G) => boolean;
  readonly peopleOf: (g: G) => readonly string[];
  /** The newest message's time, or the group's creation here. */
  readonly newestMs: (g: G) => number;
}): SharedGroup[] {
  if (input.ownKeyB64 === null) return [];
  const own = input.ownKeyB64;
  const out: SharedGroup[] = [];
  for (const g of input.groups) {
    if (input.isOneToOne(g) || !input.isActive(g)) continue;
    const people = input.peopleOf(g);
    if (!people.includes(own)) continue;
    if (!input.personKeysB64.some((k) => k !== own && people.includes(k))) continue;
    out.push({ groupIdB64: g.groupIdB64, newestMs: input.newestMs(g) });
  }
  return out;
}

/** {@link sharedGroupsWith} over the live registry, each group timed by its
 *  newest message in this account's message log. Never throws. */
export async function sharedGroupsFor(input: {
  readonly groups: readonly ActiveGroup[];
  readonly accountIdB64: string | null;
  readonly ownKeyB64: string | null;
  readonly personKeysB64: readonly string[];
}): Promise<SharedGroup[]> {
  let slots: Readonly<Record<string, PersistedGroup>> = {};
  if (input.accountIdB64 !== null) {
    try {
      const key = await getDeviceKey(input.accountIdB64);
      const log = key === null ? null : await loadMessageLog(input.accountIdB64, key);
      slots = log?.groups ?? {};
    } catch {
      slots = {};
    }
  }
  return sharedGroupsWith({
    groups: input.groups,
    personKeysB64: input.personKeysB64,
    ownKeyB64: input.ownKeyB64,
    isOneToOne: (g) => conversationKindOf(g.group) === "chat",
    isActive: (g) => readIsActive(g.group) !== false,
    peopleOf: (g) => personKeysOf(g.group),
    newestMs: (g) => {
      const at = summarizeGroupSlot(slots[syncGroupIdHex(g.group.groupId)], null).lastAt;
      const ms = at === null ? Number.NaN : new Date(at).getTime();
      return Number.isNaN(ms) ? g.createdAtMs : ms;
    },
  });
}

/**
 * G-53 - the conversations (group id hex) this account and a person are both
 * current members of, for the core's reach-write plan (`reach/write-plan.ts`
 * `sharedScopes`): any conversation, a one-to-one included, this device is
 * still a current member of and whose roster holds this account and one of
 * the person's keys (`personKeysHex`: the current key and the proven
 * predecessors). Pure; the readers are passed in.
 */
export function sharedScopeHexesWith<G>(input: {
  readonly groups: readonly G[];
  /** The person's keys, hex, current first. */
  readonly personKeysHex: readonly string[];
  /** This account's key, hex. */
  readonly ownKeyHex: string | null;
  readonly isActive: (g: G) => boolean;
  /** The roster's person keys, hex. */
  readonly peopleHexOf: (g: G) => readonly string[];
  readonly groupIdHexOf: (g: G) => string;
}): string[] {
  if (input.ownKeyHex === null) return [];
  const own = input.ownKeyHex;
  const person = input.personKeysHex.filter((k) => k !== own);
  if (person.length === 0) return [];
  const out: string[] = [];
  for (const g of input.groups) {
    if (!input.isActive(g)) continue;
    const people = input.peopleHexOf(g);
    if (!people.includes(own)) continue;
    if (!person.some((k) => people.includes(k))) continue;
    const id = input.groupIdHexOf(g);
    if (!out.includes(id)) out.push(id);
  }
  return out;
}

/** {@link sharedScopeHexesWith} over the live registry. Never throws. */
export function sharedScopeHexesFor(input: {
  readonly groups: readonly ActiveGroup[];
  readonly ownKeyB64: string | null;
  readonly personKeysHex: readonly string[];
}): string[] {
  const hexOfB64 = (b64: string): string => {
    const bin = atob(b64);
    let out = "";
    for (let i = 0; i < bin.length; i++) out += bin.charCodeAt(i).toString(16).padStart(2, "0");
    return out;
  };
  try {
    return sharedScopeHexesWith({
      groups: input.groups,
      personKeysHex: input.personKeysHex,
      ownKeyHex: input.ownKeyB64 === null ? null : hexOfB64(input.ownKeyB64),
      isActive: (g) => readIsActive(g.group) !== false,
      peopleHexOf: (g) => personKeysOf(g.group).map(hexOfB64),
      groupIdHexOf: (g) => syncGroupIdHex(g.group.groupId),
    });
  } catch {
    return [];
  }
}
