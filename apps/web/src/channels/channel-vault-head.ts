// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0064 section 7, amendment of 2026-11-14 — the owned channel's vault
 * head, on web.
 *
 * Each owned channel has a vault at the channel's conversation ref (kind 3,
 * the 16-byte channel id) under the owner account's vault root. Its head is a
 * `ConversationHead` whose `reentry` holds the two newest PublisherGroup
 * epochs (GroupInfo with the ratchet tree, the epoch's Delivery Service
 * access seed), and nothing else.
 *
 * - **Writer** ({@link writeChannelHead}): every owner device that holds the
 *   group writes the head before it sends a PublisherGroup commit (the epoch
 *   the staged commit moves to, {@link writeStagedHead}) and after it applied
 *   a commit another owner device made (the current epoch,
 *   {@link writeCurrentHead}). A `409` reads the head again, keeps the two
 *   newest of both, and writes once more. A write never throws and never
 *   holds a commit beyond that one attempt: the head only lets a device that
 *   lost everything come back, and a stale head degrades to the waiting
 *   line, never to a wrong group.
 * - **Reader** ({@link readSavedEpochs}): the saved epochs, newest first;
 *   none (no head, an unclaimed vault, a failure) is an empty list.
 * - **Entry order** ({@link enterNewestFirst}): newest saved epoch first; a
 *   lost or refused entry tries the older one; an entry the Delivery Service
 *   did not answer stops (the recorded commit is re-sent later).
 *
 * The vault keys come from the LINEAGE identity (the owner account seed the
 * PublisherGroup is created and loaded with, and the root its founding
 * claims with), so the head survives a key rotation.
 *
 * Nothing here logs a key, a seed, an id or a GroupInfo.
 */

import { loadWasm, type Wasm } from "../wasm";
import { VaultClient, type HeadWrite, type VaultSigner } from "../storage/vault-client";
import type { WasmIdentity } from "./composer";

type PublisherGroup = InstanceType<Wasm["PublisherGroup"]>;

/** One saved epoch of re-entry material. `dsAccessSeed` is secret. */
export interface ReentryEpoch {
  readonly epoch: bigint;
  readonly dsAccessSeed: Uint8Array;
  readonly groupInfo: Uint8Array;
}

/** The conversation-ref kind of a channel (ADR-0058). */
export const CHANNEL_REF_KIND = 3;

/** How many epochs a head keeps (`vaultMaxReentryEpochs()`, 2). */
export const HEAD_EPOCHS = 2;

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

/**
 * The two newest epochs of `existing` and `incoming`, newest first, one per
 * epoch (`incoming` wins an epoch both name: it is what this device has
 * just seen). Pure.
 */
export function keepTwoNewest(existing: readonly ReentryEpoch[], incoming: readonly ReentryEpoch[]): ReentryEpoch[] {
  const byEpoch = new Map<bigint, ReentryEpoch>();
  for (const r of existing) byEpoch.set(r.epoch, r);
  for (const r of incoming) byEpoch.set(r.epoch, r);
  return [...byEpoch.values()].sort((a, b) => (a.epoch > b.epoch ? -1 : a.epoch < b.epoch ? 1 : 0)).slice(0, HEAD_EPOCHS);
}

/** Whether two saved sets say the same thing (no write needed). Pure. */
export function sameSaved(a: readonly ReentryEpoch[], b: readonly ReentryEpoch[]): boolean {
  if (a.length !== b.length) return false;
  return a.every(
    (r, i) =>
      r.epoch === b[i]?.epoch && sameBytes(r.groupInfo, b[i].groupInfo) && sameBytes(r.dsAccessSeed, b[i].dsAccessSeed),
  );
}

/** How a write treats the saved epochs already in the head. */
export type HeadWriteMode = "merge" | "replace";

/** One channel's head on the service, sealed and opened; the tests inject a
 *  fake. */
export interface ChannelHeadStore {
  /** The head: its version and saved epochs (newest first), or none / an
   *  unclaimed vault. Throws on a network failure or another refusal. */
  read(): Promise<
    | { readonly kind: "head"; readonly version: bigint; readonly saved: ReentryEpoch[] }
    | { readonly kind: "none" }
    | { readonly kind: "unclaimed" }
  >;
  /** Write `saved` as the head at `version`. */
  write(version: bigint, saved: readonly ReentryEpoch[]): Promise<HeadWrite>;
  /** Claim the vault (one `vault-claim` token). */
  claim(): Promise<void>;
}

