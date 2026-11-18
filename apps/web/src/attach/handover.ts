// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 section 3.9 — the open-items hand-over.
 *
 * Committer side: after an admission commit that adds a leaf (a newcomer, or a
 * new device of a member), the device that committed it posts, sealed at the
 * new epoch, every OPEN poll and checklist it holds (with an anonymous poll's
 * ring, a public poll's latest votes, a checklist's added items and ticks),
 * and for an anonymous poll the stored ballot rows the journal will soon drop.
 * The core packs the rows (`planOpenItemsRows`, `planPollBallotsRows`).
 *
 * Receiver side: every member receives it; a device that holds an item
 * ignores it, a device that does not adopts it — only from an owner or admin,
 * or from its own account (ADR-0050: its own device admitted it), checked by
 * `openItemsSenderTrusted`. The poll's author signature and version 0 of its
 * ring are checked by `adoptOpenItem`; the rest rests on the committer's word,
 * as the ADR records.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { loadWasm } from "../wasm";
import type { IncomingChatEvent } from "../chat/client";
import type {
  ChecklistAddRecord,
  ChecklistTickRecord,
  MessageContent,
  OpenItem,
  PollVoteRecord,
  SignedChecklist,
  SignedPoll,
} from "../../wasm/lakko_ffi_wasm";
import {
  loadContent,
  updateContent,
  replacingSnapshot,
  withBallotRow,
  withEntry,
  type ContentEntry,
  type ChecklistEntry,
  type ConversationContent,
  type PollEntry,
} from "./content-store";
import { isOwnerOrAdmin, rosterAccountKeys, type ReceiveGroup } from "./receive";
import { admitterOf } from "./admitters";
import { backfillBallots } from "./ballot-backfill";
import { DsClient } from "../ds/client";
import { boundContactsAccount } from "../contacts/local-store";

type Wasm = Awaited<ReturnType<typeof loadWasm>>;
type ApplicationEvent = Extract<IncomingChatEvent, { kind: "application" }>;
type OpenItemsEvent = Extract<IncomingChatEvent, { kind: "openItems" }>;

// ----- committer ---------------------------------------------------------

/** Whether a poll or checklist is open now (by its ops and its close time). */
export function entryIsOpen(
  entry: { readonly kind: string; readonly closes: readonly unknown[]; readonly signed: unknown },
  nowMs: number,
): boolean {
  if (entry.closes.length > 0) return false;
  if (entry.kind === "poll") {
    const closeAt = (entry.signed as SignedPoll).body.closeAtMs;
    if (closeAt != null && nowMs >= closeAt) return false;
  }
  return true;
}

/** The open, verified items a hand-over would carry, ascending by ref. */
export function openEntries(c: ConversationContent, nowMs: number): (PollEntry | ChecklistEntry)[] {
  return Object.values(c.entries)
    .filter((e) => e.verified && entryIsOpen(e, nowMs))
    .sort((a, b) => a.messageId - b.messageId);
}

/**
 * The rows to post after an admission commit that added `addedLeaves`
 * leaves: the open-items envelopes, then the old-ballot envelopes. Empty when
 * no hand-over is due. An item the core refuses to pack is skipped alone.
 */
export async function planHandoverRows(groupIdB64: string, addedLeaves: number, nowMs: number): Promise<Uint8Array[]> {
  const wasm = await loadWasm();
  const content = await loadContent(groupIdB64);
  const open = openEntries(content, nowMs);
  if (!wasm.openItemsHandoverDue(addedLeaves, open.length)) return [];
  const items: OpenItem[] = [];
  const ballotRows: Uint8Array[] = [];
  try {
    for (const entry of open) {
      const item = openItemOf(wasm, entry);
      if (item !== null) items.push(item);
      if (entry.kind === "poll" && entry.ringB64 !== undefined) {
        const old = entry.ballots
          .filter((b) => b.verified !== null && wasm.ballotNeedsHandover(BigInt(b.createdAtMs), BigInt(nowMs)))
          .map((b) => base64Decode(b.rowB64));
        if (old.length > 0) ballotRows.push(...wasm.planPollBallotsRows(BigInt(entry.messageId), old));
      }
    }
    // `planOpenItemsRows` takes the items (they are moved into the core).
    const handed = items.splice(0);
    const rows = handed.length === 0 ? [] : wasm.planOpenItemsRows(handed);
    return [...rows, ...ballotRows];
  } finally {
    for (const i of items) i.free();
  }
}

