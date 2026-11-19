// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * History catch-up bundle plumbing on web (Phase 2a3d8-ui).
 *
 * Two helpers wrap the wasm structured-bundle FFI:
 *
 *   * [`buildHistoryBundleFromLog`] — scanner side. Reads the
 *     local persistent message log + projects it into the
 *     `HistoryGroupInput` shape the wasm `sealHistoryBundleStructured`
 *     consumes. Returns `null` when there's nothing to send
 *     (no log on disk yet) so the orchestrator can skip the
 *     mailbox PUT.
 *
 *   * [`applyHistoryBundleToLog`] — joining side. Takes the
 *     opened `HistoryBundle`, projects each row back into the
 *     persistent message log under the receiving device's
 *     account key. Idempotent on the `messageId` dedup
 *     (the joiner's log was empty pre-pair; the bundle rows
 *     all land net-new).
 *
 * `payload_bytes` carry the already-decoded
 * `mls_app::v1::ApplicationPayload` blob from the original
 * receive path. Today we treat them as opaque on the receive
 * side — the receiving log stores them as-is; UI projection
 * back into `text + attachments` happens lazily when the chat
 * surface re-mounts and reads the log. A future sprint can
 * teach the orchestrator to do the projection eagerly via
 * the chat envelope parser; for the MVP the opaque round-trip
 * is enough to prove the catch-up path.
 */
import { contentFromPersisted } from "../attach/preview";
import { contentOf } from "../attach/content-store";
import { foldChecklistEntry, foldPollEntry } from "../attach/ContentBubble";
import type { ContentMessage, ContentState } from "../../wasm/lakko_ffi_wasm";
import type { DeviceCipher } from "../auth/device-cipher";
import { loadWasm } from "../wasm";
import type { Wasm } from "../wasm";
import type {
  HistoryAttachmentInput,
  HistoryGroupInput,
  HistoryMessageInput,
  OpenedHistoryAttachment,
  OpenedHistoryGroup,
  OpenedHistoryMessage,
  OpenedHistoryPlaceholder,
} from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { historyMediaFromPersisted, persistedMediaFromOpened } from "./history-media";
import { b64UrlEncode, inlineThumbFromPersisted } from "../chat/persisted-attachment";
import { isInlineThumbJpeg } from "../media/visual-media";
import { fileHistoryRows } from "../chat/background-log";
import {
  loadMessageLog,
  MESSAGE_LOG_VERSION,
  updateMessageLog,
  type PersistedAttachment,
  type PersistedGroup,
  type PersistedMessage,
} from "../chat/message-log-store";

/**
 * A bookkeeping system row: what a receive loop files for an MLS commit or
 * an external-join request (`[mls commit]`, `[mls external-join-proposal]`
 * on web, `[mls external join]` on iOS and Android: `[mls <kind>]` on every
 * client). A history bundle neither carries nor applies them: the sending
 * device never shows them as what they were (iOS and Android hide them, and
 * web cannot tell a commit that changed people from one that only admitted
 * or retired a device), and a joiner restores every such row as `Members
 * changed`. A join request that is still waiting is shown from the group's
 * own pending set, not from a row, so dropping it loses nothing. Call rows
 * are system rows too and are not these. Pure.
 */
export function isBookkeepingSystemRow(m: Pick<PersistedMessage, "direction" | "text">): boolean {
  return m.direction === "system" && m.text.startsWith("[mls ") && m.text.endsWith("]");
}

/** Hard cap on the total `HistoryMessage` count across every
 *  group in a single bundle. Must match
 *  `lakko_mls::history_bundle::MAX_BUNDLE_MESSAGES`. */
const MAX_BUNDLE_MESSAGES = 1000;

/**
 * Build the sealed history-bundle wire bytes from the
 * scanner-side device's persistent message log.
 *
 * Returns `null` when:
 *   - the log doesn't exist (fresh install — nothing to send),
 *   - the log decrypts to an empty groups map.
 *
 * Caller handles the `null` case by skipping the mailbox PUT;
 * the joining side's `pollForHistoryBundle` timeout fires
 * harmlessly.
 *
 * `secret` MUST be 32 bytes — typically the output of
 * `DeviceGroup.historyBundleSecret(identity)` after
 * `addDeviceFromWire` has advanced the DeviceGroup view to
 * the new epoch.
 */
