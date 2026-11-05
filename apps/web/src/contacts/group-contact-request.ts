// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A contact request sent inside a group both people are in
 * (`docs/ux/who-can-find-me.md` section 6.4; ADR-0086 section 6), receive
 * side.
 *
 * The chat receive path (the open chat's poll, the background poll, the
 * committer sweep) consumes the envelope at receipt and hands it here with the
 * account key its MLS sender resolved to at receipt (C-68). The runtime that
 * owns the request list installs the sink. Every member's device tries the
 * envelope; it opens only for the person it is sealed to, and every other
 * device drops it without a trace.
 */

import type { IncomingContactRequest } from "./client";

/** One envelope as the receive path hands it on. */
export interface ArrivedGroupContactRequest {
  readonly groupIdB64: string;
  readonly groupId: Uint8Array;
  /** The journal row's id (the group's id space, not the inbox's). */
  readonly journalMessageId: number;
  readonly createdAt: string;
  /** The whole envelope. */
  readonly plaintext: Uint8Array;
  /** The account key the carrying message's MLS sender resolved to at
   *  receipt; `undefined` when it could not be resolved (dropped). */
  readonly senderAccountKey: Uint8Array | undefined;
}

/** `true`: handled for good (listed, answered, or deliberately dropped).
 *  `false`: not handled yet (no session ready, or the request could not be
 *  written) and must be tried again. */
export type GroupContactRequestSink = (arrived: ArrivedGroupContactRequest) => Promise<boolean>;

let sink: GroupContactRequestSink | null = null;

/** Envelopes a sink could not handle yet, tried again on the next delivery
 *  and when a sink is installed. In memory: across a reload the group
 *  journal row is read again, because the receive path held its cursor. */
const unhandled: ArrivedGroupContactRequest[] = [];
const MAX_UNHANDLED = 64;

/** Install (or, with `null`, remove) the handler the runtime owns. */
export function installGroupContactRequestSink(next: GroupContactRequestSink | null): void {
  sink = next;
  if (next !== null) void retryUnhandled();
}

async function handle(arrived: ArrivedGroupContactRequest): Promise<boolean> {
  const current = sink;
  if (current === null) return false;
  try {
    return await current(arrived);
  } catch {
    // eslint-disable-next-line no-console
    console.warn("[contacts] a contact request from a group could not be handled yet");
    return false;
  }
}

function keepUnhandled(arrived: ArrivedGroupContactRequest): void {
  const same = (a: ArrivedGroupContactRequest): boolean =>
    a.groupIdB64 === arrived.groupIdB64 && a.journalMessageId === arrived.journalMessageId;
  if (unhandled.some(same)) return;
  if (unhandled.length >= MAX_UNHANDLED) unhandled.shift();
  unhandled.push(arrived);
}

async function retryUnhandled(): Promise<void> {
  const waiting = unhandled.splice(0, unhandled.length);
  for (const arrived of waiting) {
    if (!(await handle(arrived))) keepUnhandled(arrived);
  }
}

/**
 * Hand one envelope on. Never throws. Returns whether it was handled for
 * good; when it was not, it is kept in memory and tried again, and the caller
 * (the journal poll) holds its cursor so a reload reads the row again: an
 * opened request is never lost between the decrypt and its write.
 */
export async function deliverGroupContactRequest(arrived: ArrivedGroupContactRequest): Promise<boolean> {
  await retryUnhandled();
  const done = await handle(arrived);
  if (!done) keepUnhandled(arrived);
  return done;
}

/** Tests: forget the envelopes kept for a retry. */
export function resetUnhandledForTest(): void {
  unhandled.splice(0, unhandled.length);
}

/**
 * Section 6.4: what the person's device does with a request that opened and
 * passed the setting filter. A sender this device already talks to in a
 * one-to-one is answered silently, as a request from a contact through the
 * inbox is (ADR-0073 R.7 case 1). A contact with NO one-to-one here (first
 * met in a group) is the very case the request is for: it becomes a Requests
 * row whose Accept founds the one-to-one; answering it silently would send
 * an ack with no chat behind it, and the sender's next Message would ask
 * again, forever. Anyone else: listed.
 */
export function groupRequestHandling(facts: {
  readonly senderIsContact: boolean;
  readonly holdsOneToOne: boolean;
}): "answer-silently" | "list" {
  return facts.senderIsContact && facts.holdsOneToOne ? "answer-silently" : "list";
}

/**
 * The id a request from a group is listed under. Requests are keyed by
 * message id, and a group's journal ids share no space with the inbox's, so a
 * group request takes a NEGATIVE id derived from the group and the journal
 * row: deterministic (a row fetched again maps to the same id and is not
 * listed twice) and never equal to an inbox id.
 */
