// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Named membership rows (`docs/ux/second-device.md` section 4, "Membership
 * rows", decided 2026-10-29).
 *
 * There is no generic `Members changed` row. In a GROUP, when a received batch
 * that carries at least one commit changes the set of PEOPLE (person keys,
 * ADR-0050 account keys), the device records on the batch's LAST commit row
 * the person keys that joined and the person keys that left, and the row is
 * drawn as one sentence per person, joined first ("VG3 joined", "VG2 is no
 * longer in the group": the roster cannot tell leaving from being removed).
 * The keys are stored, never the names: a name is resolved when the row is
 * drawn, through the one display resolver, so the row follows the person's
 * profile when it lands. A person the resolver has no real name for is not
 * drawn at all (no placeholder word, no fingerprint) until it has one.
 *
 * Nothing else is drawn: a commit row with no keys (every row stored before
 * this rule, and every commit that only admitted or retired a person's
 * devices) and any commit row of a one-to-one, whose people cannot change
 * (ADR-0062). A person key retired by an accepted reset and its successor are
 * one person (`deviceSwapAcrossCommits`, ADR-0073 R.7 case 2), so a reset
 * repair records nothing either. This device's own joining or removal has
 * its own surfaces and is never named here.
 *
 * Pure: no wasm, no Solid, no storage, so vitest runs every case.
 */
import type { IncomingChatEvent } from "./client";
import type { ConversationKind } from "./conversation-kind";
import { deviceSwapAcrossCommits, peopleByAccount, type AwaitedSwaps } from "./reset-repair";

/** The person keys (base64 account keys) a batch added and removed. */
export interface MembershipChange {
  readonly joined: readonly string[];
  readonly left: readonly string[];
}

/** Whether a change names anyone. */
export function namesAnyone(change: MembershipChange | undefined): change is MembershipChange {
  return change !== undefined && (change.joined.length > 0 || change.left.length > 0);
}

/**
 * The people a batch of commits added and removed, and the device-swap memory
 * to keep (`deviceSwapAcrossCommits`).
 *
 * `change` is `null` when nothing is to be recorded: a torn roster read on
 * either side, a batch that only changed devices or only swapped a reset
 * person's keys (in one batch or across two), or a change that names nobody
 * but this device's own person. One key per person: the first key the roster
 * named them under, preferring one no accepted reset retired.
 */
export function membershipChangeOfBatch(input: {
  readonly personKeysBefore: readonly string[] | null;
  readonly personKeysAfter: readonly string[] | null;
  readonly accountOfKey: (keyB64: string) => string | null;
  readonly isSuperseded: (keyB64: string) => boolean;
  readonly awaited: AwaitedSwaps;
  readonly nowMs: number;
  /** This device's own person key; never named. */
  readonly ownPersonKeyB64: string | null;
}): { readonly change: MembershipChange | null; readonly awaited: AwaitedSwaps } {
  const swap = deviceSwapAcrossCommits(input);
  const before = input.personKeysBefore;
  const after = input.personKeysAfter;
  if (swap.suppress || before === null || after === null) {
    return { change: null, awaited: swap.awaited };
  }
  const person = (k: string): string => input.accountOfKey(k) ?? k;
  const own = input.ownPersonKeyB64 === null ? null : person(input.ownPersonKeyB64);
  const peopleBefore = peopleByAccount(before, input.accountOfKey);
  const peopleAfter = peopleByAccount(after, input.accountOfKey);
  const keyOf = (keys: readonly string[], p: string): string | undefined => {
    const of = keys.filter((k) => person(k) === p);
    return of.find((k) => !input.isSuperseded(k)) ?? of[0];
  };
  const pick = (people: readonly string[], keys: readonly string[]): string[] => {
    const out: string[] = [];
    for (const p of people) {
      if (p === own) continue;
      const k = keyOf(keys, p);
      if (k !== undefined && !out.includes(k)) out.push(k);
    }
    return out;
  };
  const change: MembershipChange = {
    joined: pick(
      peopleAfter.filter((p) => !peopleBefore.includes(p)),
      after,
    ),
    left: pick(
      peopleBefore.filter((p) => !peopleAfter.includes(p)),
      before,
    ),
  };
  return { change: namesAnyone(change) ? change : null, awaited: swap.awaited };
}

/** The part of a received event this module reads and writes. */
export interface MembershipEventShape {
  readonly kind: string;
  readonly messageId?: number;
  readonly systemKind?: string;
  readonly membership?: MembershipChange;
}

/** Whether an event is a commit row. */
export function isCommitEvent(e: { readonly kind: string; readonly systemKind?: string }): boolean {
  return e.kind === "system" && e.systemKind === "commit";
}

/**
 * The batch with `change` recorded on its LAST commit row (by DS id). Every
 * other event is returned as it was; a batch with no commit, a `null` change
 * and a one-to-one record nothing.
 */
export function withMembershipChange<E extends MembershipEventShape>(
  events: readonly E[],
  change: MembershipChange | null,
  kind: ConversationKind,
): E[] {
  if (change === null || kind !== "group" || !namesAnyone(change)) return [...events];
  let last: E | undefined;
  let lastId = Number.NEGATIVE_INFINITY;
  for (const e of events) {
    if (isCommitEvent(e) && e.messageId !== undefined && e.messageId > lastId) {
      last = e;
      lastId = e.messageId;
    }
  }
  if (last === undefined) return [...events];
  const target = last;
  return events.map((e) => (e === target ? { ...e, membership: change } : e));
}

/**
 * The events of a sweep as one batch: the rows the verdict applied below this
 * device's own commit, then, when the sweep merged, the own commit itself at
 * the DS id the commit route answered.
 *
 * A device never receives its own commit (the echo is suppressed), so without
 * this the committer would be the one member with no row for the change it
 * made. The own commit is the batch's last commit, so `withMembershipChange`
 * puts the change judged across the whole sweep (the roster before its reads
 * against the roster after the merge) on it, and the rows below it carry none:
 * nothing they changed is lost. `createdAt` is this device's clock; the DS
 * time of the row is not returned by the commit route.
 */
export function sweepBatchWithOwnCommit(
  swept: readonly IncomingChatEvent[],
  outcome: { readonly kind: string; readonly commitMessageId?: number } | null,
  createdAt: string,
): IncomingChatEvent[] {
  const id = outcome?.kind === "committed" ? outcome.commitMessageId : undefined;
  if (id === undefined || swept.some((e) => "messageId" in e && e.messageId === id)) return [...swept];
  return [...swept, { kind: "system", messageId: id, systemKind: "commit", createdAt }];
}

/** The part of a timeline row the display rule reads. */
export interface MembershipRowShape {
  readonly kind: string;
  readonly event?: {
    readonly kind: string;
    readonly systemKind?: string;
    readonly membership?: MembershipChange;
  };
}

/**
 * Whether a timeline row is drawn as far as membership is concerned: a commit
 * row only in a group and only when it names someone. Every other row is
 * drawn.
 */
export function isDrawnMembershipRow(row: MembershipRowShape, kind: ConversationKind): boolean {
  if (!(row.kind === "incoming" && row.event !== undefined && isCommitEvent(row.event))) return true;
  return kind === "group" && namesAnyone(row.event.membership);
}

/**
 * The rows of an open conversation as the timeline shows them: every commit
 * row the rule above does not draw is left out, stored ones included. The log
 * keeps them.
 */
export function withDrawnMembershipRows<Row extends MembershipRowShape>(
  rows: readonly Row[],
  kind: ConversationKind,
): readonly Row[] {
  return rows.filter((r) => isDrawnMembershipRow(r, kind));
}

/**
 * The sentences of a membership row, joined first: one per person, each
 * `{name} joined` / `{name} left` with the name the resolver gives the stored
 * key at draw time. A person `nameOf` has no real name for (`null`, blank)
 * gets no sentence yet; the row is drawn again when their profile lands.
 */
export function membershipSentences(
  change: MembershipChange,
  render: (key: "chat.system.member_joined" | "chat.system.member_left", name: string) => string,
  nameOf: (personKeyB64: string) => string | null,
): string[] {
  const named = (keys: readonly string[], key: "chat.system.member_joined" | "chat.system.member_left"): string[] =>
    keys.flatMap((k) => {
      const name = nameOf(k)?.trim() ?? "";
      return name.length > 0 ? [render(key, name)] : [];
    });
  return [...named(change.joined, "chat.system.member_joined"), ...named(change.left, "chat.system.member_left")];
}

/** The stored form of a change (`PersistedMessage.membership`). */
export interface PersistedMembership {
  readonly joinedKeysB64: readonly string[];
  readonly leftKeysB64: readonly string[];
}

/** A change as the log stores it, or `undefined` when it names nobody. */
export function persistedFromMembership(
  change: MembershipChange | undefined,
): PersistedMembership | undefined {
  if (!namesAnyone(change)) return undefined;
  return { joinedKeysB64: [...change.joined], leftKeysB64: [...change.left] };
}

/** A stored change read back; anything malformed reads as none. */
export function membershipFromPersisted(stored: unknown): MembershipChange | undefined {
  if (typeof stored !== "object" || stored === null) return undefined;
  const s = stored as { joinedKeysB64?: unknown; leftKeysB64?: unknown };
  const strings = (v: unknown): string[] =>
    Array.isArray(v) ? v.filter((x): x is string => typeof x === "string" && x.length > 0) : [];
  const change: MembershipChange = { joined: strings(s.joinedKeysB64), left: strings(s.leftKeysB64) };
  return namesAnyone(change) ? change : undefined;
}
