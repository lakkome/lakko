// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0090 — what the chat receive loop (`chat/client.ts`) does with a poll,
 * a checklist, the ops on them, an anonymous ballot row, a commit and an
 * open-items hand-over. Called under the per-group MLS lock, at receipt, so
 * every attribution reads the roster of the epoch the row was sealed at
 * (C-68): the author of a poll, the voter of a public vote, whether a closer
 * is an owner or admin, version 0 of an anonymous poll's ring.
 *
 * Every refusal is a silent drop (fail-closed): a poll whose author signature
 * or ring does not verify is filed `verified: false` and shows no totals and
 * takes no vote; an op that does not fit its target is ignored by the fold.
 */
import { base64Decode, base64Encode } from "../auth/account-id";
import { loadWasm } from "../wasm";
import type { ContentOp } from "../chat/attachments";
import { persistedFromAttachmentRef } from "../chat/persisted-attachment";
import type { MessageContent, SignedChecklist } from "../../wasm/lakko_ffi_wasm";
import {
  compareKeyB64,
  loadContent,
  ringAdditions,
  updateContent,
  withArticleEdit,
  withBallotRow,
  withEntry,
  withOp,
  type BallotRow,
  type ConversationContent,
  type HeldOp,
  type PollEntry,
} from "./content-store";

type Wasm = Awaited<ReturnType<typeof loadWasm>>;

/** The group surface this needs, structurally (a wasm `Group`). */
export interface ReceiveGroup {
  readonly groupId: Uint8Array;
  members(): unknown[];
  groupKind?(): unknown;
  roleTableOwner?(): Uint8Array | undefined;
  roleTableAdmins?(): Uint8Array;
}

/** The distinct account keys of the roster, standard base64. A member whose
 *  leaf carries no binding counts by its leaf key (Phase 1). */
export function rosterAccountKeys(group: ReceiveGroup): string[] {
  let members: unknown[];
  try {
    members = group.members();
  } catch {
    return [];
  }
  const out = new Set<string>();
  for (const m of members as { accountKey?: Uint8Array; signaturePublicKey?: Uint8Array }[]) {
    const key = m.accountKey ?? m.signaturePublicKey;
    if (key instanceof Uint8Array && key.length === 32) out.add(base64Encode(key));
  }
  return [...out].sort(compareKeyB64);
}

/** Whether `keyB64` is the owner or an admin of the live role table. */
export function isOwnerOrAdmin(group: ReceiveGroup, keyB64: string): boolean {
  try {
    const owner = group.roleTableOwner?.();
    if (owner !== undefined && base64Encode(owner) === keyB64) return true;
    const flat = group.roleTableAdmins?.() ?? new Uint8Array(0);
    for (let i = 0; i + 32 <= flat.length; i += 32) {
      if (base64Encode(flat.slice(i, i + 32)) === keyB64) return true;
    }
  } catch {
    return false;
  }
  return false;
}

function flatKeys(keysB64: readonly string[]): Uint8Array {
  const out = new Uint8Array(keysB64.length * 32);
  keysB64.forEach((k, i) => out.set(base64Decode(k), i * 32));
  return out;
}

function msOf(createdAt: string): number {
  const ms = Date.parse(createdAt);
  return Number.isFinite(ms) ? ms : 0;
}

function isOneToOne(wasm: Wasm, group: ReceiveGroup): boolean {
  try {
    return group.groupKind?.() === wasm.GroupKind.OneToOne;
  } catch {
    return false;
  }
}

/**
 * A poll or checklist message: file it with its author and, for an anonymous
 * poll, version 0 of its ring from this roster. A location, contact card or
 * article has nothing to file here (its bubble is the message itself).
 */