/**
 * Save `incoming` in the head: read, merge (two newest), write at the next
 * version; a vault nobody claimed is claimed first; a `409` reads, merges and
 * writes once more. Returns whether the head now carries it. Never throws.
 */
export async function writeChannelHead(
  store: ChannelHeadStore,
  incoming: ReentryEpoch,
  /** `"replace"` (decision A re-founded the group): the head holds the new
   *  group's epoch only. A re-founded group starts at a low epoch, so a merge
   *  by epoch number would keep the dead group's GroupInfos. */
  mode: HeadWriteMode = "merge",
): Promise<boolean> {
  try {
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const current = await store.read();
      let version = 0n;
      let existing: ReentryEpoch[] = [];
      if (current.kind === "unclaimed") {
        await store.claim();
      } else if (current.kind === "head") {
        version = current.version;
        existing = current.saved;
      }
      const merged = mode === "replace" ? [incoming] : keepTwoNewest(existing, [incoming]);
      if (!merged.some((r) => r.epoch === incoming.epoch)) {
        // The head already holds two newer epochs: nothing of this one is
        // worth saving.
        return false;
      }
      if (current.kind === "head" && sameSaved(merged, keepTwoNewest(existing, []))) return true;
      const answer = await store.write(version + 1n, merged);
      if (answer === "written") return true;
      if (answer === "unclaimed") return false;
      // "conflict": another device wrote between the read and the write.
    }
    return false;
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] the channel's saved copy was not written", err instanceof Error ? err.name : "");
    return false;
  }
}

/** The saved epochs, newest first; empty when there is no head, the vault is
 *  unclaimed, or the read failed. Never throws. */
export async function readSavedEpochs(store: ChannelHeadStore): Promise<ReentryEpoch[]> {
  try {
    const current = await store.read();
    return current.kind === "head" ? keepTwoNewest(current.saved, []) : [];
  } catch {
    return [];
  }
}

/** What one entry attempt at a saved epoch ended in. */
export type EntryAttempt =
  /** The DS took the commit; the group is held here now. */
  | "entered"
  /** Another commit owns the epoch (409) or the DS refused it for good. */
  | "lost"
  /** The core refused to build the entry from this GroupInfo. */
  | "refused"
  /** The DS did not answer; the recorded entry is re-sent later. */
  | "owed";

/** What an entry pass ended in. */
export type EntryResult = "entered" | "owed" | "none";

/**
 * Enter from the saved epochs, newest first: an epoch that is lost or
 * refused tries the older one; an owed entry stops (nothing older is tried
 * while one awaits its answer); none left is `"none"` (the waiting line, or
 * decision A where it applies). Never throws: a throwing attempt counts as
 * refused.
 */
export async function enterNewestFirst(
  saved: readonly ReentryEpoch[],
  attempt: (r: ReentryEpoch) => Promise<EntryAttempt>,
): Promise<EntryResult> {
  for (const r of keepTwoNewest(saved, [])) {
    let outcome: EntryAttempt;
    try {
      outcome = await attempt(r);
    } catch {
      outcome = "refused";
    }
    if (outcome === "entered") return "entered";
    if (outcome === "owed") return "owed";
  }
  return "none";
}

// ---------------------------------------------------------------------------
// Over wasm and the storage service
// ---------------------------------------------------------------------------

function copyReentry(r: InstanceType<Wasm["VaultReentryEpoch"]>): ReentryEpoch {
  try {
    return { epoch: r.epoch, dsAccessSeed: r.dsAccessSeed.slice(), groupInfo: r.groupInfo.slice() };
  } finally {
    r.free();
  }
}

/**
 * The head store of channel `channelId` under `vaultIdentity`'s vault root
 * (the lineage identity). The caller frees nothing: the keys live until
 * `release()`.
 */
