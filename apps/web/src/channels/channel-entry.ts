// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0064 section 7, amendment of 2026-11-14 — an owner device that holds
 * no PublisherGroup for a channel the account owns enters it by itself, from
 * a GroupInfo another owner device saved in the channel's vault head
 * (`channel-vault-head.ts`), on web.
 *
 * Newest saved epoch first ({@link enterOwnedChannel}): the external commit
 * is built (`PublisherGroup.enter`, ExternalInit alone, this device's bound
 * leaf), recorded durably with the provider carrying the held entry like any
 * staged PublisherGroup commit (kind `entry`, so a reload re-sends it), sent
 * through the DS commit route under the journal root, proven with the saved
 * epoch's access seed. On `201` the entry is merged and the new channel seed
 * resealed to the subscribers (the outbox's steps), then this device takes
 * the publisher keys and saves the current epoch in the head
 * ({@link finishEntry}). `409` or a refusal: the entry is abandoned and the
 * older saved epoch is tried; none left: the caller falls back to today's
 * behaviour (decision A where it applies, else the waiting line). Nothing is
 * shown to the person; the composer simply appears.
 *
 * Never for a co-publisher's device, a device that holds the group, a
 * channel with a suspected reset, or a channel whose entry already awaits the
 * DS ({@link entryVerdict}).
 */

import { loadWasm, type Wasm } from "../wasm";
import { base64Encode } from "../auth/account-id";
import { dsAccess } from "../ds/access";
import { withGroupMlsLock } from "../mls/group-lock";
import { channelsActions } from "../state/channels-state";
import { fetchEpoch } from "./api";
import { channelIdFromHex, heldPublisherKeys, type WasmIdentity } from "./composer";
import { publisherDeviceOrNull } from "./publisher-device";
import { withPublisherGroup } from "./publisher-group-registry";
import { newEntryId, type PublisherCommitEntry } from "./publisher-commit-outbox";
import { runRecordedPublisherCommit } from "./rotation-cron";
import {
  enterNewestFirst,
  keepTwoNewest,
  withChannelHeadStore,
  writeCurrentHead,
  type EntryAttempt,
  type EntryResult,
  type ReentryEpoch,
} from "./channel-vault-head";

type PublisherGroup = InstanceType<Wasm["PublisherGroup"]>;

function hexOf(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/**
 * Whether this device tries to enter a channel from its saved copy. Pure.
 * Only for a channel the account owns (the register's role), that this
 * device holds no group for, with no suspected reset (a device that suspects
 * one holds a group then) and no entry already awaiting the DS.
 */
export function entryVerdict(input: {
  readonly registerRole: "owner" | "co-publisher" | "subscriber" | null;
  readonly holdsGroup: boolean;
  readonly resetSuspected: boolean;
  readonly entryPending: boolean;
}): boolean {
  return input.registerRole === "owner" && !input.holdsGroup && !input.resetSuspected && !input.entryPending;
}

/** Whether an entry for `idHex` is recorded and not finished. */
export function entryPendingFor(idHex: string): boolean {
  return channelsActions.pendingPublisherCommits().some((e) => e.kind === "entry" && e.channelIdHex === idHex);
}

let ownerSourceInstalled = false;

/**
 * ADR-0057: the DS client signs a commit with the journal ROOT only for a
 * journal a source says this account owns. A group this device is entering
 * is held by no registry handle yet, so the recorded entries name it (live
 * and after a reload). Idempotent.
 */
export function ensureEntryOwnerSource(): void {
  if (ownerSourceInstalled) return;
  ownerSourceInstalled = true;
  dsAccess.registerOwnerSource((groupId) => {
    const hex = hexOf(groupId);
    return channelsActions.pendingPublisherCommits().some((e) => e.kind === "entry" && e.groupIdHex === hex)
      ? true
      : null;
  });
}

/** What one entry pass needs; the tests inject fakes. */
export interface EntryPorts {
  /** The saved epochs, newest first; `null` when the head could not be read
   *  (a network failure: nothing is decided this pass). */
  readSaved(): Promise<ReentryEpoch[] | null>;
  /** One entry at a saved epoch. */
  attempt(r: ReentryEpoch): Promise<EntryAttempt>;
  /** After the entry landed: keys, epoch, the head. */
  finish(): Promise<void>;
}

/**
 * Enter from the saved copy, newest saved epoch first. `"entered"`: this
 * device publishes now; `"owed"`: an entry awaits the DS (or the read
 * failed), nothing more this pass; `"none"`: no saved copy, or every saved
 * epoch was lost or refused. Never throws.
 */
export async function enterOwnedChannel(ports: EntryPorts): Promise<EntryResult> {
  let saved: ReentryEpoch[] | null;
  try {
    saved = await ports.readSaved();
  } catch {
    saved = null;
  }
  if (saved === null) return "owed";
  if (saved.length === 0) return "none";
  const result = await enterNewestFirst(saved, (r) => ports.attempt(r));
  if (result !== "entered") return result;
  try {
    await ports.finish();
  } catch (err) {
    // The group is held and merged; the reader pass takes its keys next time.
    // eslint-disable-next-line no-console
    console.warn("[channels] an entered channel's keys were not taken; the next pass takes them", err instanceof Error ? err.name : "");
  }
  return "entered";
}

/**
 * Take this device's publisher keys after its entry landed (the reseal
 * moved the channels service to `serverTargetEpoch`), and save the current
 * epoch in the head. Used live and after a replayed entry landed.
 */
export async function finishEntry(lineage: WasmIdentity, idHex: string, serverTargetEpoch: number): Promise<boolean> {
  const keys = await heldPublisherKeys(lineage, idHex);
  if (keys === null) return false;
  const groupEpoch = keys.seedEpoch ?? 0;
  // The channel epoch the group's epoch serves: the reseal of the entry put
  // the service at `serverTargetEpoch`.
  const offset = Math.max(0, serverTargetEpoch - groupEpoch);
  await channelsActions.updateAccessDurable(idHex, (r) => {
    if ((r.epochOffset ?? 0) === offset) return r;
    if (offset === 0) {
      const { epochOffset: _drop, ...rest } = r;
      void _drop;
      return rest as typeof r;
    }
    return { ...r, epochOffset: offset };
  });
  channelsActions.setKeys(idHex, { ...keys, seedEpoch: groupEpoch + offset });
  channelsActions.setEpoch(idHex, serverTargetEpoch);
  await channelsActions.persistPublisherGroupDurable();
  await withPublisherGroup(lineage, idHex, (g, device) => writeCurrentHead(g, device, lineage)).catch(() => false);
  // eslint-disable-next-line no-console
  console.info("[channels] entered an owned channel from its saved copy");
  return true;
}

/**
 * One entry at saved epoch `r` over wasm, the DS and the channels service.
 * The channels service epoch is read first (a failure: `"owed"`, nothing
 * built). A refused build: `"refused"`. A record that could not be written:
 * `"owed"` (the entry was abandoned, nothing left the device).
 */
async function attemptEntry(
  lineage: WasmIdentity,
  idHex: string,
  r: ReentryEpoch,
  onLanded: (serverTargetEpoch: number) => void,
): Promise<EntryAttempt> {
  const device = publisherDeviceOrNull();
  if (device === null) return "owed";
  let serverEpoch: number;
  try {
    serverEpoch = (await fetchEpoch(idHex)).epoch;
  } catch {
    return "owed";
  }
  const wasm = await loadWasm();
  let group: PublisherGroup;
  let commit: Uint8Array;
  try {
    // G-55: the commit comes back wrapped under the saved epoch's access seed,
    // so the Delivery Service never reads the entering leaf's account binding.
    const entry = wasm.PublisherGroup.enter(
      device,
      r.groupInfo,
      lineage.ed25519Seed,
      channelIdFromHex(idHex),
      r.dsAccessSeed,
    );
    try {
      commit = entry.commit;
      group = entry.takeGroup();
    } finally {
      entry.free();
    }
  } catch {
    return "refused";
  }
  ensureEntryOwnerSource();
  const serverTargetEpoch = serverEpoch + 1;
  try {
    const groupId = group.groupId();
    const record: PublisherCommitEntry = {
      id: newEntryId(),
      kind: "entry",
      channelIdHex: idHex,
      groupIdHex: hexOf(groupId),
      targetEpoch: (r.epoch + 1n).toString(),
      serverTargetEpoch,
      commitB64: base64Encode(commit),
      welcomeB64: null,
      joinerKeyB64: null,
      replyGrantB64: null,
      proofEpoch: r.epoch.toString(),
      proofSeedB64: base64Encode(r.dsAccessSeed),
      journalLost: false,
      done: [],
    };
    const outcome = await withGroupMlsLock(base64Encode(groupId), async () => {
      try {
        return await runRecordedPublisherCommit({ publisherGroup: group, identity: lineage, device, channelIdHex: idHex }, record);
      } catch {
        // The record was refused (nothing left the device, the entry was
        // abandoned), or a step after the merge failed (the record stays
        // and the replay finishes it): nothing more this pass.
        return "owed" as const;
      }
    });
    if (outcome === "landed") {
      onLanded(serverTargetEpoch);
      return "entered";
    }
    return outcome === "lost" ? "lost" : "owed";
  } finally {
    try {
      group.free();
    } catch {
      /* already freed */
    }
  }
}

/** The entry ports of `idHex` over wasm and the services. */
export function wasmEntryPorts(lineage: WasmIdentity, idHex: string): EntryPorts {
  let landed: number | null = null;
  return {
    async readSaved() {
      try {
        return await withChannelHeadStore(lineage, channelIdFromHex(idHex), async (store) => {
          const current = await store.read();
          return current.kind === "head" ? keepTwoNewest(current.saved, []) : [];
        });
      } catch {
        return null;
      }
    },
    attempt: (r) =>
      attemptEntry(lineage, idHex, r, (target) => {
        landed = target;
      }),
    async finish() {
      if (landed !== null) await finishEntry(lineage, idHex, landed);
    },
  };
}

/**
 * ADR-0090 section 3 for an entry: a group this device holds for an owned
 * channel with a staged commit no recorded entry names (a reload between
 * `PublisherGroup.enter` and its record): nothing of it was sent, so it is
 * abandoned, which deletes what the entry wrote. Never throws.
 */
export async function abandonUnrecordedEntry(lineage: WasmIdentity, idHex: string): Promise<boolean> {
  if (channelsActions.pendingPublisherCommits().some((e) => e.channelIdHex === idHex)) return false;
  const device = publisherDeviceOrNull();
  if (device === null) return false;
  try {
    const wasm = await loadWasm();
    const held = wasm.PublisherGroup.load(device, lineage.ed25519Seed, channelIdFromHex(idHex));
    if (held === undefined) return false;
    try {
      if (!held.hasStagedCommit()) return false;
      held.abandonStagedCommit(device);
    } finally {
      held.free();
    }
    await channelsActions.persistPublisherGroupDurable().catch(() => {
      channelsActions.notePublisherGroupChanged();
    });
    // eslint-disable-next-line no-console
    console.info("[channels] an unrecorded entry was abandoned");
    return true;
  } catch {
    return false;
  }
}

/** The whole entry for one channel over wasm and the services. Never throws. */
export function enterOwnedChannelFromSaved(lineage: WasmIdentity, idHex: string): Promise<EntryResult> {
  return enterOwnedChannel(wasmEntryPorts(lineage, idHex));
}
