// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 - the owned-channels list bound to the channels state
 * and the services: what the publisher pass runs
 * ({@link runOwnedChannelsPass}) and what the account's sign-up, sign-in,
 * passkey, reset and deletion paths call (re-exported from `index.ts`).
 *
 * ## The session
 *
 * A device with the account keys opens the keys lock once per session and
 * keeps the list key `R`, the server half `S` and the list as last read in
 * memory ({@link dropOwnedChannelsSession} zeroes them). Nothing of it is
 * written to this device's storage. An account with no keys lock (made
 * before this build, or whose creation has not landed yet) is asked again
 * after {@link ABSENT_RECHECK_MS}, and at once when a caller makes the
 * vaults.
 *
 * ## The pass (a device that holds the keys)
 *
 * Nothing is asked of the storage service while this device owns no channel
 * and owes the list nothing.
 *
 * 1. A retake owed after a key reset on this device continues
 *    (`owned-retake.ts`).
 * 2. Every channel this device owns registers the recovery key derived from
 *    `R` (`PUT /v1/channels/{id}/recovery-key`), once per device and channel.
 * 3. A retaken channel whose PublisherGroup is founded again and whose
 *    record this device has read hands the record to the publisher pass,
 *    which posts it again under the new epoch.
 * 4. The list is brought in line with what this device holds: an entry per
 *    channel it owns (the read key of a public one, the admission seeds, the
 *    newest epoch seeds of epochs the channel has reached), and the entries
 *    of channels it deleted taken out. One read-merge-write, and only when
 *    something differs from the list as last read.
 *
 * Every exported function here never throws: a failure is an outcome the
 * caller tries again at its next landing, and the pass runs again by
 * itself. Calls run one at a time.
 *
 * Nothing here logs a key, a seed, a root or an id.
 */
import { base64Decode, base64Encode } from "../../auth/account-id";
import { loadWasm, type Wasm } from "../../wasm";
import { channelsActions, channelsState } from "../../state/channels-state";
import {
  channelOwnerKeyOf,
  listSubscriberPage,
  ownerProofIdentity,
  putChannelOwnerByRecovery,
  putChannelRecoveryKey,
} from "../api";
import { channelRotatesHere } from "../channel-access";
import { notifyChannelReturnsHeld } from "../channel-return-runtime";
import { channelSubscriptionPort } from "../channel-subscription-register";
import { channelIdFromHex, freshSubscriberKeys } from "../composer";
import { markFoundedAlone } from "../founded-alone-marker";
import { highestSeq, markRecoveredChannel } from "../recovered-channels";
import { loadRotationState } from "../rotation-state-store";
import {
  channelsOwingRecoveryKey,
  clearOwnedChannelsStoredFacts,
  factsStorage,
  markRecoveryRegistered,
  oweRetakenRecord,
  readOwedListDeletes,
  readRecoveryRegistered,
  readRetake,
  readRetakenRecordsOwed,
  settleListDeletes,
  settleRetakenRecord,
  type FactsStorage,
} from "./owned-facts";
import {
  hexOf,
  listWritePlan,
  trimEntry,
  withChannelChanges,
  type OwnedEntry,
  type OwnedList,
} from "./owned-list";
import {
  continueRetake,
  retakeAfterReset,
  type ResetSealedBy,
  type RetakeOutcome,
  type RetakePorts,
  type RetakenChannel,
} from "./owned-retake";
import { OwnedChannelsStore, OwnedVaultError, type OpenedOwned, type OwnedVaultPort } from "./owned-store";
import { noteOwnedChannelsChanged } from "./owned-wake";
import { channelHandleSeed } from "../../chat/group-handle";
import { openHandleOwed, owedHandleFor, recordOwedHandle } from "../../chat/handle-owed";

type Identity = InstanceType<Wasm["Identity"]>;

/** How long an account with no keys lock is left alone before the lock is
 *  asked for again (chosen, not measured). */
export const ABSENT_RECHECK_MS = 10 * 60_000;