export function wasmChannelHeadStore(
  wasm: Wasm,
  vaultIdentity: WasmIdentity,
  channelId: Uint8Array,
  client: VaultClient = new VaultClient(),
): ChannelHeadStore & { release(): void } {
  const keys = wasm.vaultKeys(vaultIdentity);
  const ref = wasm.VaultRef.conversation(CHANNEL_REF_KIND, channelId);
  const signer: VaultSigner = {
    vaultId: () => keys.vaultId(ref),
    writePublicKey: () => keys.writePublicKey(ref),
    signRequest: (method, path, body) => keys.signRequest(ref, method, path, body),
  };
  return {
    async read() {
      const r = await client.getHead(signer);
      if (r.kind !== "head") return r;
      const plain = keys.openHead(ref, r.head.version, r.head.sealed);
      const head = wasm.decodeConversationHead(plain);
      try {
        return { kind: "head", version: r.head.version, saved: head.reentry.map(copyReentry) };
      } finally {
        head.free();
      }
    },
    async write(version, saved) {
      const head = new wasm.VaultConversationHead(CHANNEL_REF_KIND, channelId, 0, "", 0, BigInt(Date.now()));
      let plain: Uint8Array;
      try {
        head.setReentry(saved.map((r) => new wasm.VaultReentryEpoch(r.epoch, r.dsAccessSeed, r.groupInfo)));
        plain = wasm.encodeConversationHead(head);
      } finally {
        head.free();
      }
      const sealed = keys.sealHead(ref, version, plain);
      plain.fill(0);
      return client.putHead(signer, version, sealed);
    },
    claim: () => client.claim(signer),
    release() {
      try {
        ref.free();
      } catch {
        /* already freed */
      }
      try {
        keys.free();
      } catch {
        /* already freed */
      }
    },
  };
}

/** Run `fn` with channel `channelIdHex`'s head store. */
export async function withChannelHeadStore<T>(
  vaultIdentity: WasmIdentity,
  channelId: Uint8Array,
  fn: (store: ChannelHeadStore) => Promise<T>,
): Promise<T> {
  const wasm = await loadWasm();
  const store = wasmChannelHeadStore(wasm, vaultIdentity, channelId);
  try {
    return await fn(store);
  } finally {
    store.release();
  }
}

/** How a head write reaches the service; the tests replace it. */
export type HeadWriter = (
  vaultIdentity: WasmIdentity,
  channelId: Uint8Array,
  incoming: ReentryEpoch,
  mode: HeadWriteMode,
) => Promise<boolean>;

const defaultHeadWriter: HeadWriter = (vaultIdentity, channelId, incoming, mode) =>
  withChannelHeadStore(vaultIdentity, channelId, (store) => writeChannelHead(store, incoming, mode)).catch(() => false);

let headWriter: HeadWriter = defaultHeadWriter;

/** Tests: replace the head writer; `null` puts the default back. */
export function setHeadWriterForTests(w: HeadWriter | null): void {
  headWriter = w ?? defaultHeadWriter;
}

function isOwnerDevice(group: PublisherGroup, device: WasmIdentity): boolean {
  try {
    return group.isOwnerDevice(device);
  } catch {
    return false;
  }
}

/**
 * Before a staged PublisherGroup commit is sent: save the epoch it moves to
 * (`stagedGroupInfo`, `stagedDsAccessSeed`, `epoch + 1`). Only an owner
 * device writes. Never throws.
 */
export async function writeStagedHead(
  group: PublisherGroup,
  device: WasmIdentity,
  vaultIdentity: WasmIdentity,
): Promise<boolean> {
  let incoming: ReentryEpoch | null = null;
  let channelId: Uint8Array;
  try {
    if (!isOwnerDevice(group, device)) return false;
    const groupInfo = group.stagedGroupInfo(device);
    const seed = group.stagedDsAccessSeed(device);
    if (groupInfo === undefined || seed === undefined) return false;
    incoming = { epoch: group.epoch() + 1n, dsAccessSeed: seed, groupInfo };
    channelId = group.channelId();
  } catch {
    return false;
  }
  try {
    return await headWriter(vaultIdentity, channelId, incoming, "merge");
  } finally {
    incoming.dsAccessSeed.fill(0);
  }
}

/**
 * After a commit another owner device made was applied here (or this device
 * entered, or founded the group): save the current epoch. Only an owner
 * device writes. Never throws.
 */
export async function writeCurrentHead(
  group: PublisherGroup,
  device: WasmIdentity,
  vaultIdentity: WasmIdentity,
  /** `"replace"` after decision A re-founded the group. */
  mode: HeadWriteMode = "merge",
): Promise<boolean> {
  let incoming: ReentryEpoch | null = null;
  let channelId: Uint8Array;
  try {
    if (!isOwnerDevice(group, device) || !group.isActive() || group.hasStagedCommit()) return false;
    incoming = { epoch: group.epoch(), dsAccessSeed: group.dsAccessSeed(device), groupInfo: group.groupInfo(device) };
    channelId = group.channelId();
  } catch {
    return false;
  }
  try {
    return await headWriter(vaultIdentity, channelId, incoming, mode);
  } finally {
    incoming.dsAccessSeed.fill(0);
  }
}