function openItemOf(wasm: Wasm, entry: PollEntry | ChecklistEntry): OpenItem | null {
  const author = base64Decode(entry.authorKeyB64);
  try {
    if (entry.kind === "poll") {
      const ring = entry.ringB64 === undefined ? undefined : wasm.PollRing.decode(base64Decode(entry.ringB64));
      try {
        return wasm.openItemForPoll(BigInt(entry.messageId), entry.signed, author, ring, latestVotes(entry.votes));
      } finally {
        ring?.free();
      }
    }
    const current = entry.currentSigned ?? entry.signed;
    const state = wasm.foldChecklist(
      entry.signed.body,
      author,
      BigInt(entry.messageId),
      [...entry.edits],
      [...entry.adds],
      [...entry.ticks],
      [...entry.closes],
    );
    return wasm.openItemForChecklist(BigInt(entry.messageId), current, author, state, latestTicks(entry.ticks));
  } catch (err) {
    console.warn("[attach] open item not packed", err instanceof Error ? err.name : "unknown");
    return null;
  }
}

/** Each voter's latest vote, retracts left out (folds the same). */
export function latestVotes(votes: readonly PollVoteRecord[]): PollVoteRecord[] {
  const last = new Map<string, PollVoteRecord>();
  for (const v of [...votes].sort((a, b) => a.messageId - b.messageId)) last.set(v.voter, v);
  return [...last.values()].filter((v) => v.choices.length > 0).sort((a, b) => (a.voter < b.voter ? -1 : 1));
}

/** One tick per item, its latest; undone marks left out. */
export function latestTicks(ticks: readonly ChecklistTickRecord[]): ChecklistTickRecord[] {
  const last = new Map<string, ChecklistTickRecord>();
  for (const t of [...ticks].sort((a, b) => a.messageId - b.messageId)) last.set(JSON.stringify(t.item), t);
  return [...last.values()].filter((t) => t.done);
}

// ----- receiver ----------------------------------------------------------

/** Rows of hand-overs received but not yet whole, per conversation. */
const pending = new Map<string, { sender: Uint8Array; envelope: Uint8Array }[]>();

/**
 * One hand-over row: kept when trusted, then every item it completes and this
 * device does not hold is adopted; each adopted item comes back as the
 * timeline event of its poll or checklist.
 */
export async function receiveOpenItems(
  group: ReceiveGroup,
  groupIdB64: string,
  event: OpenItemsEvent,
): Promise<ApplicationEvent[]> {
  if (event.senderKey === undefined) return [];
  const wasm = await loadWasm();
  const sender = base64Decode(event.senderKey);
  // The admitting account: the sender of this device's Welcome, recorded at
  // join (`admitters.ts`) — a plain member's self-service admission included.
  const account = boundContactsAccount();
  const admitter = account === null ? undefined : await admitterOf(account, groupIdB64);
  const trusted = wasm.openItemsSenderTrusted(
    sender,
    admitter === undefined ? undefined : base64Decode(admitter),
    isOwnerOrAdmin(group, event.senderKey),
  );
  if (!trusted) return [];
  const content = await loadContent(groupIdB64);
  if (event.ballots) {
    await adoptOldBallots(wasm, group.groupId, groupIdB64, event.envelope);
    return [];
  }
  const rows = [...(pending.get(groupIdB64) ?? []), { sender, envelope: event.envelope }];
  pending.set(groupIdB64, rows);
  // An entry filed from a history bundle is not "held": the hand-over's
  // per-voter state replaces it (`replacingSnapshot`).
  const held = BigInt64Array.from(
    Object.entries(content.entries)
      .filter(([, e]) => e.fromSnapshot !== true)
      .map(([k]) => BigInt(k)),
  );
  const received = rows.map((r) => new wasm.ReceivedOpenItemsRow(r.sender, r.envelope));
  const assembled = wasm.assembleOpenItems(received, held);
  const out: ApplicationEvent[] = [];
  for (const item of assembled) {
    try {
      const adopted = wasm.adoptOpenItem(group.groupId, item);
      try {
        const ring = adopted.takeRing();
        let ringB64: string | undefined;
        if (ring !== undefined) {
          try {
            ringB64 = base64Encode(ring.encode());
          } finally {
            ring.free();
          }
        }
        const record = adopted.item as AdoptedItem;
        const ev = await fileAdopted(groupIdB64, record, ringB64, event.createdAt);
        if (ev !== null) out.push(ev);
        // ADR-0090 section 3.9 — the poll's earlier ballots, from the journal.
        if (record.kind === "poll" && ringB64 !== undefined) {
          const groupId = group.groupId;
          const pollRef = Number(record.pollRef);
          const ds = new DsClient();
          void backfillBallots({
            groupId,
            groupIdB64,
            pollMessageId: pollRef,
            page: (after) => ds.fetchGroup(groupId, { after, waitSecs: 0 }),
            isBallotRow: (p) => {
              try {
                return wasm.isPollBallotRow(p);
              } catch {
                return false;
              }
            },
          });
        }
      } finally {
        adopted.free();
      }
    } catch (err) {
      console.warn("[attach] open item refused", err instanceof Error ? err.name : "unknown");
    } finally {
      item.free();
    }
  }
  // Rows of items now held are no longer needed; a partial item keeps its
  // rows until the rest arrive.
  if (out.length > 0) pending.delete(groupIdB64);
  return out;
}