/** How long account deletion waits for the list and its locks (chosen). */
export const OWNED_VAULTS_DELETE_BUDGET_MS = 5_000;

// ----------------------------------------------------------------------------
// The store and the session
// ----------------------------------------------------------------------------

let vaultPort: OwnedVaultPort | undefined;

/** Tests: the vault service the store talks to; `null` puts the real one
 *  back. */
export function setOwnedVaultPortForTests(port: OwnedVaultPort | null): void {
  vaultPort = port ?? undefined;
}

async function store(): Promise<{ wasm: Wasm; store: OwnedChannelsStore }> {
  const wasm = await loadWasm();
  return { wasm, store: new OwnedChannelsStore(wasm, vaultPort) };
}

interface Session {
  readonly key: string;
  readonly accountIdB64: string;
  readonly listKey: Uint8Array;
  readonly serverHalf: Uint8Array | null;
  list: OwnedList;
}

let session: Session | null = null;
let absent: { readonly key: string; readonly untilMs: number } | null = null;

function sessionKey(accountIdB64: string, lineage: Identity): string {
  return `${accountIdB64}:${hexOf(lineage.signaturePublicKey)}`;
}

/** Forget `R`, `S` and the list held in memory (sign-out, another account). */
export function dropOwnedChannelsSession(): void {
  if (session !== null) {
    session.listKey.fill(0);
    session.serverHalf?.fill(0);
  }
  session = null;
  absent = null;
}

function adopt(accountIdB64: string, lineage: Identity, opened: OpenedOwned): Session {
  const key = sessionKey(accountIdB64, lineage);
  if (session !== null && session.key !== key) dropOwnedChannelsSession();
  absent = null;
  session = {
    key,
    accountIdB64,
    listKey: opened.listKey.slice(),
    serverHalf: opened.serverHalf === null ? (session?.serverHalf ?? null) : opened.serverHalf.slice(),
    list: opened.list,
  };
  return session;
}

/** The session of this account and lineage: the one held, else the keys
 *  lock opened now; `null` when the account has none. Throws on a network
 *  failure. */
async function openSession(s: OwnedChannelsStore, accountIdB64: string, lineage: Identity): Promise<Session | null> {
  const key = sessionKey(accountIdB64, lineage);
  if (session !== null && session.key === key) return session;
  if (absent !== null && absent.key === key && Date.now() < absent.untilMs) return null;
  const opened = await s.openWithKeys(lineage);
  if (opened === "absent") {
    absent = { key, untilMs: Date.now() + ABSENT_RECHECK_MS };
    return null;
  }
  return adopt(accountIdB64, lineage, opened);
}

let queue: Promise<unknown> = Promise.resolve();

/** One owned-channels operation at a time on this device. */
function serial<T>(fn: () => Promise<T>): Promise<T> {
  const next = queue.then(fn, fn);
  queue = next.catch(() => undefined);
  return next;
}

function warn(what: string, err: unknown): void {
  // eslint-disable-next-line no-console
  console.warn(`[channels] ${what}`, err instanceof Error ? err.name : "");
}

// ----------------------------------------------------------------------------
// What this device lists
// ----------------------------------------------------------------------------

function b64(bytes: Uint8Array): string {
  return base64Encode(bytes);
}

/**
 * The list entry of a channel this device owns, read off its state: the
 * read key of a channel that is public (and only then: a retake treats an
 * entry with a read key as public), every admission seed held, and the seeds of
 * the epochs the channel has reached (a seed of an epoch whose reseal has
 * not landed is not listed: the channel may never serve it). Trimmed to the
 * entry's bounds.
 */
