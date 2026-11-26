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
  Identity as WasmIdentity,
  OpenedHistoryAttachment,
  OpenedHistoryBundle,
  OpenedHistoryGroup,
  OpenedHistoryMessage,
  OpenedHistoryPlaceholder,
} from "../../wasm/lakko_ffi_wasm";
import { base64Decode, base64Encode } from "../auth/account-id";
import { historyMediaFromPersisted, persistedMediaFromOpened } from "./history-media";
import { b64UrlDecode, b64UrlEncode, inlineThumbFromPersisted } from "../chat/persisted-attachment";
import { isInlineThumbJpeg } from "../media/visual-media";
import { fileHistoryRows } from "../chat/background-log";
import type { CallLogEntry } from "../calls/call-log";
import {
  callDurationSecsForBundle,
  callEntryFromHistory,
  callHistoryMessageId,
  callRecordOutcomeOf,
  fileHistoryCallEntries,
  loadCallLogForBundle,
} from "../calls/call-log-history";
import type { CallLogData } from "../calls/call-log-store";
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
  // `docs/ux/calls.md` section 6 — this account's call rows travel too
  // (placeholder version 7). They live in their own log on web.
  let callLog: CallLogData | null = null;
  try {
    callLog = await loadCallLogForBundle(args.accountIdB64, args.deviceKey);
  } catch {
    callLog = null;
  }
  const callsByHex = callRowsByGroupHex(callLog);
  const groupHexes = [...new Set([...Object.keys(log?.groups ?? {}), ...callsByHex.keys()])];
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
  let skipped = 0;

  for (const hex of groupHexes) {
    const slot = log?.groups[hex];
    const groupIdBytes = hexToBytes(hex);
    if (groupIdBytes === null) continue;
    const carried = (slot?.messages ?? []).filter((m) => !isBookkeepingSystemRow(m));
    // Messages and calls compete for the group's share by time, newest kept.
    const merged = mergeRowsAndCalls(carried, callsByHex.get(hex) ?? []);
    const trimmed = merged.slice(-perGroupCap);
    const capped = trimmed.length < merged.length;
    const messages: HistoryMessageInput[] = [];
    for (const item of trimmed) {
      if (item.kind === "call") {
        const call = callPlaceholderOrNull(wasm, item.call);
        if (call === null) {
          skipped += 1;
          continue;
        }
        // A call row's id is not a Delivery Service id: it never moves the
        // bundle's high-water mark.
        messages.push(
          new wasm.HistoryMessageInput(
            call.placeholder,
            /* sender_leaf */ 0,
            bigIntToBeBytes(call.messageId),
            BigInt(Math.max(0, Math.floor(item.call.startedAtMs))),
          ),
        );
        continue;
      }
      const m = item.row;
      // G-6 step 2: `payload_bytes` is now the canonical
      // cross-platform placeholder produced by the shared Rust
      // core (`encodeHistoryPlaceholder`), identical bytes to
      // what iOS / Android emit. The sibling `message_id` field
      // is the 8-byte big-endian u64 (the native convention),
      // replacing the old web-only UTF-8-of-decimal encoding so
      // a web-scanner -> native-joiner pairing interoperates.
      const placeholder = placeholderOrNull(wasm, m, base64Encode(groupIdBytes));
      if (placeholder === null) {
        skipped += 1;
        continue;
      }
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

  if (skipped > 0) {
    // eslint-disable-next-line no-console
    console.warn(`history bundle: ${skipped} messages could not be carried`);
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
  /** Rewrites each bundled row before it is filed (the gap bundle's
   *  author and direction, live walk 3). */
  readonly adapt?: (row: PersistedMessage) => PersistedMessage;
}): Promise<number> {
  const wasm = await loadWasm();
  const opened = wasm.openHistoryBundleStructured(args.secret, args.sealedBytes);
  // A pairing bundle comes from a device of this same account, so its call
  // rows are from this account's seat and go to the call log.
  return fileOpenedHistoryBundle(wasm, opened, { ...args, calls: "file" });
}

/** File an already-opened bundle into the log (the body of
 *  {@link applyHistoryBundleToLog}). */
async function fileOpenedHistoryBundle(
  wasm: Wasm,
  opened: OpenedHistoryBundle,
  args: {
    readonly accountIdB64: string;
    readonly deviceKey: DeviceCipher;
    readonly adapt?: (row: PersistedMessage) => PersistedMessage;
    /** What to do with call rows: `file` them into the call log (a bundle
     *  from a device of this account) or `drop` them (a bundle from another
     *  account's seat). */
    readonly calls: "file" | "drop";
  },
): Promise<number> {
  const openedGroups = opened.groups() as unknown[];

  const bundled: {
    readonly hex: string;
    readonly rows: readonly PersistedMessage[];
    readonly newest: bigint | null;
    /** The group carried call rows and nothing else: no message slot is
     *  made for it. */
    readonly callsOnly: boolean;
  }[] = [];
  const callsByGroup = new Map<string, CallLogEntry[]>();
  for (const g of openedGroups) {
    const group = g as OpenedHistoryGroup;
    const rows: PersistedMessage[] = [];
    let newest: bigint | null = null;
    let sawCall = false;
    let sawMessage = false;
    const groupIdB64 = base64Encode(group.groupId);
    for (const m of group.messages() as unknown[]) {
      const decoded = decodeHistoryRow(wasm, m as OpenedHistoryMessage);
      if (decoded === null) continue;
      if (decoded.kind === "call") {
        // A call row's id is not a Delivery Service id: it never sets the
        // history floor and never enters the message log.
        sawCall = true;
        if (args.calls === "file") {
          const list = callsByGroup.get(groupIdB64) ?? [];
          list.push(decoded.entry);
          callsByGroup.set(groupIdB64, list);
        }
        continue;
      }
      sawMessage = true;
      const row = decoded.row;
      const id = safeBigInt(row.messageId);
      if (id !== null && (newest === null || id > newest)) newest = id;
      // A sender older than this rule (or another client) may carry them.
      if (isBookkeepingSystemRow(row)) continue;
      rows.push(args.adapt === undefined ? row : args.adapt(row));
    }
    bundled.push({ hex: bytesToHex(group.groupId), rows, newest, callsOnly: sawCall && !sawMessage });
  }

  let applied = 0;
  if (bundled.some((b) => !b.callsOnly)) {
    await updateMessageLog(args.accountIdB64, args.deviceKey, (current) => {
      const groups: { [hex: string]: PersistedGroup } = { ...(current?.groups ?? {}) };
      applied = 0;
      for (const { hex, rows, newest, callsOnly } of bundled) {
        if (callsOnly) continue;
        const filed = fileHistoryRows(groups[hex], rows, newest);
        groups[hex] = filed.slot;
        applied += filed.added;
      }
      return { version: current?.version ?? MESSAGE_LOG_VERSION, groups };
    });
  }
  if (callsByGroup.size > 0) {
    try {
      applied += await fileHistoryCallEntries(args.accountIdB64, args.deviceKey, callsByGroup);
    } catch (err) {
      // The messages are filed; a call log that cannot be written costs the
      // call rows only.
      // eslint-disable-next-line no-console
      console.warn("history bundle: call rows could not be filed", err);
    }
  }
  return applied;
}

// ---- ADR-0091 section 7: the messages of a re-entry's gap --------

/** When a row was written, unix ms; `NaN` when unknown. */
function rowMs(m: PersistedMessage): number {
  if (m.createdAt === undefined || m.createdAt === null) return Number.NaN;
  const v = Date.parse(m.createdAt);
  return Number.isFinite(v) ? v : Number.NaN;
}

/**
 * ADR-0091 section 7 - the device that brought another one back sends the
 * messages of ONE conversation it holds from `fromMs` on, sealed to the
 * returning device's public gap key and signed by `readmitter`'s leaf key
 * (`sealGapHistoryStructured`, security review 2026-11-21). `null` when it
 * holds none.
 *
 * The group's `countCapped` flag means "not everything": set when the cap
 * trimmed the list, and when this device holds no row older than `fromMs`
 * (it cannot vouch that it saw the start of the gap). A receiver whose
 * bundle is not capped drops its gap line (UX 2.4).
 */
export async function buildGapHistoryBundle(args: {
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  /** This device's identity in the conversation: its leaf key signs. */
  readonly readmitter: WasmIdentity;
  /** The request's 32-byte public gap key. */
  readonly gapKey: Uint8Array;
  readonly slotHex: string;
  readonly fromMs: number;
  /** This account's own key in the conversation: the author of its
   *  outgoing rows (live walk 3). */
  readonly ownAuthorKeyB64?: string | null;
}): Promise<Uint8Array | null> {
  const log = await loadMessageLog(args.accountIdB64, args.deviceKey);
  const slot = log?.groups[args.slotHex];
  if (slot === undefined) return null;
  const groupIdBytes = hexToBytes(args.slotHex);
  if (groupIdBytes === null) return null;
  const carried = slot.messages.filter((m) => !isBookkeepingSystemRow(m));
  const inGap = carried.filter((m) => {
    const t = rowMs(m);
    return Number.isFinite(t) && t >= args.fromMs;
  });
  if (inGap.length === 0) return null;
  const trimmed = inGap.slice(-MAX_BUNDLE_MESSAGES);
  const heldBefore = carried.some((m) => {
    const t = rowMs(m);
    return Number.isFinite(t) && t < args.fromMs;
  });
  const capped = trimmed.length < inGap.length || !heldBefore;
  const wasm = await loadWasm();
  const groupIdB64 = base64Encode(groupIdBytes);
  let maxHlc = 0n;
  const messages: HistoryMessageInput[] = [];
  let skipped = 0;
  for (const m of trimmed) {
    const t = rowMs(m);
    const placeholder = placeholderOrNull(wasm, m, groupIdB64, args.ownAuthorKeyB64);
    if (placeholder === null) {
      skipped += 1;
      continue;
    }
    messages.push(
      new wasm.HistoryMessageInput(
        placeholder,
        0,
        messageIdToBeBytes(m.messageId),
        BigInt(Number.isFinite(t) && t > 0 ? t : 0),
      ),
    );
    const cur = safeBigInt(m.messageId);
    if (cur !== null && cur > maxHlc) maxHlc = cur;
  }
  if (skipped > 0) {
    // eslint-disable-next-line no-console
    console.warn(`gap history: ${skipped} messages could not be carried`);
  }
  if (messages.length === 0) return null;
  return wasm.sealGapHistoryStructured(
    args.readmitter,
    args.gapKey,
    groupIdBytes,
    [new wasm.HistoryGroupInput(groupIdBytes, messages, capped)],
    maxHlc,
  );
}

/**
 * ADR-0101 section 9 - one page of a conversation's history for a person
 * back after a key reset, newest first: the rows of `sourceSlotHex` (the
 * conversation's earlier group, whose DS ids are unique across journals and
 * older than any of the successor's) with a DS id below `beforeMessageId`
 * (0: the newest), at most `pageSize`, sealed to the request's gap key for
 * `targetGroupId` (the successor the person is in), signed by
 * `readmitter`'s leaf. An EMPTY page (nothing older is held) is sealed too:
 * it tells the requester to stop. `oldestMessageId` is the next request's
 * `before`. `null` when the source slot is not held or the core refuses.
 */
export async function buildHistoryPage(args: {
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  readonly readmitter: WasmIdentity;
  readonly gapKey: Uint8Array;
  readonly sourceSlotHex: string;
  readonly targetGroupId: Uint8Array;
  readonly beforeMessageId: bigint;
  readonly pageSize: number;
  readonly ownAuthorKeyB64?: string | null;
}): Promise<{ readonly sealed: Uint8Array; readonly rows: number; readonly oldestMessageId: bigint | null } | null> {
  const log = await loadMessageLog(args.accountIdB64, args.deviceKey);
  const slot = log?.groups[args.sourceSlotHex];
  if (slot === undefined) return null;
  const page = historyPageRows(
    slot.messages.filter((m) => !isBookkeepingSystemRow(m)),
    args.beforeMessageId,
    args.pageSize,
  );
  const wasm = await loadWasm();
  const targetB64 = base64Encode(args.targetGroupId);
  const messages: HistoryMessageInput[] = [];
  let maxHlc = 0n;
  let oldest: bigint | null = null;
  for (const m of page) {
    const t = rowMs(m);
    const placeholder = placeholderOrNull(wasm, m, targetB64, args.ownAuthorKeyB64);
    const id = safeBigInt(m.messageId);
    if (id !== null && (oldest === null || id < oldest)) oldest = id;
    if (placeholder === null) continue;
    messages.push(
      new wasm.HistoryMessageInput(placeholder, 0, messageIdToBeBytes(m.messageId), BigInt(Number.isFinite(t) && t > 0 ? t : 0)),
    );
    if (id !== null && id > maxHlc) maxHlc = id;
  }
  try {
    const sealed = wasm.sealGapHistoryStructured(
      args.readmitter,
      args.gapKey,
      args.targetGroupId,
      [new wasm.HistoryGroupInput(args.targetGroupId, messages, false)],
      maxHlc,
    );
    return { sealed, rows: messages.length, oldestMessageId: oldest };
  } catch {
    return null;
  }
}

/**
 * The rows of one history page: DS id below `before` (0: no bound), the
 * newest `pageSize` of them, oldest first. Rows whose id does not parse are
 * left out. Pure.
 */
export function historyPageRows<T extends { readonly messageId: string }>(
  rows: readonly T[],
  before: bigint,
  pageSize: number,
): T[] {
  const withIds: { row: T; id: bigint }[] = [];
  for (const row of rows) {
    const id = safeBigInt(row.messageId);
    if (id === null || id <= 0n) continue;
    if (before > 0n && id >= before) continue;
    withIds.push({ row, id });
  }
  if (pageSize <= 0) return [];
  withIds.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return withIds.slice(-pageSize).map((x) => x.row);
}

/**
 * The receiving half: open a gap bundle with this device's gap secret and
 * file its rows into the log (the same filing as a pairing bundle) - only
 * when `signerAccepted` says the joined group accepts the leaf that signed
 * it (`Group.gapHistorySignerAccepted`). Returns `null` when the signer is
 * not accepted (the bundle is dropped, the gap line stays); otherwise how
 * many rows were added, whether the sender held the whole gap (`complete`),
 * and the time of the earliest row it carried. Throws when the row does not
 * open (a `gapHistory.<code>` error).
 */
export async function applyGapHistoryBundle(args: {
  readonly accountIdB64: string;
  readonly deviceKey: DeviceCipher;
  /** This device's 32-byte gap secret. */
  readonly gapSecret: Uint8Array;
  /** The conversation the bundle must be for. */
  readonly groupId: Uint8Array;
  readonly sealedBytes: Uint8Array;
  /** Whether the joined group accepts this signer's leaf key. */
  readonly signerAccepted: (signerLeafKey: Uint8Array) => boolean;
  /** This account's own key in the conversation (live walk 3). */
  readonly ownAuthorKeyB64: string | null;
  /** ADR-0101 walk 2026-11-26 (D2): the account keys this account had
   *  before a key reset; a row they authored is this account's own. */
  readonly previousOwnKeysB64?: readonly string[];
  /** The route of the request the device came back by, when known. */
  readonly route: GapBundleRoute | undefined;
}): Promise<{
  readonly applied: number;
  readonly complete: boolean;
  readonly earliestMs: number | undefined;
  /** The rows the bundle carried (bookkeeping rows left out), for an open
   *  chat to show at once. */
  readonly rows: readonly PersistedMessage[];
} | null> {
  const wasm = await loadWasm();
  const adapt = (row: PersistedMessage): PersistedMessage =>
    gapBundleRow(row, args.ownAuthorKeyB64, args.route, args.previousOwnKeysB64 ?? []);
  const structured = wasm.openGapHistoryStructured(args.gapSecret, args.groupId, args.sealedBytes);
  let opened: OpenedHistoryBundle | undefined;
  try {
    let accepted = false;
    try {
      accepted = args.signerAccepted(structured.signerLeafKey);
    } catch {
      accepted = false;
    }
    if (!accepted) {
      // eslint-disable-next-line no-console
      console.info("chat re-entry: missed messages dropped: signer not accepted");
      return null;
    }
    opened = structured.takeBundle();
  } finally {
    structured.free();
  }
  if (opened === undefined) return null;
  let complete = true;
  let earliestMs: number | undefined;
  const rows: PersistedMessage[] = [];
  for (const g of opened.groups() as unknown[]) {
    const group = g as OpenedHistoryGroup;
    if (group.countCapped) complete = false;
    for (const m of group.messages() as unknown[]) {
      // A call row is not a message of the gap (and never shown as one).
      const decoded = decodeMessagePlaceholder(wasm, m as OpenedHistoryMessage);
      if (decoded === null) continue;
      const t = rowMs(decoded);
      if (Number.isFinite(t) && (earliestMs === undefined || t < earliestMs)) earliestMs = t;
      if (!isBookkeepingSystemRow(decoded)) rows.push(adapt(decoded));
    }
  }
  // Call rows are from the sending account's seat: filed only when this
  // account's own device sent the bundle. Web never puts them in one.
  const applied = await fileOpenedHistoryBundle(wasm, opened, {
    ...args,
    adapt,
    calls: args.route === "own-device" ? "file" : "drop",
  });
  return { applied, complete, earliestMs, rows };
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
export function encodeMessagePlaceholder(
  wasm: Wasm,
  m: PersistedMessage,
  groupIdB64: string,
  ownAuthorKeyB64?: string | null,
): Uint8Array {
  const messageId = safeBigInt(m.messageId) ?? 0n;
  const attachments: HistoryAttachmentInput[] = m.attachments.map((a) => {
    const size = safeBigInt(a.sizeStr) ?? 0n;
    const input = new wasm.HistoryAttachmentInput(
      // A live row stores these as base64url (`persistedFromAttachmentRef`),
      // a row a bundle filed as standard base64: `b64UrlDecode` reads both.
      // `base64Decode` (atob) threw on the first `-` or `_`, and one such
      // attachment took the whole bundle with it (seen live 2026-11-23: a
      // device added from web got no history).
      b64UrlDecode(a.blobIdB64),
      b64UrlDecode(a.keyB64),
      b64UrlDecode(a.nonceB64),
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
    // Placeholder version 6 (live walk 3): who wrote it.
    keyBytesOrUndefined(placeholderAuthorKeyB64(m, ownAuthorKeyB64)),
  );
}

/**
 * {@link encodeMessagePlaceholder}, or `null` when this one row cannot be
 * encoded. A row that cannot be carried is left out and the rest of the
 * bundle still goes, as on iOS and Android.
 */
function placeholderOrNull(
  wasm: Wasm,
  m: PersistedMessage,
  groupIdB64: string,
  ownAuthorKeyB64?: string | null,
): Uint8Array | null {
  try {
    return encodeMessagePlaceholder(wasm, m, groupIdB64, ownAuthorKeyB64);
  } catch {
    return null;
  }
}

/** A standard-base64 32-byte key, else `undefined`. */
function keyBytesOrUndefined(keyB64: string | undefined): Uint8Array | undefined {
  if (keyB64 === undefined) return undefined;
  try {
    const bytes = base64Decode(keyB64);
    return bytes.length === AUTHOR_KEY_LEN ? bytes : undefined;
  } catch {
    return undefined;
  }
}

const AUTHOR_KEY_LEN = 32;

/**
 * ADR-0091 live walk 3 — the author a bundled row carries: for a received
 * row the author it was attributed to, for a row this account sent its own
 * key in the conversation (else the key the row was stamped with), for a
 * system row none. Only a 32-byte key travels.
 */
export function placeholderAuthorKeyB64(
  m: PersistedMessage,
  ownAuthorKeyB64: string | null | undefined,
): string | undefined {
  if (m.direction === "system") return undefined;
  const stamped = typeof m.authorKeyB64 === "string" ? m.authorKeyB64 : undefined;
  const key = m.direction === "outgoing" ? (ownAuthorKeyB64 ?? stamped) : stamped;
  return keyBytesOrUndefined(key ?? undefined) === undefined ? undefined : (key ?? undefined);
}

/** The route of a re-entry request (section 3). */
export type GapBundleRoute = "own-device" | "mailbox" | "knock";

/**
 * ADR-0091 live walk 3 — a row of a gap bundle as the device that came back
 * files it. A row naming its author is this account's (outgoing) when the
 * author is its own key in the conversation, else the author's (incoming).
 * A row naming none takes its direction from the route (the re-admitter's
 * outgoing row is another person's unless it is this account's own device;
 * in a one-to-one the two sides swap). A system row stays a system row and
 * names nobody. The same rule as iOS and Android.
 *
 * ADR-0101 walk 2026-11-26 (D2): after a key reset the history a contact
 * sends back names this account's OLD key on the rows it wrote before the
 * reset; a row authored by any of `previousOwnKeysB64` is this account's
 * own too (outgoing), never someone else's.
 */
export function gapBundleRow(
  row: PersistedMessage,
  ownAuthorKeyB64: string | null,
  route: GapBundleRoute | undefined,
  previousOwnKeysB64: readonly string[] = [],
): PersistedMessage {
  if (row.direction === "system") {
    const { authorKeyB64: _dropped, ...rest } = row;
    return rest;
  }
  const author = typeof row.authorKeyB64 === "string" ? row.authorKeyB64 : undefined;
  if (author !== undefined) {
    const own = (ownAuthorKeyB64 !== null && author === ownAuthorKeyB64) || previousOwnKeysB64.includes(author);
    return { ...row, direction: own ? "outgoing" : "incoming" };
  }
  let direction: PersistedMessage["direction"];
  if (route === "own-device") direction = row.direction;
  else if (route === "mailbox") direction = row.direction === "outgoing" ? "incoming" : "outgoing";
  else direction = "incoming";
  return { ...row, direction };
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
 *  message row. Returns `null` on any malformed payload, and for a call row
 *  (which is not a message: {@link decodeHistoryRow}). Exported for the
 *  tests. */
export function decodeMessagePlaceholder(
  wasm: Wasm,
  m: OpenedHistoryMessage,
): PersistedMessage | null {
  const decoded = decodeHistoryRow(wasm, m);
  return decoded?.kind === "message" ? decoded.row : null;
}

/** One bundled row: a message for the message log, or a call for the call
 *  log (placeholder version 7). */
export type DecodedHistoryRow =
  | { readonly kind: "message"; readonly row: PersistedMessage }
  | { readonly kind: "call"; readonly entry: CallLogEntry };

/** Decode one bundled row. `null` on any malformed payload, and for a call
 *  row this build cannot use. Exported for the tests. */
export function decodeHistoryRow(wasm: Wasm, m: OpenedHistoryMessage): DecodedHistoryRow | null {
  let opened: OpenedHistoryPlaceholder;
  try {
    opened = wasm.decodeHistoryPlaceholder(m.payloadBytes);
  } catch {
    return null;
  }
  const callOutcome = opened.callOutcome;
  if (callOutcome !== undefined) {
    const entry = callEntryFromHistory(
      wasm,
      opened.messageId,
      opened.createdAt,
      callOutcome,
      opened.callVideo,
      opened.callDurationSecs,
    );
    return entry === null ? null : { kind: "call", entry };
  }
  const row = messageRowOf(opened);
  return row === null ? null : { kind: "message", row };
}

function messageRowOf(opened: OpenedHistoryPlaceholder): PersistedMessage | null {
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
  const direction = tagToDirection(opened.direction);
  // Placeholder version 6 (live walk 3): the author, as a live row stores it.
  const author = direction === "system" ? undefined : opened.authorKey;
  const authorKeyB64 = author !== undefined && author.length === AUTHOR_KEY_LEN ? base64Encode(author) : undefined;
  const origin =
    originGroup !== undefined && originMessage !== undefined
      ? { groupIdB64: base64Encode(originGroup), messageId: originMessage.toString() }
      : undefined;
  return {
    messageId: opened.messageId.toString(),
    direction,
    ...(authorKeyB64 === undefined ? {} : { authorKeyB64 }),
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

// ---- call rows (docs/ux/calls.md section 6, placeholder version 7) ----

/** The call log's groups keyed by group hex (the message log's key). */
function callRowsByGroupHex(log: CallLogData | null): Map<string, readonly CallLogEntry[]> {
  const out = new Map<string, readonly CallLogEntry[]>();
  if (log === null) return out;
  for (const [groupIdB64, list] of log.groups) {
    if (list.length === 0) continue;
    let bytes: Uint8Array;
    try {
      bytes = base64Decode(groupIdB64);
    } catch {
      continue;
    }
    if (bytes.length === 0) continue;
    out.set(bytesToHex(bytes), list);
  }
  return out;
}

type BundleItem =
  | { readonly kind: "message"; readonly row: PersistedMessage }
  | { readonly kind: "call"; readonly call: CallLogEntry };

/**
 * A group's messages (oldest first, the log's order) and its calls (any
 * order) in one list, oldest first. A message keeps its place among messages;
 * a message with no time takes the previous one's; a call that ties with a
 * message goes after it (the timeline's own rule, `chat/call-timeline.ts`).
 * Pure; exported for the tests.
 */
export function mergeRowsAndCalls(
  messages: readonly PersistedMessage[],
  calls: readonly CallLogEntry[],
): BundleItem[] {
  const pending = [...calls].sort((a, b) => a.startedAtMs - b.startedAtMs);
  const out: BundleItem[] = [];
  let last = Number.NEGATIVE_INFINITY;
  let next = 0;
  for (const row of messages) {
    const t = rowMs(row);
    if (Number.isFinite(t)) last = t;
    while (next < pending.length && (pending[next] as CallLogEntry).startedAtMs < last) {
      out.push({ kind: "call", call: pending[next] as CallLogEntry });
      next += 1;
    }
    out.push({ kind: "message", row });
  }
  for (; next < pending.length; next += 1) out.push({ kind: "call", call: pending[next] as CallLogEntry });
  return out;
}

/** The placeholder bytes of one call row and the id it is carried under. */
export function encodeCallPlaceholder(
  wasm: Wasm,
  entry: CallLogEntry,
): { readonly placeholder: Uint8Array; readonly messageId: bigint } {
  const messageId = callHistoryMessageId(entry.id);
  const placeholder = wasm.encodeHistoryPlaceholder(
    messageId,
    DIRECTION_SYSTEM,
    new Date(entry.startedAtMs).toISOString(),
    "",
    [],
    undefined, // channel invitation
    undefined, // reply target
    undefined, // reply author
    undefined, // reply excerpt
    undefined, // forwarded from
    undefined, // forward origin group
    undefined, // forward origin message
    undefined, // content
    undefined, // author: a call row names nobody
    callRecordOutcomeOf(wasm, entry.outcome),
    entry.video,
    callDurationSecsForBundle(entry),
  );
  return { placeholder, messageId };
}

function callPlaceholderOrNull(
  wasm: Wasm,
  entry: CallLogEntry,
): { readonly placeholder: Uint8Array; readonly messageId: bigint } | null {
  if (!Number.isFinite(entry.startedAtMs) || entry.startedAtMs < 0) return null;
  try {
    return encodeCallPlaceholder(wasm, entry);
  } catch {
    return null;
  }
}

/** A u64 as the 8 big-endian bytes `HistoryMessage.message_id` carries. */
function bigIntToBeBytes(value: bigint): Uint8Array {
  const out = new Uint8Array(8);
  let v = value & 0xff_ff_ff_ff_ff_ff_ff_ffn;
  for (let i = 7; i >= 0; i -= 1) {
    out[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return out;
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