export async function receiveContent(
  group: ReceiveGroup,
  groupIdB64: string,
  messageId: number,
  createdAt: string,
  senderKeyB64: string | undefined,
  content: MessageContent,
): Promise<void> {
  if (senderKeyB64 === undefined) return;
  if (content.kind !== "poll" && content.kind !== "checklist") return;
  const wasm = await loadWasm();
  const author = base64Decode(senderKeyB64);
  if (content.kind === "checklist") {
    let verified = true;
    try {
      wasm.verifyChecklist(content.checklist, group.groupId, author);
    } catch {
      verified = false;
    }
    await updateContent(groupIdB64, (c) =>
      withEntry(c, {
        kind: "checklist",
        messageId,
        createdAtMs: msOf(createdAt),
        authorKeyB64: senderKeyB64,
        signed: content.checklist,
        verified,
        edits: [],
        adds: [],
        ticks: [],
        closes: [],
      }),
    );
    return;
  }
  const poll = content.poll;
  let verified = true;
  try {
    wasm.verifyPoll(poll, group.groupId, author);
  } catch {
    verified = false;
  }
  let ringB64: string | undefined;
  if (poll.body.anonymous != null) {
    // An anonymous poll is refused in a one-to-one (ADR-0090 section 3.1).
    if (isOneToOne(wasm, group)) verified = false;
    const keys = rosterAccountKeys(group);
    try {
      const hash = wasm.pollRingHash(flatKeys(keys));
      if (base64Encode(hash) !== poll.body.anonymous.ring0Hash) verified = false;
      const ring = new wasm.PollRing(flatKeys(keys));
      try {
        ringB64 = base64Encode(ring.encode());
      } finally {
        ring.free();
      }
    } catch {
      verified = false;
    }
  }
  await updateContent(groupIdB64, (c) =>
    withEntry(c, {
      kind: "poll",
      messageId,
      createdAtMs: msOf(createdAt),
      authorKeyB64: senderKeyB64,
      signed: poll,
      verified,
      ...(ringB64 === undefined ? {} : { ringB64 }),
      votes: [],
      closes: [],
      ballots: [],
    }),
  );
}

/** One op from a member, attributed at receipt. */
export async function receiveContentOp(
  group: ReceiveGroup,
  groupIdB64: string,
  messageId: number,
  createdAt: string,
  senderKeyB64: string | undefined,
  op: ContentOp,
): Promise<void> {
  if (senderKeyB64 === undefined) return;
  if (op.op === "edit-article") {
    // Shown only where the editor is the article's author (checked by the
    // bubble, which knows the author).
    const edit = {
      messageId,
      editor: senderKeyB64,
      article: op.article,
      attachments: op.attachments.map((a) => persistedFromAttachmentRef(a)),
    };
    await updateContent(groupIdB64, (c) => withArticleEdit(c, op.target, edit));
    return;
  }
  const held = heldOpOf(op, messageId, msOf(createdAt), senderKeyB64, isOwnerOrAdmin(group, senderKeyB64));
  if (held === null) return;
  let signedEdit: SignedChecklist | undefined;
  if (op.op === "edit-checklist") {
    // Kept only when the author signed it: it is what a hand-over passes on.
    const wasm = await loadWasm();
    try {
      wasm.verifyChecklist(op.checklist, group.groupId, base64Decode(senderKeyB64));
      signedEdit = op.checklist;
    } catch {
      signedEdit = undefined;
    }
  }
  await updateContent(groupIdB64, (c) => {
    const next = withOp(c, held);
    const key = String(op.target);
    const e = next.entries[key];
    if (signedEdit === undefined || e === undefined || e.kind !== "checklist" || e.authorKeyB64 !== senderKeyB64) {
      return next;
    }
    return { ...next, entries: { ...next.entries, [key]: { ...e, currentSigned: signedEdit } } };
  });
}

/** The stored record of one op (shared with the local apply of own ops). */
export function heldOpOf(
  op: ContentOp,
  messageId: number,
  createdAtMs: number,
  senderKeyB64: string,
  senderIsAdmin: boolean,
): HeldOp | null {
  switch (op.op) {
    case "poll-vote":
      return {
        kind: "vote",
        target: op.target,
        record: { messageId, createdAtMs, voter: senderKeyB64, choices: [...op.choices] },
      };
    case "poll-close":
      return {
        kind: "close",
        target: op.target,
        record: { messageId, closer: senderKeyB64, closerIsAdmin: senderIsAdmin },
      };
    case "checklist-tick":
      return {
        kind: "tick",
        target: op.target,
        record: { messageId, ticker: senderKeyB64, privileged: false, item: op.item, done: op.done },
      };
    case "checklist-add":
      return {
        kind: "add",
        target: op.target,
        record: { messageId, adder: senderKeyB64, privileged: false, text: op.text },
      };
    case "edit-checklist":
      return {
        kind: "edit",
        target: op.target,
        record: { messageId, editor: senderKeyB64, body: op.checklist.body },
      };
    case "edit-article":
      // An article edit replaces the message itself; the chat overlays it.
      return null;
  }
}

/**
 * An anonymous ballot row (never an MLS message): find the open anonymous
 * poll whose secret opens it, verify it against the ring version it names,
 * and file it. A row no held poll opens is dropped; one naming a ring version
 * not held yet is kept unverified and retried after the next commit.
 */