export function ownedEntryOf(idHex: string, handleSeed: Uint8Array | null = null): OwnedEntry {
  const access = channelsActions.accessOf(idHex);
  const keys = channelsState.keysByChannel().get(idHex);
  const reached = channelsState.epochByChannel().get(idHex);
  const held = new Map<number, Uint8Array>(channelsState.subscribedSeedsByChannel().get(idHex) ?? []);
  if (keys?.seedEpoch !== undefined && !held.has(keys.seedEpoch)) held.set(keys.seedEpoch, keys.seed);
  const meta = channelsState.metaByChannel().get(idHex);
  // The read key says "public". It is listed only while neither this
  // device's setting nor the channel's newest record says private: another
  // owner device may have made the channel private since.
  const recordSaysPrivate = (meta?.handle?.isPublic ?? meta?.listed) === false;
  const readKey = (() => {
    if (access.visibility !== "public" || recordSaysPrivate || access.readKeyB64 === null) return null;
    try {
      return base64Decode(access.readKeyB64);
    } catch {
      return null;
    }
  })();
  const admission = access.admissionSeeds.flatMap((s) => {
    try {
      return [{ version: s.version, seed: base64Decode(s.seedB64) }];
    } catch {
      return [];
    }
  });
  return trimEntry({
    idHex: idHex.toLowerCase(),
    readKey,
    admission,
    epochs: [...held]
      .filter(([epoch]) => reached === undefined || epoch <= reached)
      .map(([epoch, seed]) => ({ epoch, seed })),
    handleSeed,
  });
}

/**
 * The handle write seed a channel's list entry carries (section 7 item 4):
 * the seed discovery holds its `@name` under. While a retake still owes the
 * `rekey` that moves the name, that is the listed (old) seed; otherwise the
 * one this device derives (`channelHandleSeed`, ADR-0087 section 3). `null`
 * when it cannot be derived here.
 */
export function listedHandleSeedOf(
  wasm: Wasm,
  idHex: string,
  lineage: Identity,
  current: Identity,
): Uint8Array | null {
  const owed = owedHandleFor("channel", idHex)?.rekeyFromSeed;
  if (owed !== undefined) {
    try {
      const seed = base64Decode(owed);
      if (seed.length === 32) return seed;
    } catch {
      // Not a seed: the derived one is listed.
    }
  }
  try {
    return channelHandleSeed(wasm, lineage, current, channelIdFromHex(idHex));
  } catch {
    return null;
  }
}

function rotatedAtOf(accountIdB64: string): (idHex: string) => number {
  return (idHex) => {
    try {
      return Number(loadRotationState(idHex, accountIdB64)?.lastRotationMs ?? 0n);
    } catch {
      return 0;
    }
  };
}

function ownedHere(): string[] {
  return [...channelsState.keysByChannel().keys()].filter((id) => channelRotatesHere(id)).map((id) => id.toLowerCase());
}

// ----------------------------------------------------------------------------
// The pass
// ----------------------------------------------------------------------------

/** What the pass needs from the publisher loops. */
export interface OwnedPassContext {
  readonly accountIdB64: string;
  /** The lineage account identity (the keys lock is named by its seed). */
  readonly lineage: Identity;
  /** The current account identity (owner proofs are signed with it). */
  readonly current: Identity;
}

async function registerRecoveryKeys(wasm: Wasm, s: Session, owner: Identity): Promise<void> {
  const storage = factsStorage();
  const owing = channelsOwingRecoveryKey(ownedHere(), readRecoveryRegistered(storage, s.accountIdB64));
  for (const idHex of owing) {
    const key = wasm.ChannelRecoveryKey.fromListKey(s.listKey, channelIdFromHex(idHex));
    try {
      const status = await putChannelRecoveryKey(idHex, owner, key.verifyingKey());
      if (status === 204) markRecoveryRegistered(storage, s.accountIdB64, idHex);
    } finally {
      key.free();
    }
  }
}

/**
 * Item 8, "re-issues the channel metadata": a retaken channel's record is
 * posted again under the re-founded group's epoch, so a reader let in from
 * now on (who opens that epoch only) sees the channel's name. It is owed
 * until the group is founded again AND this device holds the record (read
 * from the posts the listed epoch seeds open); then the access record's
 * owed mark is set and the publisher pass posts it (`owed-record.ts`).
 * Marking it owed earlier would settle the mark with nothing posted.
 */