export async function buildHistoryBundleFromLog(args: {
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  readonly secret: Uint8Array;
}): Promise<Uint8Array | null> {
  const log = await loadMessageLog(args.accountIdB64, args.deviceKey);
  if (log === null) return null;
  const groupHexes = Object.keys(log.groups);
  if (groupHexes.length === 0) return null;

  const wasm = await loadWasm();

  // Trim to MAX_BUNDLE_MESSAGES total across all groups,
  // preferring most-recent messages per group. Each group
  // retains at most `evenSplit` messages, with the remainder
  // re-distributed if a group has fewer to spare.
  const perGroupCap = Math.max(
    1,
    Math.floor(MAX_BUNDLE_MESSAGES / groupHexes.length),
  );

  const groupInputs: HistoryGroupInput[] = [];
  let maxHlc = 0n;

  for (const hex of groupHexes) {
    const slot = log.groups[hex];
    if (slot === undefined) continue;
    const groupIdBytes = hexToBytes(hex);
    if (groupIdBytes === null) continue;
    const carried = slot.messages.filter((m) => !isBookkeepingSystemRow(m));
    const trimmed = carried.slice(-perGroupCap);
    const capped = trimmed.length < carried.length;
    const messages: HistoryMessageInput[] = [];
    for (const m of trimmed) {
      // G-6 step 2: `payload_bytes` is now the canonical
      // cross-platform placeholder produced by the shared Rust
      // core (`encodeHistoryPlaceholder`), identical bytes to
      // what iOS / Android emit. The sibling `message_id` field
      // is the 8-byte big-endian u64 (the native convention),
      // replacing the old web-only UTF-8-of-decimal encoding so
      // a web-scanner -> native-joiner pairing interoperates.
      const placeholder = encodeMessagePlaceholder(wasm, m, base64Encode(groupIdBytes));
      const createdMs = (() => {
        if (m.createdAt === undefined || m.createdAt === null) return 0;
        const v = Date.parse(m.createdAt);
        return Number.isFinite(v) && v > 0 ? v : 0;
      })();
      messages.push(
        new wasm.HistoryMessageInput(
          placeholder,
          /* sender_leaf */ 0,
          messageIdToBeBytes(m.messageId),
          BigInt(createdMs),
        ),
      );
      const cur = safeBigInt(m.messageId);
      if (cur !== null && cur > maxHlc) maxHlc = cur;
    }
    groupInputs.push(new wasm.HistoryGroupInput(groupIdBytes, messages, capped));
  }

  if (groupInputs.length === 0) return null;

  return wasm.sealHistoryBundleStructured(
    args.secret,
    groupInputs,
    maxHlc,
  );
}

/**
 * Inverse — open the sealed bytes + project the rows into the
 * joining device's persistent message log under `accountIdB64`.
 *
 * Existing log rows survive: per-group inserts merge by
 * `messageId` (collisions take the on-disk value to avoid
 * stomping concurrent edits), and everything else the slot holds
 * stays. The write is serialized with every other writer of the
 * log: the background receive is running by now, and a bundle
 * written over what it filed meanwhile would lose rows that can
 * never be decrypted again.
 *
 * Each group's slot also gets its history floor, the highest id
 * the bundle carried for it: what came with the history is not
 * news (`fileHistoryRows`, docs/ux/second-device.md section 2).
 *
 * Returns the number of rows applied (across all groups), so
 * the orchestrator can surface a "+N messages" toast.
 */
