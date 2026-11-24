// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The rows of a channel's subscriber list (`docs/ux/group-and-channel-privacy.md`
 * sections 15.1 and 7.3), from the member cards this device opened and, in a
 * private channel for the owner and publishers, every registered key.
 *
 * The rule is the shared one (`lakko_ui_rules::subscribers`, over wasm), so
 * the three clients cannot disagree: a card that carries no name is not a
 * named row; a person shown by name is never listed again among the others;
 * in a private channel the owner and publishers may remove every row but
 * their own, named rows included. Base walk 2026-11-13: a nameless card read
 * "Member", and both subscribers were then listed a second time as
 * anonymous rows, so the owner could not tell whom `Remove` took.
 */
import type { ChannelMemberRow } from "./member-cards";

/** A registered key with the link it came by (`channelSubscriberPage`). */
export interface ListedSubscriber {
  readonly pkHex: string;
}

/** The shared rule, as the wasm surface exposes it. */
export interface SubscriberListRule {
  channelSubscriberList(
    cardSubscriberKeys: string[],
    cardPersonKeys: string[],
    cardFlags: Uint8Array,
    listedKeys: string[],
    count: number | undefined,
    canRemove: boolean,
  ): {
    readonly peopleCount: number;
    personCards(i: number): Uint32Array | number[];
    personRemovable(i: number): boolean;
    readonly others: Uint32Array | number[];
    readonly othersCount: number;
    free?: () => void;
  };
}

export interface SubscriberPersonView {
  readonly row: ChannelMemberRow;
  /** Every subscriber key of this person: `Remove` removes them all. */
  readonly pksHex: readonly string[];
  readonly removable: boolean;
}

export interface SubscriberListView<T extends ListedSubscriber> {
  readonly people: readonly SubscriberPersonView[];
  /** The others listed one row each (private channel, owner, publishers). */
  readonly others: readonly T[];
  /** The number in "{n} others."; `0` says nothing. */
  readonly othersCount: number;
}

/**
 * Build the list. `members` in display order (the viewer first, then by
 * name); `listed` the keys read so far (empty unless the viewer may remove);
 * `count` the subscriber count or `null`. Pure.
 */
export function subscriberListView<T extends ListedSubscriber>(
  rule: SubscriberListRule,
  members: readonly ChannelMemberRow[],
  listed: readonly T[],
  count: number | null,
  canRemove: boolean,
  /** This device's own registered subscriber key: always the viewer's own
   *  ("You"), card or not, so it is never an anonymous row to remove. */
  ownKeyHex: string | null = null,
  /** Keys that are not subscribers: the publishing viewer's own account
   *  reader key (`subscriber-count.ts` `ownPublisherKeys`). Left out of the
   *  cards and the listed rows; the count passed must already leave them
   *  out (`shownSubscriberCount`). */
  excludedKeysHex: readonly string[] = [],
): SubscriberListView<T> {
  const excluded = new Set(excludedKeysHex.map((k) => k.toLowerCase()));
  if (excluded.size > 0) {
    members = members
      .map((m) => ({ ...m, subscriberPksHex: m.subscriberPksHex.filter((k) => !excluded.has(k.toLowerCase())) }))
      .filter((m) => m.subscriberPksHex.length > 0);
    listed = listed.filter((l) => !excluded.has(l.pkHex.toLowerCase()));
  }
  const ownLower = ownKeyHex?.toLowerCase() ?? null;
  const own = ownLower !== null && excluded.has(ownLower) ? null : ownLower;
  if (own !== null && !members.some((m) => m.subscriberPksHex.some((k) => k.toLowerCase() === own))) {
    const selfRow = members.find((m) => m.isSelf);
    const youRow: ChannelMemberRow = selfRow ?? {
      key: "self",
      name: "",
      username: null,
      isSelf: true,
      named: true,
      subscriberPksHex: [],
    };
    members = [
      { ...youRow, subscriberPksHex: [...youRow.subscriberPksHex, own] },
      ...members.filter((m) => m !== selfRow),
    ];
  }
  const cardRows: ChannelMemberRow[] = [];
  const cardKeys: string[] = [];
  const personKeys: string[] = [];
  const flags: number[] = [];
  for (const row of members) {
    for (const pk of row.subscriberPksHex) {
      cardRows.push(row);
      cardKeys.push(pk);
      personKeys.push(row.isSelf ? "self" : row.key);
      flags.push((row.named ? 1 : 0) | (row.isSelf ? 2 : 0));
    }
  }
  const out = rule.channelSubscriberList(
    cardKeys,
    personKeys,
    Uint8Array.from(flags),
    listed.map((l) => l.pkHex),
    count ?? undefined,
    canRemove,
  );
  try {
    const people: SubscriberPersonView[] = [];
    for (let i = 0; i < out.peopleCount; i++) {
      const cards = Array.from(out.personCards(i));
      const first = cards[0];
      const row = first === undefined ? undefined : cardRows[first];
      if (row === undefined) continue;
      people.push({
        row,
        pksHex: cards.map((c) => cardKeys[c]).filter((k): k is string => k !== undefined),
        removable: out.personRemovable(i),
      });
    }
    const others = Array.from(out.others)
      .map((i) => listed[i])
      .filter((l): l is T => l !== undefined);
    return { people, others, othersCount: out.othersCount };
  } finally {
    out.free?.();
  }
}