async function oweRetakenRecords(accountIdB64: string): Promise<void> {
  const storage = factsStorage();
  for (const idHex of readRetakenRecordsOwed(storage, accountIdB64)) {
    if (!channelsState.channels().some((c) => c.id === idHex)) {
      settleRetakenRecord(storage, accountIdB64, idHex);
      continue;
    }
    if (!channelsState.keysByChannel().has(idHex) || channelsState.metaByChannel().get(idHex) === undefined) continue;
    await channelsActions.updateAccessDurable(idHex, (r) => ({ ...r, recordOwed: true }));
    settleRetakenRecord(storage, accountIdB64, idHex);
  }
}

async function syncList(wasm: Wasm, s: OwnedChannelsStore, sess: Session, ctx: OwnedPassContext): Promise<void> {
  const storage = factsStorage();
  const owed = readOwedListDeletes(storage, sess.accountIdB64);
  const rotatedAt = rotatedAtOf(sess.accountIdB64);
  // The owed `@name` writes say which seed a retaken channel's name is
  // still held under.
  await openHandleOwed(sess.accountIdB64);
  const desired = ownedHere().map((id) => ownedEntryOf(id, listedHandleSeedOf(wasm, id, ctx.lineage, ctx.current)));
  const plan = listWritePlan(sess.list, desired, owed, rotatedAt);
  if (plan.entries.length > 0 || plan.deletes.length > 0) {
    const written = await s.writeList(sess.listKey, (current) =>
      withChannelChanges(wasm, current, plan.entries, owed, rotatedAt),
    );
    sess.list = written.list;
  }
  // A delete stays owed while this device still holds the channel (its local
  // wipe follows the delete on the service): settled earlier, a pass in
  // between would list the channel again.
  const stillHeld = new Set(ownedHere());
  const settled = owed.filter((id) => !stillHeld.has(id));
  if (settled.length > 0) settleListDeletes(storage, sess.accountIdB64, settled);
}

async function pass(ctx: OwnedPassContext): Promise<void> {
  const storage = factsStorage();
  // Most accounts own no channel: with nothing owned here and nothing owed,
  // the storage service is not asked at all.
  if (
    ownedHere().length === 0 &&
    readRetake(storage, ctx.accountIdB64) === null &&
    readOwedListDeletes(storage, ctx.accountIdB64).length === 0
  ) {
    return;
  }
  const { wasm, store: s } = await store();
  const sess = await openSession(s, ctx.accountIdB64, ctx.lineage);
  if (sess === null) return;
  const owner = ownerProofIdentity(ctx.current);
  if (readRetake(storage, ctx.accountIdB64) !== null) {
    try {
      // The reset deleted the passkeys; their locks go with them.
      if (sess.list.passkeyLocks.length > 0) {
        await s.removeAllPasskeyLocks(sess.listKey);
        const read = await s.readList(sess.listKey);
        if (read !== null) sess.list = read.list;
      }
      const continued = await continueRetake(
        wasm,
        s,
        storage,
        ctx.accountIdB64,
        { listKey: sess.listKey, serverHalf: sess.serverHalf, list: sess.list, listVersion: 0n },
        retakePorts(ctx.accountIdB64, owner, ctx.lineage, ctx.current),
      );
      sess.list = continued.list;
    } catch (err) {
      warn("the account's channels were not all taken back yet", err);
    }
  }
  try {
    await oweRetakenRecords(ctx.accountIdB64);
  } catch (err) {
    warn("a retaken channel's record was not handed over; tried again next pass", err);
  }
  try {
    await registerRecoveryKeys(wasm, sess, owner);
  } catch (err) {
    warn("a channel's recovery key was not registered; tried again next pass", err);
  }
  try {
    await syncList(wasm, s, sess, ctx);
  } catch (err) {
    warn("the account's channel list was not written; tried again next pass", err);
  }
}