export async function applyHistoryBundleToLog(args: {
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  readonly secret: Uint8Array;
  readonly sealedBytes: Uint8Array;
}): Promise<number> {
  const wasm = await loadWasm();
  const opened = wasm.openHistoryBundleStructured(args.secret, args.sealedBytes);
  const openedGroups = opened.groups() as unknown[];

  const bundled: {
    readonly hex: string;
    readonly rows: readonly PersistedMessage[];
    readonly newest: bigint | null;
  }[] = [];
  for (const g of openedGroups) {
    const group = g as OpenedHistoryGroup;
    const rows: PersistedMessage[] = [];
    let newest: bigint | null = null;
    for (const m of group.messages() as unknown[]) {
      const decoded = decodeMessagePlaceholder(wasm, m as OpenedHistoryMessage);
      if (decoded === null) continue;
      const id = safeBigInt(decoded.messageId);
      if (id !== null && (newest === null || id > newest)) newest = id;
      // A sender older than this rule (or another client) may carry them.
      if (isBookkeepingSystemRow(decoded)) continue;
      rows.push(decoded);
    }
    bundled.push({ hex: bytesToHex(group.groupId), rows, newest });
  }

  let applied = 0;
  await updateMessageLog(args.accountIdB64, args.deviceKey, (current) => {
    const groups: { [hex: string]: PersistedGroup } = { ...(current?.groups ?? {}) };
    applied = 0;
    for (const { hex, rows, newest } of bundled) {
      const filed = fileHistoryRows(groups[hex], rows, newest);
      groups[hex] = filed.slot;
      applied += filed.added;
    }
    return { version: current?.version ?? MESSAGE_LOG_VERSION, groups };
  });
  return applied;
}

// ---- placeholder encode / decode (G-6 step 2) -------------------

// `payload_bytes` is the canonical cross-platform placeholder
// produced by the shared Rust core via wasm
// (`encodeHistoryPlaceholder` / `decodeHistoryPlaceholder`).
// iOS / Android call the SAME codec through UniFFI, so the bytes
// agree on the wire — a web-scanner -> native-joiner pairing now
// interoperates (closing the JSON-shape divergence documented in
// known-gaps G-6). The wasm boundary carries `direction` as a
// u8: 0 = outgoing, 1 = incoming, 2 = system.

const DIRECTION_OUTGOING = 0;
const DIRECTION_INCOMING = 1;
const DIRECTION_SYSTEM = 2;

function directionToTag(d: PersistedMessage["direction"]): number {
  switch (d) {
    case "outgoing":
      return DIRECTION_OUTGOING;
    case "incoming":
      return DIRECTION_INCOMING;
    default:
      return DIRECTION_SYSTEM;
  }
}

function tagToDirection(tag: number): PersistedMessage["direction"] {
  switch (tag) {
    case DIRECTION_OUTGOING:
      return "outgoing";
    case DIRECTION_INCOMING:
      return "incoming";
    default:
      return "system";
  }
}

/** Standard base64 to bytes, or `undefined` when it does not decode. */
function decodeB64OrUndefined(b64: string | undefined): Uint8Array | undefined {
  if (b64 === undefined || b64.length === 0) return undefined;
  try {
    return base64Decode(b64);
  } catch {
    return undefined;
  }
}

/** Encode the local-store message row into the canonical
 *  cross-platform placeholder bytes (version 5 adds the ADR-0047 section 6
 *  micro thumbnail; version 4: the ADR-0047 media metadata,
 *  so a voice message, video note or GIF restores as itself on the new
 *  device; the ADR-0087 channel invitation card, so an invitation restores
 *  as its card and not as the bare `/g/` link; the ADR-0052 reply and forward
 *  snapshots; the ADR-0092 audio tags and content). The local-only `played`
 *  marker is never transferred. `groupIdB64` names the conversation a poll's
 *  or checklist's state is read from. Exported for the tests. */
