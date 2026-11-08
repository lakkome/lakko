// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Publisher requests this account sent and that have not landed (ADR-0087
 * section 15, ADR-0045 decision 2: a request has no deadline).
 *
 * `Accept` on a publisher invitation knocks with a KeyPackage of the
 * conversation account identity and waits for the owner's device. The owner
 * may approve minutes or days later, when its app next runs; the Welcome then
 * lands in this account's mailbox. The record is what lets the mailbox
 * receive loop (`contacts/MailboxContext.tsx`) join that Welcome whenever it
 * arrives, after a restart too, whether or not the Accept page is still open.
 *
 * Per account, in `localStorage` (no secret in it: the KeyPackage's private
 * key is in the identity's provider snapshot, saved before the knock). A
 * record goes once the channel is joined, when the channel is gone, and with
 * the account wipe. A second request for the same channel replaces the first.
 */
import { createSignal, type Accessor } from "solid-js";

const KEY_PREFIX = "lakko.channels.pending-publisher.v1:";

/** One request waiting for the owner's Welcome. */
export interface PendingPublisherRequest {
  readonly channelIdHex: string;
  /** The PublisherGroup the link named. */
  readonly groupIdHex: string;
  /** base64 of the account key the knocked KeyPackage belongs to (the
   *  conversation account identity at the time of the request). */
  readonly joinerKeyB64: string;
  /** What the link said, for the channel row once joined. */
  readonly name: string;
  readonly description: string;
  /** When the request was last sent: the first `Accept`, or the last
   *  `Ask again` that went out (UX 15.2.2: the 7 days start from it). */
  readonly requestedAtMs: number;
  /** `Ask again` found the link no longer works (UX 15.2.2). The record
   *  stays, so an approval of the earlier request still lands. */
  readonly linkDead?: boolean;
}

const [version, setVersion] = createSignal(0);

/** Reactive: moves whenever a record is written or dropped. */
export const pendingPublisherRequestsVersion: Accessor<number> = version;

function keyFor(accountIdB64: string): string {
  return `${KEY_PREFIX}${accountIdB64}`;
}

function isRecord(value: unknown): value is PendingPublisherRequest {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v["channelIdHex"] === "string" &&
    typeof v["groupIdHex"] === "string" &&
    typeof v["joinerKeyB64"] === "string" &&
    typeof v["name"] === "string" &&
    typeof v["description"] === "string" &&
    typeof v["requestedAtMs"] === "number"
  );
}

/** Every pending request of the account; an unreadable row reads as none. */
export function loadPendingPublisherRequests(accountIdB64: string): PendingPublisherRequest[] {
  if (typeof localStorage === "undefined") return [];
  let raw: string | null;
  try {
    raw = localStorage.getItem(keyFor(accountIdB64));
  } catch {
    return [];
  }
  if (raw === null) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter(isRecord) : [];
  } catch {
    return [];
  }
}

function save(accountIdB64: string, list: readonly PendingPublisherRequest[]): void {
  if (typeof localStorage === "undefined") throw new Error("no local storage");
  if (list.length === 0) localStorage.removeItem(keyFor(accountIdB64));
  else localStorage.setItem(keyFor(accountIdB64), JSON.stringify(list));
  setVersion((v) => v + 1);
}

/** Record a request, replacing an earlier one for the same channel. Throws
 *  when it cannot be written (the caller must not knock then). */
export function recordPendingPublisherRequest(accountIdB64: string, record: PendingPublisherRequest): void {
  const rest = loadPendingPublisherRequests(accountIdB64).filter((r) => r.channelIdHex !== record.channelIdHex);
  save(accountIdB64, [...rest, record]);
}

/** UX 15.2.2 — `Ask again` found the link dead: the record stays (a late
 *  approval still lands) and the card keeps the invalid-link line. */
export function markPendingPublisherLinkDead(accountIdB64: string, channelIdHex: string): void {
  const list = loadPendingPublisherRequests(accountIdB64);
  if (!list.some((r) => r.channelIdHex === channelIdHex && r.linkDead !== true)) return;
  save(
    accountIdB64,
    list.map((r) => (r.channelIdHex === channelIdHex ? { ...r, linkDead: true } : r)),
  );
}

/** Drop the request for a channel (joined, or the channel is gone). */
export function dropPendingPublisherRequest(accountIdB64: string, channelIdHex: string): void {
  const list = loadPendingPublisherRequests(accountIdB64);
  const rest = list.filter((r) => r.channelIdHex !== channelIdHex);
  if (rest.length !== list.length) save(accountIdB64, rest);
}

/** The account wipe. */
export function clearPendingPublisherRequests(accountIdB64: string): void {
  if (typeof localStorage === "undefined") return;
  localStorage.removeItem(keyFor(accountIdB64));
  setVersion((v) => v + 1);
}