/**
 * The publisher pass's owned-channels step, and what a change noted through
 * `owned-wake.ts` runs. Never throws.
 */
export function runOwnedChannelsPass(ctx: OwnedPassContext): Promise<void> {
  return serial(() => pass(ctx)).catch((err: unknown) => warn("the account's channel list was not read", err));
}

// ----------------------------------------------------------------------------
// The retake's ports over the channels state
// ----------------------------------------------------------------------------

async function listedThroughSeq(idHex: string, identity: Identity): Promise<number> {
  let through = 0;
  for (let after = 0; ; ) {
    const page = await listSubscriberPage(idHex, identity, after);
    through = highestSeq(page.entries, through);
    if (page.entries.length === 0 || page.nextAfter <= after) break;
    after = page.nextAfter;
  }
  return through;
}

/**
 * A channel whose owner key is the new account's now is put on this device
 * from its list entry: listed, its epoch seeds readable, its access record
 * carrying the entry's read key and admission seeds, marked recovered (the
 * publisher pass founds its PublisherGroup again at the id the new seed
 * derives, ADR-0090 decision A, reseals and re-issues its record) and
 * recorded as Owner in the account's register. Idempotent. Throws when a
 * step has to be tried again (the register is not loaded yet, the
 * subscriber list could not be read).
 */
async function takeRetakenChannel(
  accountIdB64: string,
  identity: Identity,
  handleKeys: { readonly lineage: Identity; readonly current: Identity },
  entry: OwnedEntry,
  plan: RetakenChannel,
): Promise<void> {
  const idHex = entry.idHex;
  const port = channelSubscriptionPort();
  if (port === null) throw new Error("the account's register is not loaded yet");
  // Every reader the service lists stays let in when the entry carries
  // nothing to judge a row with; a private channel's rows are judged again
  // by their tickets under the listed admission seeds (mark 0).
  const throughSeq = plan.admission === "everyone-listed" ? await listedThroughSeq(idHex, identity) : 0;
  if (!channelsState.channels().some((c) => c.id === idHex)) {
    channelsActions.addChannel({ id: idHex, localName: "", localDescription: "", addedAtMs: Date.now() });
  }
  for (const e of entry.epochs) {
    if (channelsActions.currentSeed(idHex, e.epoch) === undefined) {
      channelsActions.setSubscribedSeedForEpoch(idHex, e.epoch, e.seed.slice());
    }
  }
  markRecoveredChannel(accountIdB64, idHex, throughSeq);
  const wasm = await loadWasm();
  const listed = entry.admission.map((a) => ({ version: a.version, seedB64: b64(a.seed) }));
  await channelsActions.updateAccessDurable(idHex, (r) => {
    const seeds = [...r.admissionSeeds];
    for (const s of listed) if (!seeds.some((m) => m.seedB64 === s.seedB64)) seeds.push(s);
    // A channel of unknown type with no seed listed: a fresh one verifies
    // the tickets of invitations made from now on.
    if (seeds.length === 0 && plan.visibility !== "public") {
      seeds.push({ version: 1, seedB64: b64(wasm.generateChannelAdmissionSeed()) });
    }
    return {
      ...r,
      visibility: plan.visibility ?? r.visibility,
      readKeyB64: entry.readKey === null ? r.readKeyB64 : b64(entry.readKey),
      admittedThroughSeq: Math.max(r.admittedThroughSeq, throughSeq),
      admissionMarkSet: true as const,
      admissionSeeds: seeds,
    };
  });
  const held = port.get(idHex);
  if (!(held?.subscribed === true && held.role === "owner")) {
    const fresh = await freshSubscriberKeys();
    await port.set(idHex, {
      role: "owner",
      subscriberSecret: fresh.secret,
      readKey: new Uint8Array(0),
      publisherGroupIdHex: "",
      ownerAccountKey: new Uint8Array(0),
      name: "",
      handle: "",
    });
  }
  oweRetakenRecord(factsStorage(), accountIdB64, idHex);
  await oweRetakenHandleRekey(wasm, accountIdB64, entry, handleKeys);
  // This device holds the account's DeviceGroup alone after the reset: the
  // pass may found the channel's group again here.
  markFoundedAlone(accountIdB64);
  // The re-founding runs in the publisher pass: now, not a minute later.
  notifyChannelReturnsHeld();
}

