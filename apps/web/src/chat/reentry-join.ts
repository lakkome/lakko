// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0091 phase 2 section 5 - what follows a re-entry join, once
 * `Group.joinReentry` replaced the stale group and the registry holds the new
 * one (`MailboxContext`, which also makes the snapshot durable).
 *
 * In order:
 *
 * 1. The gap channel (`reentry-history.ts`) is derived NOW, at the epoch the
 *    replacement created (`epoch_left + 1`): a later commit changes the
 *    exporter.
 * 2. The suspicion is cleared and a hold or behind mark the group is past is
 *    dropped.
 * 3. The next poll reads again from the commit that brought this device
 *    back (the rows after it were skipped before the Welcome arrived).
 * 4. The gap line goes into the chat, from the first row this device could
 *    not read to now (UX 2.4).
 * 5. The record goes, and the private keys of every recorded KeyPackage are
 *    forgotten.
 * 6. The missed messages are looked for on the gap channel, durably: once
 *    now, then after every poll of the group and on opening the chat, at
 *    most once a minute, until they arrive or 30 days passed (live walk
 *    2026-11-19; the line is narrowed or removed when they arrive).
 */

import { base64Decode } from "../auth/account-id";
import type { Group as WasmGroup, Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { conversationBehind } from "./conversation-behind";
import { registeredCommitHolds } from "./registered-commit-hold";
import { gapChannelOf, type GapChannel } from "./reentry-history";
import { addGapLine, deleteReentryRecord, setReentryReadFrom, type ReentryRecord } from "./reentry-store";

/** What the bookkeeping needs from the app. */
export interface ReentryJoinPorts {
  now(): number;
  identity(): WasmIdentity | null;
  persistDurable(): Promise<void>;
  /** Fetch the gap's messages in the background (never awaited). */
  fetchGapHistory(input: {
    readonly groupIdB64: string;
    readonly channel: GapChannel;
    readonly gapFromMs: number;
    readonly gapToMs: number;
  }): void;
}

let ports: ReentryJoinPorts | null = null;

/** Install (or, with `null`, remove) the ports. */
export function installReentryJoinPorts(next: ReentryJoinPorts | null): void {
  ports = next;
}

/** Steps 1 to 6 above. Never throws past a failed snapshot write. */
export async function noteReentryJoined(input: {
  readonly groupIdB64: string;
  readonly group: WasmGroup;
  readonly record: ReentryRecord;
}): Promise<void> {
  const p = ports;
  const nowMs = p?.now() ?? Date.now();
  const identity = p?.identity() ?? null;
  let epoch: bigint | undefined;
  try {
    epoch = input.group.epoch;
  } catch {
    epoch = undefined;
  }
  let channel: GapChannel | null = null;
  if (identity !== null) {
    try {
      // This device's own leaf key is the exporter's context (section 7).
      channel = await gapChannelOf(input.group, identity, identity.signaturePublicKey);
    } catch {
      channel = null;
    }
  }
  // The commit that reached the new epoch, as the reads signed at the old
  // epoch named it.
  const commitId = epoch === undefined ? undefined : conversationBehind.registeredCommits(input.groupIdB64).get(epoch);
  conversationBehind.clear(input.groupIdB64);
  registeredCommitHolds.observeEpoch(input.groupIdB64, epoch);
  if (commitId !== undefined) setReentryReadFrom(input.groupIdB64, commitId);
  addGapLine(input.groupIdB64, { fromMs: input.record.gapFromMs, toMs: nowMs });
  deleteReentryRecord(input.groupIdB64);
  if (identity !== null) {
    for (const kp of input.record.keyPackagesB64) {
      try {
        identity.forgetKeyPackage(base64Decode(kp));
      } catch {
        /* already gone */
      }
    }
    await p?.persistDurable().catch(() => undefined);
  }
  // eslint-disable-next-line no-console
  console.info(`chat re-entry joined: epoch=${epoch ?? "-"}`);
  if (channel !== null && p !== null) p.fetchGapHistory({ groupIdB64: input.groupIdB64, channel, gapFromMs: input.record.gapFromMs, gapToMs: nowMs });
}