type AdoptedItem =
  | { kind: "poll"; pollRef: number; author: string; poll: SignedPoll; votes: PollVoteRecord[] }
  | {
      kind: "checklist";
      pollRef: number;
      author: string;
      checklist: SignedChecklist;
      adds: ChecklistAddRecord[];
      ticks: ChecklistTickRecord[];
    };

/** File an adopted item: new, or replacing an entry filed from a bundle. */
function adoptInto(c: ConversationContent, adopted: ContentEntry): ConversationContent {
  const key = String(adopted.messageId);
  const have = c.entries[key];
  if (have === undefined) return withEntry(c, adopted);
  if (have.fromSnapshot !== true) return c;
  return { ...c, entries: { ...c.entries, [key]: replacingSnapshot(have, adopted) } };
}

async function fileAdopted(
  groupIdB64: string,
  item: AdoptedItem,
  ringB64: string | undefined,
  createdAt: string,
): Promise<ApplicationEvent | null> {
  const messageId = Number(item.pollRef);
  let content: MessageContent;
  if (item.kind === "poll") {
    content = { kind: "poll", poll: item.poll };
    await updateContent(groupIdB64, (c) =>
      adoptInto(c, {
        kind: "poll",
        messageId,
        createdAtMs: Date.parse(createdAt) || 0,
        authorKeyB64: item.author,
        signed: item.poll,
        verified: true,
        ...(ringB64 === undefined ? {} : { ringB64 }),
        votes: item.votes,
        closes: [],
        ballots: [],
      }),
    );
  } else {
    content = { kind: "checklist", checklist: item.checklist };
    await updateContent(groupIdB64, (c) =>
      adoptInto(c, {
        kind: "checklist",
        messageId,
        createdAtMs: Date.parse(createdAt) || 0,
        authorKeyB64: item.author,
        signed: item.checklist,
        verified: true,
        edits: [],
        adds: item.adds,
        ticks: item.ticks,
        closes: [],
      }),
    );
  }
  return {
    kind: "application",
    messageId,
    text: "",
    attachments: [],
    plaintext: "",
    senderLeaf: undefined,
    senderKey: item.author,
    createdAt,
    content,
  };
}

/** Old ballot rows the committer stored: filed under their poll, ordered
 *  before every live row (they are older than the journal retention). */
async function adoptOldBallots(wasm: Wasm, groupId: Uint8Array, groupIdB64: string, envelope: Uint8Array): Promise<void> {
  const decoded = wasm.decodePollBallotsEnvelope(envelope);
  try {
    const pollRef = Number(decoded.pollRef);
    const rows = decoded.rows;
    const content = await loadContent(groupIdB64);
    const entry = content.entries[String(pollRef)];
    if (entry === undefined || entry.kind !== "poll" || entry.ringB64 === undefined) return;
    const anon = entry.signed.body.anonymous;
    if (anon == null) return;
    const keys = new wasm.PollBallotKeys(groupId, base64Decode(entry.signed.body.id), base64Decode(anon.secret));
    const ring = wasm.PollRing.decode(base64Decode(entry.ringB64));
    try {
      let next: PollEntry = entry;
      rows.forEach((row, i) => {
        let verified: { tag: string; choices: number[]; version: number } | null = null;
        try {
          const v = wasm.openAndVerifyPollBallot(keys, ring, row);
          try {
            verified = { tag: base64Encode(v.tag), choices: Array.from(v.choices), version: v.version };
          } finally {
            v.free();
          }
        } catch {
          verified = null;
        }
        if (verified === null) return;
        next = withBallotRow(next, { messageId: -(rows.length - i), createdAtMs: 0, rowB64: base64Encode(row), verified });
      });
      const final = next;
      await updateContent(groupIdB64, (c) => ({ ...c, entries: { ...c.entries, [String(pollRef)]: final } }));
    } finally {
      keys.free();
      ring.free();
    }
  } finally {
    decoded.free();
  }
}

/** Test seam: the roster keys helper, re-exported for the handover tests. */
export { rosterAccountKeys };

/**
 * After an admission commit this device won: post the hand-over rows, sealed
 * at the new epoch (the publish is the conversation's quiet send). Failures
 * are logged; the items reach the newcomer at the next admission at worst.
 */
export async function sendOpenItemsHandover(
  groupIdB64: string,
  addedLeaves: number,
  publish: (row: Uint8Array) => Promise<unknown>,
): Promise<void> {
  try {
    const rows = await planHandoverRows(groupIdB64, addedLeaves, Date.now());
    for (const row of rows) await publish(row);
  } catch (err) {
    console.warn("[attach] open items not handed over", err instanceof Error ? err.name : "unknown");
  }
}