export function encodeMessagePlaceholder(wasm: Wasm, m: PersistedMessage, groupIdB64: string): Uint8Array {
  const messageId = safeBigInt(m.messageId) ?? 0n;
  const attachments: HistoryAttachmentInput[] = m.attachments.map((a) => {
    const size = safeBigInt(a.sizeStr) ?? 0n;
    const input = new wasm.HistoryAttachmentInput(
      base64Decode(a.blobIdB64),
      base64Decode(a.keyB64),
      base64Decode(a.nonceB64),
      size,
      a.mime ?? undefined,
      a.name ?? undefined,
    );
    const { media, thumb } = historyMediaFromPersisted(a);
    if (media !== null) {
      input.setMedia(media.kind, media.durationMs, media.waveform, media.width, media.height);
    }
    // ADR-0092 section 3.11 — an audio file's tags travel too.
    if (typeof a.title === "string" || typeof a.artist === "string") {
      input.setAudioTags(a.title ?? null, a.artist ?? null);
    }
    // ADR-0047 section 6 — the micro thumbnail travels too (placeholder
    // version 5); the core keeps it on the first photo or video only.
    const inline = inlineThumbFromPersisted(a.inlineThumbB64);
    if (inline !== undefined) input.setInlineThumb(inline);
    if (thumb !== null) {
      input.setThumb(
        thumb.blobId,
        thumb.key,
        thumb.nonce,
        thumb.size,
        thumb.mime,
        thumb.width,
        thumb.height,
      );
    }
    return input;
  });
  const card = m.deleted === true ? undefined : decodeB64OrUndefined(m.channelInviteB64);
  const replyTarget = m.reply === undefined ? null : safeBigInt(m.reply.target);
  const forwardOriginId = m.forward?.origin === undefined ? null : safeBigInt(m.forward.origin.messageId);
  const forwardOriginGroup =
    forwardOriginId === null ? undefined : decodeB64OrUndefined(m.forward?.origin?.groupIdB64);
  return wasm.encodeHistoryPlaceholder(
    messageId,
    directionToTag(m.direction),
    m.createdAt,
    m.text,
    attachments,
    card,
    replyTarget ?? undefined,
    replyTarget === null ? undefined : m.reply?.author,
    replyTarget === null ? undefined : m.reply?.excerpt,
    m.forward?.author,
    forwardOriginGroup,
    forwardOriginGroup === undefined ? undefined : (forwardOriginId ?? undefined),
    // ADR-0092 section 3.11 — the content and, for a poll or checklist, its
    // state as this device folded it.
    contentMessageForBundle(wasm, m.content, groupIdB64, Number(messageId)),
  );
}

/** How a poll is closed, for the bundle's snapshot. */
function closedStateOf(
  closes: readonly { messageId: number; closer: string; closerIsAdmin: boolean }[],
  authorKeyB64: string,
  closed: boolean,
): { kind: "open" } | { kind: "byTime" } | { kind: "byOp"; messageId: number } {
  const ops = closes.filter((c) => c.closer === authorKeyB64 || c.closerIsAdmin).map((c) => c.messageId);
  if (ops.length > 0) return { kind: "byOp", messageId: Math.min(...ops) };
  return closed ? { kind: "byTime" } : { kind: "open" };
}

/** ADR-0092 section 3.11 — the bundle's content field for one message. */
function contentMessageForBundle(
  wasm: Wasm,
  raw: unknown,
  groupIdB64: string,
  messageId: number,
): ContentMessage | undefined {
  const content = contentFromPersisted(raw);
  if (content === undefined) return undefined;
  const entry = contentOf(groupIdB64)?.entries[String(messageId)];
  let state: ContentState | null = null;
  try {
    if (entry?.kind === "poll") {
      const tally = foldPollEntry(entry, Date.now());
      if (tally !== null) {
        state = wasm.pollStateFromCounts(
          Uint32Array.from(tally.counts),
          tally.voters,
          closedStateOf(entry.closes, entry.authorKeyB64, tally.closed),
        );
      }
    } else if (entry?.kind === "checklist") {
      const folded = foldChecklistEntry(entry);
      if (folded !== null) state = wasm.checklistStateSnapshot(folded);
    }
  } catch {
    state = null;
  }
  return { content, ...(state === null ? {} : { state }) };
}

/** Decode canonical placeholder bytes back into a local-store
 *  message row. Returns `null` on any malformed payload. Exported for the
 *  tests. */