export function groupRequestRowId(groupIdB64: string, journalMessageId: number): number {
  // FNV-1a over the group id, 31 bits.
  let h = 0x811c9dc5;
  for (let i = 0; i < groupIdB64.length; i += 1) {
    h ^= groupIdB64.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  const low = Math.abs(Math.trunc(journalMessageId)) % 2 ** 21;
  return -((h & 0x7fffffff) * 2 ** 21 + low + 1);
}

/** What the person's device does with an opened request. */
export type GroupRequestVerdict = "show" | "drop";

/**
 * Section 6.4, "When the person does not want it": shown only when the
 * person's own setting lets the members of THAT group find them, or when the
 * sender is already their contact; otherwise dropped without a trace.
 */
export function groupRequestVerdict(facts: {
  readonly allowsGroupMembers: boolean;
  readonly senderIsContact: boolean;
}): GroupRequestVerdict {
  return facts.allowsGroupMembers || facts.senderIsContact ? "show" : "drop";
}

/**
 * The same request already held: one arrived through the inbox and through a
 * group (the same random per-request pseudonym from the same key), or a
 * request through a group from a person whose request is already listed (one
 * request per person).
 */
export function duplicateOfHeld(
  incoming: Pick<IncomingContactRequest, "fromPubkey" | "fromPseudonym" | "viaGroupIdB64">,
  held: readonly Pick<IncomingContactRequest, "fromPubkey" | "fromPseudonym">[],
): boolean {
  const sameKey = (a: Uint8Array, b: Uint8Array): boolean =>
    a.length === b.length && a.every((v, i) => v === b[i]);
  return held.some(
    (h) =>
      sameKey(h.fromPubkey, incoming.fromPubkey) &&
      (incoming.viaGroupIdB64 !== undefined || sameKey(h.fromPseudonym, incoming.fromPseudonym)),
  );
}

/** What {@link openArrivedGroupRequest} needs. */
export interface OpenArrivedDeps {
  /** The core's receiver rule (`checkGroupContactRequest`). `undefined`:
   *  not for this member. Throws: malformed, a mismatched sender; dropped. */
  check(plaintext: Uint8Array, x25519Secret: Uint8Array, senderAccountKey: Uint8Array): Uint8Array | undefined;
  /** This account's X25519 secret, or `null` when no identity is loaded. */
  readonly x25519Secret: Uint8Array | null;
  /** Verify and open the request wire, the way an inbox row is opened. */
  decode(wire: Uint8Array, rowId: number, createdAt: string, viaGroupIdB64: string): IncomingContactRequest | null;
  allowsGroupMembers(groupId: Uint8Array): boolean;
  isContact(signaturePubkeyB64: string): boolean;
  keyB64(bytes: Uint8Array): string;
}

/**
 * The request a group envelope carries for this account, or `null` when it
 * is to be dropped without a trace: not sealed to this account, malformed, a
 * sender that is not the member who posted it, or a person whose setting does
 * not let that group's members find them (unless the sender is already a
 * contact). Never throws; logs only a reason code, never who.
 */
export function openArrivedGroupRequest(
  arrived: ArrivedGroupContactRequest,
  deps: OpenArrivedDeps,
): { readonly request: IncomingContactRequest; readonly row: { messageId: number; payload: Uint8Array; createdAt: string; viaGroupIdB64: string } } | null {
  if (arrived.senderAccountKey === undefined || deps.x25519Secret === null) return null;
  let wire: Uint8Array | undefined;
  try {
    wire = deps.check(arrived.plaintext, deps.x25519Secret, arrived.senderAccountKey);
  } catch (err) {
    const code = err instanceof Error ? err.message.split(":")[0] : "unknown";
    // eslint-disable-next-line no-console
    console.warn(`[contacts] a contact request from a group was dropped: ${code}`);
    return null;
  }
  if (wire === undefined) return null;
  const rowId = groupRequestRowId(arrived.groupIdB64, arrived.journalMessageId);
  const request = deps.decode(wire, rowId, arrived.createdAt, arrived.groupIdB64);
  if (request === null) return null;
  const verdict = groupRequestVerdict({
    allowsGroupMembers: deps.allowsGroupMembers(arrived.groupId),
    senderIsContact: deps.isContact(deps.keyB64(request.fromPubkey)),
  });
  if (verdict === "drop") return null;
  return {
    request,
    row: { messageId: rowId, payload: wire, createdAt: arrived.createdAt, viaGroupIdB64: arrived.groupIdB64 },
  };
}
