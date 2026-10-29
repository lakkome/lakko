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
import {
  loadMessageLog,
  saveMessageLog,
  type MessageLogPayload,
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
      const placeholder = encodeMessagePlaceholder(wasm, m);
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
 * stomping concurrent edits).
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

  // Read current log (may be null on a fresh joiner) +
  // mutate per-group entries.
  const existing =
    (await loadMessageLog(args.accountIdB64, args.deviceKey)) ??
    ({ version: 1, groups: {} } as MessageLogPayload);

  // Mutable working copy.
  const nextGroups: { [hex: string]: PersistedGroup } = { ...existing.groups };
  let applied = 0;

  for (const g of openedGroups) {
    const group = g as OpenedHistoryGroup;
    const hex = bytesToHex(group.groupId);
    const incomingMessages = group.messages() as unknown[];
    const slot = nextGroups[hex] ?? ({ cursor: "0", messages: [] } as PersistedGroup);
    const existingIds = new Set(slot.messages.map((m) => m.messageId));
    const merged = [...slot.messages];
    for (const m of incomingMessages) {
      const msg = m as OpenedHistoryMessage;
      const decoded = decodeMessagePlaceholder(wasm, msg);
      if (decoded === null) continue;
      // A sender older than this rule (or another client) may carry them.
      if (isBookkeepingSystemRow(decoded)) continue;
      if (existingIds.has(decoded.messageId)) continue;
      merged.push(decoded);
      existingIds.add(decoded.messageId);
      applied += 1;
    }
    // Sort by parsed messageId so the timeline lands in order.
    // Defensive on non-numeric strings — keep their relative
    // order via a string compare fallback.
    merged.sort((a, b) => {
      const ai = safeBigInt(a.messageId);
      const bi = safeBigInt(b.messageId);
      if (ai !== null && bi !== null) return Number(ai - bi);
      return a.messageId < b.messageId ? -1 : a.messageId > b.messageId ? 1 : 0;
    });
    nextGroups[hex] = {
      cursor: slot.cursor,
      messages: merged,
    };
  }

  await saveMessageLog(args.accountIdB64, args.deviceKey, {
    version: existing.version,
    groups: nextGroups,
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

/** Encode the local-store message row into the canonical
 *  cross-platform placeholder bytes (version 2, which carries the ADR-0047
 *  media metadata, so a voice message, video note or GIF restores as itself
 *  on the new device). The local-only `played` marker is never transferred. */
function encodeMessagePlaceholder(wasm: Wasm, m: PersistedMessage): Uint8Array {
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
  return wasm.encodeHistoryPlaceholder(
    messageId,
    directionToTag(m.direction),
    m.createdAt,
    m.text,
    attachments,
  );
}

/** Decode canonical placeholder bytes back into a local-store
 *  message row. Returns `null` on any malformed payload. */
function decodeMessagePlaceholder(
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
  }));
  return {
    messageId: opened.messageId.toString(),
    direction: tagToDirection(opened.direction),
    text: opened.text,
    attachments,
    createdAt: opened.createdAt,
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