export function decodeMessagePlaceholder(
  wasm: Wasm,
  m: OpenedHistoryMessage,
): PersistedMessage | null {
  let opened: OpenedHistoryPlaceholder;
  try {
    opened = wasm.decodeHistoryPlaceholder(m.payloadBytes);
  } catch {
    return null;
  }
  const attachments: PersistedAttachment[] = (
    opened.attachments as unknown as OpenedHistoryAttachment[]
  ).map((a) => ({
    blobIdB64: base64Encode(a.blobId),
    keyB64: base64Encode(a.key),
    nonceB64: base64Encode(a.nonce),
    mime: a.mime ?? null,
    sizeStr: a.size.toString(),
    name: a.name ?? null,
    ...persistedMediaFromOpened(a),
    ...(a.title === undefined ? {} : { title: a.title }),
    ...(a.artist === undefined ? {} : { artist: a.artist }),
    ...(isInlineThumbJpeg(a.inlineThumb) ? { inlineThumbB64: b64UrlEncode(a.inlineThumb) } : {}),
  }));
  const content = opened.content?.content;
  const contentState = opened.content?.state ?? undefined;
  const card = opened.channelInvite;
  const replyTarget = opened.replyTarget;
  const replyAuthor = opened.replyAuthor;
  const replyExcerpt = opened.replyExcerpt;
  const forwardedFrom = opened.forwardedFrom;
  const originGroup = opened.forwardOriginGroup;
  const originMessage = opened.forwardOriginMessage;
  const origin =
    originGroup !== undefined && originMessage !== undefined
      ? { groupIdB64: base64Encode(originGroup), messageId: originMessage.toString() }
      : undefined;
  return {
    messageId: opened.messageId.toString(),
    direction: tagToDirection(opened.direction),
    text: opened.text,
    attachments,
    createdAt: opened.createdAt,
    ...(card !== undefined && card.length > 0 ? { channelInviteB64: base64Encode(card) } : {}),
    ...(replyTarget !== undefined
      ? {
          reply: {
            target: replyTarget.toString(),
            ...(replyAuthor !== undefined ? { author: replyAuthor } : {}),
            ...(replyExcerpt !== undefined ? { excerpt: replyExcerpt } : {}),
          },
        }
      : {}),
    ...(forwardedFrom !== undefined || origin !== undefined
      ? {
          forward: {
            ...(forwardedFrom !== undefined ? { author: forwardedFrom } : {}),
            ...(origin !== undefined ? { origin } : {}),
          },
        }
      : {}),
    ...(content === undefined ? {} : { content }),
    ...(contentState === undefined || contentState === null ? {} : { contentState }),
  };
}

/** Serialize a decimal message-id string as the 8-byte
 *  big-endian u64 the `HistoryMessage.message_id` field uses on
 *  every platform (the native convention; previously web emitted
 *  UTF-8 of the decimal string, which broke web<->native). */
function messageIdToBeBytes(id: string): Uint8Array {
  const value = safeBigInt(id) ?? 0n;
  const out = new Uint8Array(8);
  let v = value & 0xff_ff_ff_ff_ff_ff_ff_ffn;
  for (let i = 7; i >= 0; i -= 1) {
    out[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return out;
}

function safeBigInt(s: string): bigint | null {
  if (s.length === 0) return null;
  for (let i = 0; i < s.length; i += 1) {
    const c = s.charCodeAt(i);
    if (c < 48 || c > 57) return null;
  }
  try {
    return BigInt(s);
  } catch {
    return null;
  }
}

// ---- hex helpers ------------------------------------------------

function hexToBytes(hex: string): Uint8Array | null {
  if (hex.length % 2 !== 0) return null;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    const hi = parseInt(hex.slice(i * 2, i * 2 + 1), 16);
    const lo = parseInt(hex.slice(i * 2 + 1, i * 2 + 2), 16);
    if (Number.isNaN(hi) || Number.isNaN(lo)) return null;
    out[i] = (hi << 4) | lo;
  }
  return out;
}

function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i += 1) {
    out += (bytes[i] as number).toString(16).padStart(2, "0");
  }
  return out;
}