/**
 * Section 7 item 4: the channel's `@name` is held under the handle key its
 * list entry carried (the old account's). A `rekey` to the key the new root
 * derives is recorded as owed (durably, in the sealed per-account row of
 * `handle-owed.ts`) and run by the `@name` runtime until it lands, the
 * pattern of a rotation's owed rekey. Nothing is owed when the entry lists
 * no seed or the seed is already the new one. Throws when it could not be
 * recorded (the channel is then taken again at the next pass).
 */
async function oweRetakenHandleRekey(
  wasm: Wasm,
  accountIdB64: string,
  entry: OwnedEntry,
  handleKeys: { readonly lineage: Identity; readonly current: Identity },
): Promise<void> {
  const old = entry.handleSeed ?? null;
  if (old === null || old.length !== 32) return;
  const fresh = channelHandleSeed(wasm, handleKeys.lineage, handleKeys.current, channelIdFromHex(entry.idHex));
  try {
    if (fresh.every((b, i) => b === old[i])) return;
  } finally {
    fresh.fill(0);
  }
  await openHandleOwed(accountIdB64);
  await recordOwedHandle({
    kind: "channel",
    id: entry.idHex,
    op: "update",
    // Read off the channel's newest record once it is read.
    handle: "",
    oldHandle: null,
    isPublic: entry.readKey !== null,
    sinceMs: Date.now(),
    reason: "rekey",
    rekeyFromSeed: b64(old),
  });
}

function retakePorts(
  accountIdB64: string,
  newOwner: Identity,
  lineage: Identity,
  current: Identity,
): RetakePorts {
  return {
    async changeOwner(idHex, listKey) {
      const wasm = await loadWasm();
      const channelId = channelIdFromHex(idHex);
      const key = wasm.ChannelRecoveryKey.fromListKey(listKey, channelId);
      try {
        const newOwnerPk = await channelOwnerKeyOf(newOwner, idHex);
        const header = key.signOwnerChange(channelId, newOwnerPk, BigInt(Date.now()));
        return await putChannelOwnerByRecovery(idHex, header, newOwner);
      } finally {
        key.free();
      }
    },
    takeChannel: (entry, plan) => takeRetakenChannel(accountIdB64, newOwner, { lineage, current }, entry, plan),
  };
}

// ----------------------------------------------------------------------------
// The ports of the account paths (re-exported from index.ts)
// ----------------------------------------------------------------------------

function retryable(err: unknown, what: string): "retry" {
  warn(what, err);
  return "retry";
}

/**
 * Port 1 - sign-up, and a guest's claim (its first password): make the keys
 * lock, the list and the password lock. Idempotent. `"made"` when all three
 * are there now; `"retry"` on any failure (call again at the next landing
 * that holds `exportKey` and `serverHalf`).
 */
export function makeOwnedChannelsVaults(input: {
  readonly accountIdB64: string;
  /** The 64-byte OPAQUE export key of the password credential. */
  readonly exportKey: Uint8Array;
  /** `S`, 32 bytes (`owned_channels_secret` of `/signup/finish`). */
  readonly serverHalf: Uint8Array;
  readonly lineageIdentity: Identity;
}): Promise<"made" | "retry"> {
  return serial(async () => {
    try {
      const { store: s } = await store();
      const opened = await s.createAll(input);
      adopt(input.accountIdB64, input.lineageIdentity, opened);
      noteOwnedChannelsChanged();
      return "made" as const;
    } catch (err) {
      return retryable(err, "the account's channel list was not made; tried again later");
    }
  });
}