// ---------------------------------------------------------------------------
// Deleting the vault (ADR-0064 section 1: the client deletes its vaults)
// ---------------------------------------------------------------------------

/** How long account deletion waits for the channel vaults before it goes on
 *  (chosen, not measured). */
export const ACCOUNT_VAULT_DELETE_BUDGET_MS = 5_000;

/** How a vault is deleted; the tests replace it. Resolves whether the vault
 *  is gone (deleted, or there was none). */
export type VaultDeleter = (vaultIdentity: WasmIdentity, channelId: Uint8Array) => Promise<boolean>;

const defaultVaultDeleter: VaultDeleter = async (vaultIdentity, channelId) => {
  const wasm = await loadWasm();
  const keys = wasm.vaultKeys(vaultIdentity);
  const ref = wasm.VaultRef.conversation(CHANNEL_REF_KIND, channelId);
  try {
    await new VaultClient().deleteVault({
      vaultId: () => keys.vaultId(ref),
      writePublicKey: () => keys.writePublicKey(ref),
      signRequest: (method, path, body) => keys.signRequest(ref, method, path, body),
    });
    return true;
  } finally {
    ref.free();
    keys.free();
  }
};

let vaultDeleter: VaultDeleter = defaultVaultDeleter;

/** Tests: replace the vault deleter; `null` puts the default back. */
export function setVaultDeleterForTests(d: VaultDeleter | null): void {
  vaultDeleter = d ?? defaultVaultDeleter;
}

/**
 * Delete one owned channel's vault, best effort: after the channel itself
 * was deleted, or before the account is. Never throws; a failure is logged
 * without identifiers. Returns whether the vault is gone.
 */
export async function deleteChannelVault(vaultIdentity: WasmIdentity, channelId: Uint8Array): Promise<boolean> {
  try {
    return await vaultDeleter(vaultIdentity, channelId);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] a channel's saved copy was not deleted", err instanceof Error ? err.name : "");
    return false;
  }
}

/** Who owns which channels, and the lineage identity the vaults are named
 *  under: installed by `ChannelPublisherLoops` for the signed-in session. */
export interface OwnedChannelVaults {
  readonly lineage: () => WasmIdentity | null;
  /** The 32-char hex ids of the channels the account's register names as
   *  Owner. */
  readonly ownedChannelIds: () => readonly string[];
}

let owned: OwnedChannelVaults | null = null;

/** Install (or clear, with `null`) the session's owned-channel source. */
export function installOwnedChannelVaults(source: OwnedChannelVaults | null): void {
  owned = source;
}

function channelIdBytes(idHex: string): Uint8Array | null {
  if (!/^[0-9a-f]{32}$/i.test(idHex)) return null;
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i += 1) out[i] = Number.parseInt(idHex.slice(2 * i, 2 * i + 2), 16);
  return out;
}

/**
 * Before the account is deleted: delete the vault of every channel the
 * account owns, in parallel, waiting at most `budgetMs` so a slow storage
 * service cannot hold the deletion. A guest, or a session with no source
 * installed, owns none. Never throws. Returns how many vaults are known gone.
 */
export async function deleteOwnedChannelVaults(budgetMs: number = ACCOUNT_VAULT_DELETE_BUDGET_MS): Promise<number> {
  const source = owned;
  if (source === null) return 0;
  let lineage: WasmIdentity | null;
  let ids: readonly string[];
  try {
    lineage = source.lineage();
    ids = source.ownedChannelIds();
  } catch {
    return 0;
  }
  if (lineage === null || ids.length === 0) return 0;
  const vaultIdentity = lineage;
  let gone = 0;
  const all = Promise.all(
    ids.map(async (idHex) => {
      const id = channelIdBytes(idHex);
      if (id !== null && (await deleteChannelVault(vaultIdentity, id))) gone += 1;
    }),
  );
  let timer: ReturnType<typeof setTimeout> | undefined;
  const budget = new Promise<void>((resolve) => {
    timer = setTimeout(resolve, budgetMs);
  });
  try {
    await Promise.race([all, budget]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
  return gone;
}
