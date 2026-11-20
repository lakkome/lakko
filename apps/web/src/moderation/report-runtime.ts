// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0099 section 2 - the production ports of `report-send.ts`: the pinned
 * root key, the moderation service, the wasm core, the `@name` lookup and
 * the evidence stored with each message at receipt.
 *
 * Evidence is read from the conversation view first when it is open (its
 * rows are newer than the log, which is written behind it), then from the
 * message log.
 */

import { getDeviceKey } from "../auth/key-store";
import { groupIdHex } from "../devices/sync/state";
import { lookupKeyForName } from "../discovery/handle-lookup";
import { loadMessageLog, type PersistedMessage } from "../chat/message-log-store";
import { attachmentsPreviewLabel } from "../chat/list-preview";
import { base64Decode } from "../auth/account-id";
import { loadWasm, type Wasm } from "../wasm";
import { evidenceFromPersisted, type MessageEvidence } from "./attestation";
import {
  fetchModeratorList,
  pinnedModerationRootKey,
  postReport,
  readListFloor,
  rememberModeratorList,
  writeListFloor,
} from "./report-api";
import type { PickCandidate, PickSource } from "./report-model";
import type { ReportSendPorts, SealSpec } from "./report-send";

/** What an open conversation view hands the report sheet. */
export interface LiveConversation {
  readonly evidence: (messageId: number) => MessageEvidence | undefined;
  readonly candidates: () => readonly PickCandidate[];
}

/** What an open channel view hands the report sheet. */
export interface LivePosts {
  readonly post: (cursor: number) => Uint8Array | undefined;
  readonly candidates: () => readonly PickCandidate[];
}

const live = new Map<string, LiveConversation>();
const livePosts = new Map<string, LivePosts>();

/** A conversation view registers itself while it is mounted; the returned
 *  function removes it. */
export function registerLiveConversation(groupIdB64: string, view: LiveConversation): () => void {
  live.set(groupIdB64, view);
  return () => {
    if (live.get(groupIdB64) === view) live.delete(groupIdB64);
  };
}

/** A channel view registers itself while it is mounted; the returned
 *  function removes it. */
export function registerLivePosts(channelIdHex: string, view: LivePosts): () => void {
  livePosts.set(channelIdHex, view);
  return () => {
    if (livePosts.get(channelIdHex) === view) livePosts.delete(channelIdHex);
  };
}

/** The stored rows of one conversation, `null` when there is no log. */
async function logRows(accountId: string, groupIdB64: string): Promise<readonly PersistedMessage[] | null> {
  const deviceKey = await getDeviceKey(accountId);
  if (deviceKey === null) return null;
  const log = await loadMessageLog(accountId, deviceKey);
  if (log === null) return null;
  let hex: string;
  try {
    hex = groupIdHex(base64Decode(groupIdB64));
  } catch {
    return null;
  }
  return log.groups[hex]?.messages ?? [];
}

/**
 * The messages or posts the "Choose messages" list offers for `source`, from
 * the open view when there is one, else from the message log (a person's
 * card, a request). A channel not open here offers none. Not yet filtered
 * (`pickCandidatesFor` is the rule).
 */
export async function pickCandidates(accountId: string | null, source: PickSource): Promise<readonly PickCandidate[]> {
  if (source.kind === "channel") return livePosts.get(source.channelIdHex)?.candidates() ?? [];
  const view = live.get(source.groupIdB64);
  if (view !== undefined) return view.candidates();
  if (accountId === null) return [];
  const rows = await logRows(accountId, source.groupIdB64);
  if (rows === null) return [];
  const out: PickCandidate[] = [];
  for (const m of rows) {
    if (m.direction !== "incoming") continue;
    const id = Number(m.messageId);
    if (!Number.isFinite(id)) continue;
    out.push({
      id,
      authorKeyB64: typeof m.authorKeyB64 === "string" ? m.authorKeyB64 : undefined,
      text: m.text,
      filesLabel: attachmentsPreviewLabel(m.attachments),
      createdAt: m.createdAt,
      deleted: m.deleted === true || typeof m.unsupportedVersion === "number",
    });
  }
  return out;
}

/** A message's evidence from the message log of `accountId`. */
async function evidenceFromLog(
  accountId: string,
  groupIdB64: string,
  messageId: number,
): Promise<MessageEvidence | undefined> {
  const rows = await logRows(accountId, groupIdB64);
  const row = rows?.find((m) => m.messageId === String(messageId));
  if (row === undefined || row.direction !== "incoming" || row.deleted === true) return undefined;
  return evidenceFromPersisted(row.evidence);
}

/** Seal through the wasm `ReportBuilder`. */
export function sealWith(wasm: Wasm, spec: SealSpec, list: Uint8Array, rootKey: Uint8Array): Uint8Array {
  const builder = new wasm.ReportBuilder(spec.targetKind, spec.target, spec.reason, spec.conversationKind, spec.comment);
  try {
    for (const m of spec.messages) builder.addMessage(m.groupId, m.plaintext, m.leafKey, m.binding);
    for (const f of spec.fixed) {
      if (f.kind === "contact-request") builder.addContactRequest(f.wire);
      else if (f.kind === "listing") builder.addListing(f.wire);
      else {
        const id = new Uint8Array(f.channelIdHex.length / 2);
        for (let i = 0; i < id.length; i += 1) id[i] = Number.parseInt(f.channelIdHex.slice(i * 2, i * 2 + 2), 16);
        builder.addChannelPost(id, f.post);
      }
    }
    return builder.seal(list, rootKey);
  } finally {
    builder.free();
  }
}

/** The production ports, for the account `accountId` (`null`: no session,
 *  so no log to read). */
export async function reportPorts(accountId: string | null): Promise<ReportSendPorts> {
  const wasm = await loadWasm();
  return {
    rootKey: pinnedModerationRootKey,
    fetchList: async () => {
      const list = await fetchModeratorList();
      // Kept for the standing proofs (F2), which verify it again.
      if (list !== null) rememberModeratorList(list);
      return list;
    },
    readFloor: readListFloor,
    writeFloor: writeListFloor,
    listVersion: (list, rootKey) => {
      const verified = wasm.verifyModeratorList(list, rootKey);
      try {
        return verified.listVersion;
      } finally {
        verified.free();
      }
    },
    accountTarget: (key) => wasm.moderationAccountTarget(key),
    groupLookupKey: (handle) => lookupKeyForName(handle),
    evidenceFor: async (groupIdB64, messageId) => {
      const fromView = live.get(groupIdB64)?.evidence(messageId);
      if (fromView !== undefined) return fromView;
      if (accountId === null) return undefined;
      return evidenceFromLog(accountId, groupIdB64, messageId);
    },
    channelPostFor: (channelIdHex, cursor) => livePosts.get(channelIdHex)?.post(cursor),
    seal: (spec, list, rootKey) => sealWith(wasm, spec, list, rootKey),
    post: (sealed) => postReport(sealed),
  };
}
