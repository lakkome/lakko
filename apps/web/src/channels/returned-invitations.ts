// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 - a channel return whose old card binding does NOT
 * verify under this account's previous key (`channel-return.ts`
 * `show-invitation`) is shown as the ordinary channel invitation card
 * (`chat/ChannelInviteCard.tsx`: Subscribe, or Accept for a publisher
 * invitation), above the chat list, for the person to decide on. It is never
 * accepted silently and never dropped; it leaves the list once the account
 * reads the channel (the person subscribed, or another return did).
 *
 * Kept per account in `localStorage`: channel id -> standard base64 of the
 * invitation card (the card carries a link secret, as an invitation message
 * in the message log does). The newest card per channel wins.
 */
import { createSignal } from "solid-js";

interface KeyValueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function storage(): KeyValueStorage | undefined {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    return undefined;
  }
}

function keyOf(accountIdB64: string): string {
  return `lakko:channel-return-invitations:${accountIdB64}`;
}

const [version, setVersion] = createSignal(0);

/** Bumped on every change. Reactive. */
export function returnedInvitationsVersion(): number {
  return version();
}

/** The held invitations, channel id -> card (standard base64). */
export function readReturnedInvitations(
  accountIdB64: string,
  s: KeyValueStorage | undefined = storage(),
): Record<string, string> {
  try {
    const raw = s?.getItem(keyOf(accountIdB64));
    if (raw === null || raw === undefined) return {};
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const out: Record<string, string> = {};
    for (const [id, card] of Object.entries(parsed)) {
      if (/^[0-9a-f]{32}$/.test(id) && typeof card === "string" && card.length > 0) out[id] = card;
    }
    return out;
  } catch {
    return {};
  }
}

/** Keep one (replacing the held card of that channel). Throws when storage
 *  refuses, which keeps the return held for the next pass. */
export function keepReturnedInvitation(
  accountIdB64: string,
  channelIdHex: string,
  cardB64: string,
  s: KeyValueStorage | undefined = storage(),
): void {
  if (s === undefined) throw new Error("no storage for a returned invitation");
  const held = readReturnedInvitations(accountIdB64, s);
  held[channelIdHex] = cardB64;
  s.setItem(keyOf(accountIdB64), JSON.stringify(held));
  setVersion((v) => v + 1);
}

/** Drop the invitations of channels the account now reads. */
export function dropReturnedInvitations(
  accountIdB64: string,
  channelIdsHex: readonly string[],
  s: KeyValueStorage | undefined = storage(),
): void {
  const held = readReturnedInvitations(accountIdB64, s);
  let changed = false;
  for (const id of channelIdsHex) {
    if (held[id] !== undefined) {
      delete held[id];
      changed = true;
    }
  }
  if (!changed) return;
  try {
    if (Object.keys(held).length === 0) s?.removeItem(keyOf(accountIdB64));
    else s?.setItem(keyOf(accountIdB64), JSON.stringify(held));
  } catch {
    /* shown once more; harmless */
  }
  setVersion((v) => v + 1);
}

/** The invitations to show: every held one whose channel the account does
 *  not read yet, in channel id order. Pure. */
export function invitationsToShow(
  held: Readonly<Record<string, string>>,
  reading: ReadonlySet<string>,
): { readonly channelIdHex: string; readonly cardB64: string }[] {
  return Object.keys(held)
    .filter((id) => !reading.has(id))
    .sort()
    .map((id) => ({ channelIdHex: id, cardB64: held[id] as string }));
}