/**
 * Port 2 - a password sign-in on a device that holds the account keys, and
 * after a password change on it: when the password lock `exportKey` names
 * does not exist, make it (with the `S` of the keys lock), record its root
 * in the list and delete the old lock (item 7).
 *
 * - `"current"`: the lock was there; nothing written.
 * - `"made"`: the lock was made again.
 * - `"absent"`: the account has no keys lock (made before this build, or
 *   its creation never landed): nothing is done, since `S` is not available
 *   here. {@link makeOwnedChannelsVaults} makes one when the caller holds `S`.
 * - `"retry"`: no answer; call again at the next landing.
 */
export function ensureOwnedPasswordLock(input: {
  readonly accountIdB64: string;
  readonly exportKey: Uint8Array;
  readonly lineageIdentity: Identity;
}): Promise<"current" | "made" | "absent" | "retry"> {
  return serial(async () => {
    try {
      const { store: s } = await store();
      const ensured = await s.ensure({
        exportKey: input.exportKey,
        serverHalf: null,
        lineageIdentity: input.lineageIdentity,
      });
      if (ensured === "absent") return "absent" as const;
      adopt(input.accountIdB64, input.lineageIdentity, ensured);
      return ensured.passwordLock;
    } catch (err) {
      return retryable(err, "the account's channel list lock was not checked; tried again later");
    }
  });
}

/**
 * Port 3a - a passkey that keeps the chats was added on a device with the
 * keys: make its lock and record it in the list. `credentialTag` is the
 * passkey's 16-byte tag in the seal announcement, `prfOutput` the 32-byte
 * `prf` output its seal key comes from. `"full"` past eight passkey locks.
 */
export function addOwnedPasskeyLock(input: {
  readonly accountIdB64: string;
  readonly lineageIdentity: Identity;
  readonly credentialTag: Uint8Array;
  readonly prfOutput: Uint8Array;
}): Promise<"added" | "absent" | "full" | "retry"> {
  return serial(async () => {
    try {
      const { store: s } = await store();
      const sess = await freshSession(s, input.accountIdB64, input.lineageIdentity);
      if (sess === null || sess.serverHalf === null) return "absent" as const;
      await s.addPasskeyLock({
        listKey: sess.listKey,
        serverHalf: sess.serverHalf,
        credentialTag: input.credentialTag,
        prfOutput: input.prfOutput,
      });
      await refresh(s, sess);
      return "added" as const;
    } catch (err) {
      if (err instanceof OwnedVaultError && err.code === "full") return "full" as const;
      return retryable(err, "a passkey's lock on the account's channel list was not made");
    }
  });
}

/** Port 3b - a passkey was removed: delete its lock and take it out of the
 *  list. `"removed"` also when the list named none. */
export function removeOwnedPasskeyLock(input: {
  readonly accountIdB64: string;
  readonly lineageIdentity: Identity;
  readonly credentialTag: Uint8Array;
}): Promise<"removed" | "absent" | "retry"> {
  return serial(async () => {
    try {
      const { store: s } = await store();
      const sess = await freshSession(s, input.accountIdB64, input.lineageIdentity);
      if (sess === null) return "absent" as const;
      await s.removePasskeyLock(sess.listKey, input.credentialTag);
      await refresh(s, sess);
      return "removed" as const;
    } catch (err) {
      return retryable(err, "a removed passkey's lock on the account's channel list was not deleted");
    }
  });
}

/** The session, asking the service again even when the account had no keys
 *  lock a moment ago (a port call is the person's own action). */
async function freshSession(s: OwnedChannelsStore, accountIdB64: string, lineage: Identity): Promise<Session | null> {
  if (absent !== null && absent.key === sessionKey(accountIdB64, lineage)) absent = null;
  return openSession(s, accountIdB64, lineage);
}

async function refresh(s: OwnedChannelsStore, sess: Session): Promise<void> {
  const read = await s.readList(sess.listKey);
  if (read !== null) sess.list = read.list;
}