export async function receiveBallotRow(
  groupId: Uint8Array,
  groupIdB64: string,
  messageId: number,
  createdAt: string,
  row: Uint8Array,
): Promise<void> {
  const wasm = await loadWasm();
  const content = await loadContent(groupIdB64);
  for (const entry of Object.values(content.entries)) {
    if (entry.kind !== "poll" || entry.ringB64 === undefined) continue;
    const anon = entry.signed.body.anonymous;
    if (anon == null) continue;
    const verified = tryBallot(wasm, groupId, entry, row);
    if (verified === "not-this-poll") continue;
    const ballot: BallotRow = {
      messageId,
      createdAtMs: msOf(createdAt),
      rowB64: base64Encode(row),
      ...(verified === "unknown-version" ? {} : { verified }),
    };
    await updateContent(groupIdB64, (c) => {
      const e = c.entries[String(entry.messageId)];
      if (e === undefined || e.kind !== "poll") return c;
      return { ...c, entries: { ...c.entries, [String(entry.messageId)]: withBallotRow(e, ballot) } };
    });
    return;
  }
}

type BallotVerdict = "not-this-poll" | "unknown-version" | null | { tag: string; choices: number[]; version: number };

function tryBallot(wasm: Wasm, groupId: Uint8Array, entry: PollEntry, row: Uint8Array): BallotVerdict {
  const anon = entry.signed.body.anonymous;
  if (anon == null || entry.ringB64 === undefined) return "not-this-poll";
  const keys = new wasm.PollBallotKeys(groupId, base64Decode(entry.signed.body.id), base64Decode(anon.secret));
  try {
    let opened;
    try {
      opened = wasm.openPollBallotRow(keys, row);
    } catch {
      return "not-this-poll";
    }
    const ring = wasm.PollRing.decode(base64Decode(entry.ringB64));
    try {
      if (ring.findVersion(opened.ringHash) === undefined) return "unknown-version";
      const v = wasm.verifyPollBallot(keys, ring, opened);
      try {
        return { tag: base64Encode(v.tag), choices: Array.from(v.choices), version: v.version };
      } finally {
        v.free();
      }
    } catch {
      return null;
    } finally {
      ring.free();
      opened.free();
    }
  } finally {
    keys.free();
  }
}

/**
 * A commit merged: extend the ring of every open anonymous poll by the
 * accounts it added, then retry the ballots that named a version not held.
 */
export async function receiveCommit(group: ReceiveGroup, groupIdB64: string, nowMs: number): Promise<void> {
  // Nothing to grow in a conversation with no anonymous poll (the common
  // case): the core is not even loaded.
  const held = await loadContent(groupIdB64);
  if (!Object.values(held.entries).some((e) => e.kind === "poll" && e.ringB64 !== undefined)) return;
  const wasm = await loadWasm();
  const roster = rosterAccountKeys(group);
  await updateContent(groupIdB64, (c) => extendRings(wasm, group.groupId, c, roster, nowMs));
}

function extendRings(
  wasm: Wasm,
  groupId: Uint8Array,
  c: ConversationContent,
  roster: readonly string[],
  nowMs: number,
): ConversationContent {
  let changed = false;
  const entries = { ...c.entries };
  for (const [key, entry] of Object.entries(c.entries)) {
    if (entry.kind !== "poll" || entry.ringB64 === undefined) continue;
    if (entry.closes.length > 0) continue;
    const closeAt = entry.signed.body.closeAtMs;
    if (closeAt != null && nowMs >= closeAt) continue;
    const ring = wasm.PollRing.decode(base64Decode(entry.ringB64));
    try {
      const added = ringAdditions((k) => ring.contains(base64Decode(k)), roster);
      let next: PollEntry = entry;
      if (added.length > 0) {
        ring.extend(flatKeys(added));
        next = { ...next, ringB64: base64Encode(ring.encode()) };
      }
      // Retry ballots that named a version this device did not hold.
      if (next.ballots.some((b) => b.verified === undefined)) {
        const ballots = next.ballots.map((b) => {
          if (b.verified !== undefined) return b;
          const v = tryBallot(wasm, groupId, next, base64Decode(b.rowB64));
          if (v === "unknown-version") return b;
          return { ...b, verified: v === "not-this-poll" ? null : v };
        });
        next = { ...next, ballots };
      }
      if (next !== entry) {
        entries[key] = next;
        changed = true;
      }
    } finally {
      ring.free();
    }
  }
  return changed ? { ...c, entries } : c;
}
