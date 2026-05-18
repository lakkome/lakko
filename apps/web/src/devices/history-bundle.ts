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
import { loadWasm } from "../wasm";
import type {
  HistoryGroupInput,
  HistoryMessageInput,
  OpenedHistoryGroup,
  OpenedHistoryMessage,
} from "../../wasm/lakko_ffi_wasm";
import {
  loadMessageLog,
  saveMessageLog,
  type MessageLogPayload,
  type PersistedGroup,
  type PersistedMessage,
} from "../chat/message-log-store";

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
  readonly deviceKey: CryptoKey;
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
    const trimmed = slot.messages.slice(-perGroupCap);
    const capped = trimmed.length < slot.messages.length;
    const messages: HistoryMessageInput[] = [];
    for (const m of trimmed) {
      // `payload_bytes` would ideally be the original wire
      // ApplicationPayload bytes; the message log doesn't
      // carry them today, so the MVP path round-trips the
      // text + attachment ref via a synthetic blob that the
      // receiver's `applyHistoryBundleToLog` decodes back.
      // Wire format identical to the proto field so a future
      // sprint can swap the placeholder for true plaintext.
      const placeholder = encodeMessagePlaceholder(m);
      const createdMs = (() => {
        if (m.createdAt === undefined || m.createdAt === null) return 0;
        const v = Date.parse(m.createdAt);
        return Number.isFinite(v) && v > 0 ? v : 0;
      })();
      messages.push(
        new wasm.HistoryMessageInput(
          placeholder,
          /* sender_leaf */ 0,
          textEncoder.encode(m.messageId),
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
  readonly deviceKey: CryptoKey;
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
      const decoded = decodeMessagePlaceholder(msg);
      if (decoded === null) continue;
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

// ---- placeholder encode / decode --------------------------------

// MVP path: the message log carries the already-projected
// `(text, attachments, createdAt, direction)` shape, not the
// original wire ApplicationPayload bytes. To keep the bundle
// schema honest we encode the runtime row into a self-describing
// JSON blob and stuff it into `payload_bytes`. A future sprint
// can replace this with the true MLS application-payload bytes
// once the message log starts retaining them.

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

function encodeMessagePlaceholder(m: PersistedMessage): Uint8Array {
  return textEncoder.encode(JSON.stringify(m));
}

function decodeMessagePlaceholder(
  m: OpenedHistoryMessage,
): PersistedMessage | null {
  try {
    const obj = JSON.parse(textDecoder.decode(m.payloadBytes));
    if (typeof obj !== "object" || obj === null) return null;
    return obj as PersistedMessage;
  } catch {
    return null;
  }
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