/**
 * Port 4 - after a key reset on this device (ADR-0101 section 7 item 8),
 * once the new account landed: open the lock of what sealed the reset, take
 * every listed channel back for the new account, make the new keys lock,
 * make the password lock again where the reset was sealed by a passkey, and
 * delete the passkey locks. Shows nothing.
 *
 * - `"done"`: every channel is taken or dropped; their PublisherGroups are
 *   founded again by the publisher pass.
 * - `"retry"`: no answer somewhere. Once the new keys lock is written the
 *   publisher pass continues by itself; before that, call again while `S` is
 *   still held (`serverHalf` may be `null` on a later call: then only a keys
 *   lock an earlier call made is used).
 * - `"nothing"`: no lock opened (the account has no list, or the list was
 *   locked with another password): nothing comes back.
 */
export function retakeOwnedChannelsAfterReset(input: {
  readonly accountIdB64: string;
  readonly serverHalf: Uint8Array | null;
  readonly sealedBy: ResetSealedBy | null;
  /** The new account identity: the owner key of every retaken channel is
   *  the one its seed derives. */
  readonly newOwner: Identity;
  /** The lineage identity after the reset (the new account identity). */
  readonly newLineageIdentity: Identity;
  /** The export key of the password in force, or `null`. */
  readonly currentExportKey: Uint8Array | null;
  /** A recall: the root of the old keys lock (`owned-retake.ts`). */
  readonly oldKeysRoot?: Uint8Array | null;
}): Promise<RetakeOutcome> {
  return serial(async () => {
    try {
      const { wasm, store: s } = await store();
      const result = await retakeAfterReset(
        wasm,
        s,
        factsStorage(),
        {
          accountIdB64: input.accountIdB64,
          serverHalf: input.serverHalf,
          sealedBy: input.sealedBy,
          newLineageIdentity: input.newLineageIdentity,
          currentExportKey: input.currentExportKey,
          oldKeysRoot: input.oldKeysRoot ?? null,
        },
        retakePorts(input.accountIdB64, input.newOwner, input.newLineageIdentity, input.newOwner),
      );
      if (result.opened !== null) adopt(input.accountIdB64, input.newLineageIdentity, result.opened);
      return result.outcome;
    } catch (err) {
      return retryable(err, "the account's channels were not taken back yet");
    }
  });
}

/**
 * Port 5 - before `DELETE /account`: delete the list and every lock, each
 * signed with its own vault's key, waiting at most `budgetMs`. `"absent"`
 * when the account never had any; `"retry"` when the service did not answer
 * in time (the vaults then stay: nothing names the account in them).
 */
export function deleteOwnedChannelsVaults(input: {
  readonly lineageIdentity: Identity;
  readonly budgetMs?: number;
}): Promise<"deleted" | "absent" | "retry"> {
  const work = serial(async () => {
    try {
      const { store: s } = await store();
      const gone = await s.deleteAll(input.lineageIdentity);
      dropOwnedChannelsSession();
      return gone > 0 ? ("deleted" as const) : ("absent" as const);
    } catch (err) {
      return retryable(err, "the account's channel list was not deleted");
    }
  });
  let timer: ReturnType<typeof setTimeout> | undefined;
  const budget = new Promise<"retry">((resolve) => {
    timer = setTimeout(() => resolve("retry"), input.budgetMs ?? OWNED_VAULTS_DELETE_BUDGET_MS);
  });
  return Promise.race([work, budget]).finally(() => {
    if (timer !== undefined) clearTimeout(timer);
  });
}

/**
 * Port 6 - sign-out with Forget device: the per-account markers this module
 * keeps in `storage`, and the list key held in memory when it is this
 * account's. Never throws.
 */
export function clearOwnedChannelsFacts(storage: FactsStorage | undefined, accountIdB64: string): void {
  clearOwnedChannelsStoredFacts(storage, accountIdB64);
  if (session === null || session.accountIdB64 === accountIdB64) dropOwnedChannelsSession();
}
