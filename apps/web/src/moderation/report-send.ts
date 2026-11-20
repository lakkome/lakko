// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0089 section 2 - preparing and sending one report. The order is the
 * sheet's contract (UX 2.3):
 *
 *   1. the pinned root key; none -> reporting is not available here;
 *   2. the moderator list (`GET /moderation/moderators`); 503 -> not
 *      available; a list below the highest version this device has seen ->
 *      not available; no answer -> the network failure;
 *   3. the target bytes (a person's account target, a group's `@name` lookup
 *      key, a channel id);
 *   4. the evidence: each picked message as stored at receipt (one whose
 *      bytes are gone is left out), then the fixed items;
 *   5. seal under the list, post with one `report` token.
 *
 * Every step is a port, so the order is tested without a network or wasm.
 */

import { base64Decode } from "../auth/account-id";
import { evidenceBytes, type MessageEvidence } from "./attestation";
import {
  acceptListVersion,
  TARGET_ACCOUNT,
  TARGET_CHANNEL,
  TARGET_GROUP,
  type FixedEvidence,
  type ReportDraft,
  type SendOutcome,
} from "./report-model";

/** One message as the report carries it. */
export interface MessageItem {
  readonly groupId: Uint8Array;
  readonly plaintext: Uint8Array;
  readonly leafKey: Uint8Array;
  readonly binding: Uint8Array;
}

/** Everything the core seals. */
export interface SealSpec {
  readonly targetKind: number;
  readonly target: Uint8Array;
  readonly reason: number;
  readonly conversationKind: number;
  readonly comment: string;
  readonly messages: readonly MessageItem[];
  readonly fixed: readonly FixedEvidence[];
}

/** The steps, injectable. */
export interface ReportSendPorts {
  readonly rootKey: () => Uint8Array | null;
  /** The list bytes, `null` for a 503; throws when there is no answer. */
  readonly fetchList: () => Promise<Uint8Array | null>;
  readonly readFloor: () => bigint | null;
  readonly writeFloor: (version: bigint) => void;
  /** The list's version under the root key; throws for a list that does not
   *  verify. */
  readonly listVersion: (list: Uint8Array, rootKey: Uint8Array) => bigint;
  readonly accountTarget: (accountKey: Uint8Array) => Uint8Array;
  /** The lookup key of a group's `@name`; throws when it cannot be had. */
  readonly groupLookupKey: (handle: string) => Promise<Uint8Array | null>;
  /** The evidence of a message as stored at receipt, if it is still here. */
  readonly evidenceFor: (groupIdB64: string, messageId: number) => Promise<MessageEvidence | undefined>;
  /** A channel post's wire as fetched, if this page still holds it. */
  readonly channelPostFor: (channelIdHex: string, cursor: number) => Uint8Array | undefined;
  readonly seal: (spec: SealSpec, list: Uint8Array, rootKey: Uint8Array) => Uint8Array;
  readonly post: (sealed: Uint8Array) => Promise<SendOutcome>;
}

/** The picked channel posts this page still holds, as fixed items. */
export function preparedPosts(
  draft: ReportDraft,
  channelPostFor: ReportSendPorts["channelPostFor"],
): FixedEvidence[] {
  const source = draft.pickFrom;
  if (source === null || source.kind !== "channel") return [];
  const out: FixedEvidence[] = [];
  for (const cursor of draft.picked) {
    const post = channelPostFor(source.channelIdHex, cursor);
    if (post !== undefined) out.push({ kind: "channel-post", channelIdHex: source.channelIdHex, post });
  }
  return out;
}

/** How many picks can be carried (messages with their evidence, posts this
 *  page holds). */
export async function availablePicks(
  draft: ReportDraft,
  ports: Pick<ReportSendPorts, "evidenceFor" | "channelPostFor">,
): Promise<number> {
  if (draft.pickFrom?.kind === "channel") return preparedPosts(draft, ports.channelPostFor).length;
  return (await preparedMessages(draft, ports.evidenceFor)).length;
}

/** The picked messages whose evidence is still on this device. */
export async function preparedMessages(
  draft: ReportDraft,
  evidenceFor: ReportSendPorts["evidenceFor"],
): Promise<MessageItem[]> {
  const source = draft.pickFrom;
  if (source === null || source.kind !== "chat") return [];
  let groupId: Uint8Array;
  try {
    groupId = base64Decode(source.groupIdB64);
  } catch {
    return [];
  }
  const out: MessageItem[] = [];
  for (const id of draft.picked) {
    let evidence: MessageEvidence | undefined;
    try {
      evidence = await evidenceFor(source.groupIdB64, id);
    } catch {
      evidence = undefined;
    }
    if (evidence === undefined) continue;
    const bytes = evidenceBytes(evidence);
    if (bytes === null || bytes.leafKey.length !== 32) continue;
    out.push({ groupId, plaintext: bytes.plaintext, leafKey: bytes.leafKey, binding: bytes.binding });
  }
  return out;
}

/** Hex (lower or upper case, even length) to bytes, or `null`. */
function hexBytes(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** Send one report. Never throws: every failure is an outcome. */
export async function sendReport(draft: ReportDraft, ports: ReportSendPorts): Promise<SendOutcome> {
  if (draft.reason === null) return { kind: "failed" };
  const rootKey = ports.rootKey();
  if (rootKey === null) return { kind: "unavailable" };

  let list: Uint8Array | null;
  try {
    list = await ports.fetchList();
  } catch {
    return { kind: "failed" };
  }
  if (list === null) return { kind: "unavailable" };
  let version: bigint;
  try {
    version = ports.listVersion(list, rootKey);
  } catch {
    return { kind: "unavailable" };
  }
  const floor = acceptListVersion(ports.readFloor(), version);
  if (!floor.accept) return { kind: "unavailable" };
  ports.writeFloor(floor.floor);

  let targetKind: number;
  let target: Uint8Array;
  try {
    const t = draft.target;
    if (t.kind === "person") {
      targetKind = TARGET_ACCOUNT;
      target = ports.accountTarget(base64Decode(t.accountKeyB64));
    } else if (t.kind === "group") {
      targetKind = TARGET_GROUP;
      const key = await ports.groupLookupKey(t.handle);
      if (key === null) return { kind: "failed" };
      target = key;
    } else {
      targetKind = TARGET_CHANNEL;
      const id = hexBytes(t.channelIdHex);
      if (id === null || id.length !== 16) return { kind: "failed" };
      target = id;
    }
  } catch {
    return { kind: "failed" };
  }

  const messages = await preparedMessages(draft, ports.evidenceFor);
  let sealed: Uint8Array;
  try {
    sealed = ports.seal(
      {
        targetKind,
        target,
        reason: draft.reason,
        conversationKind: draft.conversationKind,
        comment: draft.comment,
        messages,
        fixed: [...preparedPosts(draft, ports.channelPostFor), ...draft.fixed],
      },
      list,
      rootKey,
    );
  } catch {
    return { kind: "failed" };
  }
  return ports.post(sealed);
}
