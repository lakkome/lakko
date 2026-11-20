// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 phase B — a channel's access, on web: the machinery under the one
 * conversation screen (UX `group-and-channel-privacy.md` sections 3.2, 4.1,
 * 5.1, 7, 13, 15).
 *
 * - **Reading state** (reactive): visibility, role, `@name`, topic and
 *   language, photo, whether this device reads or waits, the seed of an epoch.
 * - **Create** ({@link createChannelFull}): photo first, then the
 *   PublisherGroup, the `@name` (private or public), the channel on the
 *   service with its flag, the metadata record v3, and when public the public
 *   seed of the epoch and the Discover listing.
 * - **Type** ({@link saveChannelType}): an owed change, written durably first
 *   and retried by `ChannelPublisherLoops` until it lands (section 8).
 * - **Subscribe** ({@link subscribeToChannel}): public reads at once from the
 *   public seeds (section 6); private registers with an invite proof and a
 *   sealed admission ticket and waits to be let in (section 7).
 * - **Publish** ({@link publishChannelPost}): any chat envelope (section 13).
 * - **Subscribers** (section 15): the count, a page of rows with the link a
 *   private row came by, removal with a rotation.
 *
 * Every refusal is a {@link ChannelError} with a code; the screen maps codes
 * to copy (no catalog text here). Nothing is logged that names a key.
 *
 * Identities: `lineage` is the identity the channel's PublisherGroup lives in
 * (`chats.conversationAccountIdentity()`), whose seed derives the group id,
 * the owner key and the read key; `current` is the current account identity
 * (`chats.accountIdentity()`), whose journal root signs the `@name`. They are
 * the same instance until an ADR-0074 rotation.
 */

import { createSignal } from "solid-js";

import type { Wasm } from "../wasm";
import { loadWasm } from "../wasm";
import {
  apiError,
  CHANNELS_UNREACHABLE,
  createChannel,
  deleteChannelOnService,
  deleteListing,
  fetchEnvelopes,
  fetchEpoch,
  fetchPublicSeed,
  fetchSubscriberCount,
  listSubscriberPage,
  PUBLISHER_REFUSED,
  publishPost,
  putChannelAccess,
  putListing,
  putPublicSeed,
  removeSubscriberAsPublisher,
  SUBSCRIBER_REMOVED,
  subscribeWithAccess,
} from "./api";
import {
  bytesToHex,
  channelIdFromHex,
  composePost,
  freshPublisherKeys,
  freshSubscriberKeys,
  isCurrentSubscriberKey,
  openSubscriberSeedEnvelope,
  type ChannelSubscriberKeys,
  type WasmIdentity,
} from "./composer";
import {
  emptyAccessRecord,
  foldVisibility,
  newestAdmissionSeed,
  removalSteps,
  subscriberVia,
  type ChannelAccessRecord,
  type ChannelVisibility,
} from "./channel-access-record";
import { publishChannelMeta } from "./channel-meta";
import type { ChannelSharedMeta } from "./channel-log-codec";
import { rememberSubscriberCount } from "./ui-cache";
import { loadRotationState, saveRotationState } from "./rotation-state-store";
import { ticketInviteKeyHex } from "./subscriber-admission";
import { channelIdForHandle, forgetClaimedHandle } from "./claimed-handles";
import { LegacyChannelError } from "./legacy-v1";
import { composerSeedFor, outgoingPublisherSeed, readsPublicSeeds } from "./publisher-seed";
import { listedStamp, nextListingStamp } from "../discover/listing";
import { channelsActions, channelsState, awaitChannelsHydration } from "../state/channels-state";
import {
  claimChannelHandle,
  replaceChannelHandle,
  retireChannelHandle,
  signChannelHandle,
  updateChannelHandle,
  type ChannelHandleWrite,
} from "../chat/group-handle";
import { forgetHandle, noteHandleWrite, notePublicRecordChangedHere } from "../chat/handle-owed";
import type { AllowedSetting } from "../chat/reaction-rules";
import { sessionTokens } from "../auth/session-token-provider";
import { registeredLinks, revokeRegisteredLink } from "../devices/sync/link-register";
import { HandleWriteError } from "../discovery/handle-lookup";
import { DiscoveryClient, DiscoveryHttpError, DiscoveryNetworkError } from "../discovery/client";
import { anonTokenQuotaExhausted, isAnonTokenNetworkFailure } from "../tokens/anon-token-pool";
import { uploadLocalAvatar } from "../profile/avatar-upload";
import { reuploadedPhoto } from "../chat/GroupInfoExtras";
import type { LocalAvatar } from "../profile/avatar-media";
import { isSendLimit, isStorageFull } from "../chat/attachment-failure";
import { bytesToB64, b64ToBytes, type AvatarData } from "../profile/avatar-model";

export type { ChannelVisibility } from "./channel-access-record";
export type ChannelRole = "owner" | "publisher" | "subscriber";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

export type ChannelErrorCode =
  | "sign-in"
  | "handle-held"
  | "handle-malformed"
  | "too-many-today"
  | "offline"
  | "setup-failed"
  | "photo-upload"
  | "photo-limit"
  | "photo-full"
  | "link-invalid"
  | "link-damaged"
  | "not-publisher"
  | "needs-owner"
  | "failed";

/** A refused channel action. `code` is what the screen words; `cause` (when
 *  set) is the underlying error, never shown. */
export class ChannelError extends Error {
  override name = "ChannelError";
  constructor(
    readonly code: ChannelErrorCode,
    options?: { readonly cause?: unknown },
  ) {
    super(`channel action refused: ${code}`, options);
  }
}

/** Map a handle write failure to a code. */
function handleErrorCode(err: unknown): ChannelErrorCode {
  if (err instanceof ChannelError) return err.code;
  if (err instanceof HandleWriteError) {
    if (err.status === 409 || err.code === "name-taken") return "handle-held";
    if (err.status === 429) return "too-many-today";
    if (err.status === 400) return "handle-malformed";
    return "failed";
  }
  if (err instanceof DiscoveryHttpError) {
    if (err.status === 409) return "handle-held";
    if (err.status === 429) return "too-many-today";
    return "failed";
  }
  if (anonTokenQuotaExhausted(err) !== null) return "too-many-today";
  if (isAnonTokenNetworkFailure(err) || err instanceof DiscoveryNetworkError || err instanceof TypeError) {
    return "offline";
  }
  return "failed";
}

/** Map a channels-service failure to a code. Exported for the invite-link
 *  path, so a refusal is never worded as a connection problem. */
export function serviceErrorCode(err: unknown, fallback: ChannelErrorCode = "failed"): ChannelErrorCode {
  if (err instanceof ChannelError) return err.code;
  if (err instanceof LegacyChannelError) return "setup-failed";
  if (apiError.is(err)) {
    if (err.code === CHANNELS_UNREACHABLE) return "offline";
    if (err.code === PUBLISHER_REFUSED) return "not-publisher";
    if (err.code === "invite-proof-required") return "link-invalid";
    return fallback;
  }
  if (anonTokenQuotaExhausted(err) !== null) return "too-many-today";
  if (isAnonTokenNetworkFailure(err) || err instanceof TypeError) return "offline";
  return fallback;
}

/**
 * A private channel created before phase B holds no admission seed: no link
 * can be minted for it, and nothing used to say why (seen live 2026-11-05:
 * "Could not save the link" with no request made). The owner's device mints
 * what is missing here, as `createChannelFull` mints it for a new private
 * channel: the first admission seed, the record's own visibility, and the
 * service's private flag (best effort; a refused flag is logged and set again
 * by the next save of the channel type). A co-publisher's device cannot mint
 * it and waits for the owner's device to hand it one.
 */
export async function repairPrivateChannelAccess(
  identity: WasmIdentity,
  idHex: string,
): Promise<{ readonly version: number; readonly seedB64: string }> {
  const held = newestAdmissionSeed(accessOf(idHex));
  if (held !== null) return held;
  if (!isOwnerHere(idHex)) throw new ChannelError("needs-owner");
  const wasm = await loadWasm();
  // The rows already there came in before tickets existed: they stay readers
  // (as going private keeps today's subscribers). Without this mark the
  // ticket check would remove every one of them.
  let through = accessOf(idHex).admittedThroughSeq;
  try {
    for (let after = through; ; ) {
      const page = await listSubscriberPage(idHex, identity, after);
      const last = page.entries.at(-1);
      if (last === undefined) break;
      through = Math.max(through, last.seq);
      if (page.nextAfter <= after) break;
      after = page.nextAfter;
    }
  } catch (err) {
    throw new ChannelError(serviceErrorCode(err), { cause: err });
  }
  const minted = { version: 1, seedB64: bytesToB64(wasm.generateChannelAdmissionSeed()) };
  await channelsActions.updateAccessDurable(idHex, (r) => ({
    ...r,
    visibility: r.visibility ?? "private",
    admittedThroughSeq: Math.max(r.admittedThroughSeq, through),
    admissionSeeds: r.admissionSeeds.length > 0 ? r.admissionSeeds : [minted],
  }));
  try {
    await putChannelAccess(idHex, identity, true);
  } catch (err) {
    console.warn("[channels] private flag not set during repair", apiError.is(err) ? err.code : "error");
  }
  return newestAdmissionSeed(accessOf(idHex)) ?? minted;
}

// ---------------------------------------------------------------------------
// Reactive reads
// ---------------------------------------------------------------------------

function accessOf(idHex: string): ChannelAccessRecord {
  return channelsState.accessByChannel().get(idHex) ?? emptyAccessRecord();
}

function isPublisherHere(idHex: string): boolean {
  return channelsState.keysByChannel().has(idHex);
}

function isOwnerHere(idHex: string): boolean {
  return isPublisherHere(idHex) && channelsActions.coPublisherGroupIdOf(idHex) === undefined;
}

/** Only the channel owner's device rotates the channel key (maintainer,
 *  2026-11-08); a co-publisher's never does, scheduled or on demand. */
export function channelRotatesHere(idHex: string): boolean {
  return isOwnerHere(idHex);
}

/** Reactive. Owner: its own setting; reader: the newest record's flag, else
 *  what it came by. */
export function channelVisibilityOf(idHex: string): ChannelVisibility | null {
  const a = accessOf(idHex);
  const meta = channelsState.metaByChannel().get(idHex);
  return foldVisibility({
    ownerSetting: isOwnerHere(idHex) ? a.visibility : null,
    metaListed: meta?.listed,
    cameBy: a.visibility,
  });
}

/** Reactive. `null`: this device holds nothing for the channel. */
export function channelRoleOf(idHex: string): ChannelRole | null {
  if (isPublisherHere(idHex)) return isOwnerHere(idHex) ? "owner" : "publisher";
  if (channelsState.subscriberKeysByChannel().has(idHex)) return "subscriber";
  if (channelsState.channels().some((c) => c.id === idHex)) return "subscriber";
  return null;
}

/** Reactive. The channel's `@name` (canonical, no `@`) when known. */
export function channelHandleOf(idHex: string): string | null {
  const a = accessOf(idHex);
  if (isOwnerHere(idHex) && a.handle !== null) return a.handle;
  const signed = channelsState.metaByChannel().get(idHex)?.handle?.handle;
  if (signed !== undefined) return signed;
  if (a.handle !== null) return a.handle;
  const local = channelsState.channels().find((c) => c.id === idHex)?.localHandle;
  return local === undefined || local.length === 0 ? null : local.replace(/^@/, "");
}

/** Reactive. Topic id and language, when known. */
export function channelTopicOf(idHex: string): { topic: number; language: string } | null {
  const a = accessOf(idHex);
  if (isOwnerHere(idHex) && a.topic !== null) return { topic: a.topic, language: a.language ?? "" };
  const meta = channelsState.metaByChannel().get(idHex);
  if (meta?.topic !== undefined || meta?.language !== undefined) {
    return { topic: meta.topic ?? 0, language: meta.language ?? "" };
  }
  if (a.topic !== null) return { topic: a.topic, language: a.language ?? "" };
  return null;
}

/** Reactive. The photo from the newest record, or the owner's own. */
export function channelAvatarOf(idHex: string): AvatarData | null {
  const meta = channelsState.metaByChannel().get(idHex);
  if (meta !== undefined && meta.updatedAtMs > 0n) {
    // The record decides, a removed photo included; an owner's fresher
    // local choice shows until its own record comes back.
    const own = accessOf(idHex).avatar;
    return meta.avatar ?? (isOwnerHere(idHex) ? own : null);
  }
  return accessOf(idHex).avatar;
}

/** Reactive. `reading`: this device holds the seed of the current epoch (or
 *  publishes); `waiting`: private, registered, not let in yet. */
export function channelReadStateOf(idHex: string): "reading" | "waiting" | "none" {
  if (isPublisherHere(idHex)) return "reading";
  const seeds = channelsState.subscribedSeedsByChannel().get(idHex);
  const epoch = channelsState.epochByChannel().get(idHex);
  if (seeds !== undefined && seeds.size > 0 && (epoch === undefined || seeds.has(epoch))) return "reading";
  if (accessOf(idHex).waiting) return "waiting";
  if (seeds !== undefined && seeds.size > 0) return "reading";
  return "none";
}

/** The seed of `epoch` this device holds (owner, publisher or reader). */
export function channelSeedFor(idHex: string, epoch: number): Uint8Array | undefined {
  const keys = channelsState.keysByChannel().get(idHex);
  if (keys !== undefined) {
    // The composer's seed opens the epoch it serves; any other epoch reads
    // from the seeds kept per epoch.
    const served = keys.seedEpoch ?? channelsState.epochByChannel().get(idHex);
    if (served === undefined || served === epoch) return keys.seed;
  }
  return channelsActions.currentSeed(idHex, epoch);
}

/** The current epoch as this device last saw it. */
export function channelEpochOf(idHex: string): number | undefined {
  return channelsState.epochByChannel().get(idHex);
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

function freshChannelIdHex(): string {
  const buf = new Uint8Array(16);
  crypto.getRandomValues(buf);
  return bytesToHex(buf);
}

function bytesToB64Url(bytes: Uint8Array): string {
  return bytesToB64(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** The read key of a public channel this device can reach: derived by the
 *  owner, kept by a reader or a co-publisher that learned it. */
export async function channelReadKeyHere(lineage: WasmIdentity | null, idHex: string): Promise<Uint8Array | null> {
  const a = accessOf(idHex);
  if (a.readKeyB64 !== null) return b64ToBytes(a.readKeyB64);
  if (lineage !== null && isOwnerHere(idHex)) {
    const wasm = await loadWasm();
    const key = wasm.channelReadKey(lineage, channelIdFromHex(idHex));
    channelsActions.updateAccess(idHex, (r) => ({ ...r, readKeyB64: bytesToB64(key) }));
    return key;
  }
  return null;
}

/** File the public seed of `epoch` (idempotent on the service). */
async function filePublicSeed(identity: WasmIdentity, idHex: string, epoch: number, seed: Uint8Array): Promise<void> {
  const readKey = await channelReadKeyHere(identity, idHex);
  if (readKey === null) return;
  const wasm = await loadWasm();
  const envelope = wasm.sealPublicSeedEnvelope(readKey, channelIdFromHex(idHex), epoch, seed);
  await putPublicSeed(idHex, epoch, envelope, identity);
}

/** Put the Discover listing for a public channel as it is now (section 16:
 *  public means listed). A language the listing codec refuses lists nothing. */
async function putChannelListing(identity: WasmIdentity, idHex: string): Promise<void> {
  const a = accessOf(idHex);
  const handle = a.handle;
  const meta = channelsState.metaByChannel().get(idHex);
  const local = channelsState.channels().find((c) => c.id === idHex);
  if (handle === null) return;
  const wasm = await loadWasm();
  const language = (a.language ?? meta?.language ?? "").trim();
  if (!wasm.channelLanguageIsValid(language)) return;
  const stored = await listedStamp(idHex).catch(() => null);
  const bytes = wasm.composeChannelListing(
    channelIdFromHex(idHex),
    handle,
    (meta?.name ?? local?.localName ?? "").trim(),
    (meta?.description ?? local?.localDescription ?? "").trim(),
    a.topic ?? meta?.topic ?? 0,
    language,
    nextListingStamp(Date.now(), stored),
  );
  await putListing(idHex, identity, bytes);
}

/** The signed `@name` envelope for the metadata record (section 11). */
async function signedHandleOf(
  lineage: WasmIdentity,
  current: WasmIdentity,
  idHex: string,
  written: ChannelHandleWrite,
): Promise<NonNullable<ChannelSharedMeta["handle"]> | null> {
  if (written.certificate === null) return null;
  const wasm = await loadWasm();
  const h = signChannelHandle({
    wasm,
    lineageIdentity: lineage,
    currentIdentity: current,
    channelId: channelIdFromHex(idHex),
    handle: written.handle,
    isPublic: written.isPublic,
    certificate: written.certificate,
    stampMs: BigInt(Date.now()),
  });
  try {
    return {
      handle: written.handle,
      isPublic: written.isPublic,
      envelopeB64: bytesToB64(wasm.composeConversationHandleEnvelope(h)),
    };
  } finally {
    h.free();
  }
}

async function uploadPhoto(photo: LocalAvatar): Promise<AvatarData> {
  try {
    return await uploadLocalAvatar(photo);
  } catch (err) {
    const code = isStorageFull(err) ? "photo-full" : isSendLimit(err) ? "photo-limit" : "photo-upload";
    throw new ChannelError(code, { cause: err });
  }
}

/** Keep the seed of the current epoch beside the per-epoch map, so a
 *  publisher still opens its own older posts after a rotation. */
function keepPublisherSeed(idHex: string, epoch: number, seed: Uint8Array): void {
  if (channelsActions.currentSeed(idHex, epoch) === undefined) {
    channelsActions.setSubscribedSeedForEpoch(idHex, epoch, seed.slice());
  }
}

// ---------------------------------------------------------------------------
// Create (UX 3.2)
// ---------------------------------------------------------------------------

export interface CreateChannelInput {
  readonly name: string;
  readonly description: string;
  /** Canonical, no `@`, well formed. */
  readonly handle: string;
  readonly visibility: ChannelVisibility;
  readonly topic: number;
  readonly language: string;
  /** The chosen photo, not yet uploaded: uploaded before anything is created. */
  readonly photo: LocalAvatar | null;
}

/**
 * Create a channel. In order, so a failure leaves nothing announced:
 * the photo upload ("photo-upload" / "photo-limit"), the PublisherGroup and
 * its journal claim ("setup-failed"), the `@name` ("handle-held",
 * "too-many-today", "offline"), the channel on the service with its flag
 * ("offline", "setup-failed"). Then, best effort (the loops retry what does
 * not land): the metadata record v3, and when public the public seed of the
 * epoch and the listing. A private channel also mints its first admission
 * seed. Returns the channel id hex.
 */
export async function createChannelFull(
  lineage: WasmIdentity,
  current: WasmIdentity,
  accountIdB64: string | null,
  input: CreateChannelInput,
): Promise<string> {
  const name = input.name.trim();
  const description = input.description.trim();
  const isPublic = input.visibility === "public";
  const handle = input.handle.replace(/^@/, "").trim();
  const avatar = input.photo === null ? null : await uploadPhoto(input.photo);
  await awaitChannelsHydration();
  const id =
    handle.length > 0 && accountIdB64 !== null
      ? channelIdForHandle(accountIdB64, handle, freshChannelIdHex())
      : freshChannelIdHex();
  const wasm = await loadWasm();
  // ADR-0057: found the PublisherGroup and claim its journal first.
  let keys;
  try {
    keys = await freshPublisherKeys(lineage, id, channelsState.keysByChannel().get(id)?.signingKeySeed);
  } catch (err) {
    throw new ChannelError(serviceErrorCode(err, "setup-failed"), { cause: err });
  }
  // The @name, before the channel exists on the service: a held name aborts
  // with nothing announced. The same id claims it again on a retry.
  let written: ChannelHandleWrite;
  try {
    written = await claimChannelHandle({
      wasm,
      lineageIdentity: lineage,
      currentIdentity: current,
      channelId: channelIdFromHex(id),
      handle,
      isPublic,
      facts: { name, description, avatar },
    });
  } catch (err) {
    const code = handleErrorCode(err);
    if (code === "handle-held" && accountIdB64 !== null) forgetClaimedHandle(accountIdB64, handle);
    throw new ChannelError(code, { cause: err });
  }
  try {
    await createChannel(id, lineage, undefined, !isPublic);
  } catch (err) {
    throw new ChannelError(serviceErrorCode(err, "setup-failed"), { cause: err });
  }
  if (accountIdB64 !== null) {
    forgetClaimedHandle(accountIdB64, handle);
    await noteHandleWrite({
      kind: "channel",
      id,
      handle: written.handle,
      isPublic,
      lastWriteMs: Date.now(),
      leaseUntilMs: written.leaseUntilMs,
    }).catch(() => undefined);
  }
  const signed = await signedHandleOf(lineage, current, id, written).catch(() => null);
  const admissionSeeds = isPublic ? [] : [{ version: 1, seedB64: bytesToB64(wasm.generateChannelAdmissionSeed()) }];
  channelsActions.addChannel({
    id,
    localName: name,
    localDescription: description,
    addedAtMs: Date.now(),
    localHandle: written.handle,
  });
  channelsActions.setKeys(id, keys);
  channelsActions.setEpoch(id, 0);
  keepPublisherSeed(id, 0, keys.seed);
  await channelsActions.updateAccessDurable(id, (r) => ({
    ...r,
    visibility: input.visibility,
    handle: written.handle,
    handleCertB64: written.certificate === null ? null : bytesToB64(written.certificate),
    handleEnvelopeB64: signed?.envelopeB64 ?? null,
    topic: input.topic,
    language: input.language,
    avatar,
    readKeyB64: isPublic ? bytesToB64(wasm.channelReadKey(lineage, channelIdFromHex(id))) : null,
    admissionSeeds,
    admittedThroughSeq: 0,
  }));
  try {
    await publishChannelMeta(
      id,
      {
        name,
        description,
        topic: input.topic,
        language: input.language,
        listed: isPublic,
        ...(signed === null ? {} : { handle: signed }),
        ...(avatar === null ? {} : { avatar }),
      },
      lineage,
    );
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] the first metadata record was not posted; the loops re-issue it", err instanceof Error ? err.name : "");
  }
  if (isPublic) {
    try {
      await filePublicSeed(lineage, id, 0, keys.seed);
      await putChannelListing(lineage, id);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.warn("[channels] public seed or listing not filed; the loops retry", err instanceof Error ? err.name : "");
    }
  }
  return id;
}

// ---------------------------------------------------------------------------
// Type, name, photo (UX 4.1, 4.5.2)
// ---------------------------------------------------------------------------

const [typeProgress, setTypeProgress] = createSignal<ReadonlyMap<string, { done: number; total: number }>>(new Map());

/** Reactive: going private, the reseal's progress, or `null`. */
export function channelTypeProgress(idHex: string): { done: number; total: number } | null {
  return typeProgress().get(idHex) ?? null;
}

function setProgress(idHex: string, p: { done: number; total: number } | null): void {
  setTypeProgress((m) => {
    const next = new Map(m);
    if (p === null) next.delete(idHex);
    else next.set(idHex, p);
    return next;
  });
}

/** The rotation the loops run on demand, installed by `ChannelPublisherLoops`
 *  (it holds the one PublisherGroup handle per channel). */
type RotateNow = (idHex: string, onProgress?: (done: number, total: number) => void) => Promise<void>;
let rotateNow: RotateNow | null = null;

/** `ChannelPublisherLoops` installs how to rotate a channel now. */
export function installChannelRotateNow(fn: RotateNow | null): void {
  rotateNow = fn;
}

async function rotateChannelNow(idHex: string, onProgress?: (done: number, total: number) => void): Promise<void> {
  if (rotateNow === null) throw new ChannelError("failed");
  await rotateNow(idHex, onProgress);
}

/**
 * Owner, Channel type screen (UX 4.1, ADR-0087 section 8). Recorded as owed
 * first (durably), then run; a failure leaves it owed and the loops retry it
 * after a restart. Throws ChannelError ("handle-held", "offline",
 * "too-many-today", "failed") for this attempt.
 */
export async function saveChannelType(
  lineage: WasmIdentity,
  current: WasmIdentity,
  idHex: string,
  next: { visibility: ChannelVisibility; handle: string; topic: number; language: string },
): Promise<void> {
  if (!isOwnerHere(idHex)) throw new ChannelError("not-publisher");
  const from = accessOf(idHex).visibility;
  await channelsActions.updateAccessDurable(idHex, (r) => ({
    ...r,
    owedType: { ...next, handle: next.handle.replace(/^@/, ""), from },
  }));
  await runOwedChannelType(lineage, current, idHex);
}

/** Run the owed type change of `idHex`, if any (called by the loops too).
 *  Each step is idempotent, so a retry repeats the ones that landed. */
export async function runOwedChannelType(lineage: WasmIdentity, current: WasmIdentity, idHex: string): Promise<void> {
  const owed = accessOf(idHex).owedType;
  if (owed === null) return;
  const a = accessOf(idHex);
  const wasm = await loadWasm();
  const goingPrivate = owed.visibility === "private" && owed.from !== "private";
  const goingPublic = owed.visibility === "public" && owed.from !== "public";
  const meta = channelsState.metaByChannel().get(idHex);
  const local = channelsState.channels().find((c) => c.id === idHex);
  const facts = {
    name: (meta?.name ?? local?.localName ?? "").trim(),
    description: (meta?.description ?? local?.localDescription ?? "").trim(),
    avatar: channelAvatarOf(idHex),
  };
  const isPublic = owed.visibility === "public";
  // 1. The name (section 8: one signed write; a new name is a replace).
  let written: ChannelHandleWrite;
  try {
    const common = {
      wasm,
      lineageIdentity: lineage,
      currentIdentity: current,
      channelId: channelIdFromHex(idHex),
      handle: owed.handle,
      isPublic,
      facts,
    };
    written =
      a.handle !== null && a.handle !== owed.handle
        ? await replaceChannelHandle({ ...common, oldHandle: a.handle })
        : a.handle === null
          ? await claimChannelHandle(common)
          : await updateChannelHandle(common);
  } catch (err) {
    const code = handleErrorCode(err);
    if (code === "handle-held" || code === "handle-malformed") {
      // Refused for good: drop the owed change, the name stays as it was.
      await channelsActions.updateAccessDurable(idHex, (r) => ({ ...r, owedType: null }));
    }
    throw new ChannelError(code, { cause: err });
  }
  await noteHandleWrite({
    kind: "channel",
    id: idHex,
    handle: written.handle,
    isPublic,
    lastWriteMs: Date.now(),
    leaseUntilMs: written.leaseUntilMs,
  }).catch(() => undefined);
  const signed = await signedHandleOf(lineage, current, idHex, written).catch(() => null);
  try {
    // 2. The flag on the service.
    if (goingPrivate || goingPublic) await putChannelAccess(idHex, lineage, !isPublic);
    // 3. Going private: the rows of today stay readers; a new epoch whose
    //    seed goes to each of them once; the listing and the public seed go.
    if (goingPrivate) {
      let through = a.admittedThroughSeq;
      for (let after = through; ; ) {
        const page = await listSubscriberPage(idHex, lineage, after);
        const last = page.entries.at(-1);
        if (last === undefined) break;
        through = Math.max(through, last.seq);
        if (page.nextAfter <= after) break;
        after = page.nextAfter;
      }
      await channelsActions.updateAccessDurable(idHex, (r) => ({
        ...r,
        visibility: "private",
        admittedThroughSeq: Math.max(r.admittedThroughSeq, through),
        admissionSeeds:
          r.admissionSeeds.length > 0 ? r.admissionSeeds : [{ version: 1, seedB64: bytesToB64(wasm.generateChannelAdmissionSeed()) }],
      }));
      await deleteListing(idHex, lineage).catch(() => undefined);
      setProgress(idHex, { done: 0, total: 0 });
      try {
        await rotateChannelNow(idHex, (done, total) => setProgress(idHex, { done, total }));
      } finally {
        setProgress(idHex, null);
      }
      // Section 18.3: the photo again under a fresh key.
      const fresh = await reuploadedPhoto(channelAvatarOf(idHex)).catch(() => null);
      if (fresh !== null) channelsActions.updateAccess(idHex, (r) => ({ ...r, avatar: fresh }));
    }
    channelsActions.updateAccess(idHex, (r) => ({
      ...r,
      visibility: owed.visibility,
      handle: written.handle,
      handleCertB64: written.certificate === null ? r.handleCertB64 : bytesToB64(written.certificate),
      handleEnvelopeB64: signed?.envelopeB64 ?? r.handleEnvelopeB64,
      topic: owed.topic,
      language: owed.language,
    }));
    // 4. The record every reader folds: flag, name, topic, language, photo.
    await publishChannelMeta(
      idHex,
      {
        name: facts.name.length > 0 ? facts.name : (local?.localName ?? ""),
        description: facts.description,
        topic: owed.topic,
        language: owed.language,
        listed: isPublic,
        ...(signed === null ? {} : { handle: signed }),
        avatar: accessOf(idHex).avatar ?? channelAvatarOf(idHex),
      },
      lineage,
    );
    // 5. Going public: the public seed of the epoch and the listing.
    if (isPublic) {
      const keys = channelsState.keysByChannel().get(idHex);
      const epoch = (await fetchEpoch(idHex)).epoch;
      channelsActions.setEpoch(idHex, epoch);
      if (keys !== undefined) await filePublicSeed(lineage, idHex, epoch, keys.seed);
      await putChannelListing(lineage, idHex);
    }
  } catch (err) {
    throw new ChannelError(err instanceof ChannelError ? err.code : serviceErrorCode(err), { cause: err });
  }
  await channelsActions.updateAccessDurable(idHex, (r) => ({ ...r, owedType: null }));
}

/** Owner: rename / describe from the info header. A record the composer
 *  refuses throws the composer's error unchanged (its `channelMetaError`
 *  code is what `channelMetaErrorText` words); a service failure is a
 *  ChannelError. */
export async function saveChannelName(
  identity: WasmIdentity,
  idHex: string,
  name: string,
  description: string,
): Promise<void> {
  if (!isPublisherHere(idHex)) throw new ChannelError("not-publisher");
  let meta: ChannelSharedMeta;
  try {
    meta = await publishChannelMeta(idHex, { name, description }, identity);
  } catch (err) {
    if (typeof err === "object" && err !== null && "channelMetaError" in err) throw err;
    throw new ChannelError(serviceErrorCode(err), { cause: err });
  }
  channelsActions.updateLocalMeta(idHex, { localName: meta.name, localDescription: meta.description });
  if (isOwnerHere(idHex) && channelVisibilityOf(idHex) === "public") {
    await putChannelListing(identity, idHex).catch(() => undefined);
  }
}

/** Owner: set or remove the photo (UX 4.5.2). Upload first; nothing is sent
 *  when it fails. */
export async function saveChannelPhoto(
  identity: WasmIdentity,
  idHex: string,
  photo: LocalAvatar | null,
): Promise<void> {
  if (!isOwnerHere(idHex)) throw new ChannelError("not-publisher");
  const avatar = photo === null ? null : await uploadPhoto(photo);
  const held = channelsState.metaByChannel().get(idHex);
  const local = channelsState.channels().find((c) => c.id === idHex);
  try {
    await publishChannelMeta(
      idHex,
      {
        name: held?.name ?? local?.localName ?? "",
        description: held?.description ?? local?.localDescription ?? "",
        avatar,
      },
      identity,
    );
  } catch (err) {
    throw new ChannelError(serviceErrorCode(err), { cause: err });
  }
  channelsActions.updateAccess(idHex, (r) => ({ ...r, avatar }));
  // Section 18.3: a public record carries the photo; republish owed.
  if (channelVisibilityOf(idHex) === "public") await notePublicRecordChangedHere("channel", idHex).catch(() => undefined);
}

// ---------------------------------------------------------------------------
// Subscribe and read (UX 5.1, 7.2)
// ---------------------------------------------------------------------------

/** A public record or a link gave this device what it needs to read. */
export type ChannelAccessKey =
  | { kind: "public"; readKey: Uint8Array }
  | { kind: "private"; inviteSeed: Uint8Array; admissionPublicKey: Uint8Array; admissionVersion: number };

async function ensureSubscriberKeys(idHex: string): Promise<ChannelSubscriberKeys> {
  await awaitChannelsHydration();
  const existing = channelsState.subscriberKeysByChannel().get(idHex);
  if (existing !== undefined && (await isCurrentSubscriberKey(existing))) return existing;
  const fresh = await freshSubscriberKeys();
  channelsActions.setSubscriberKeys(idHex, fresh);
  return fresh;
}

/**
 * Subscribe (UX 5.1 card, 7.2 link page). Public: registers and reads at once
 * from the public seeds. Private: registers with the link's invite proof and
 * a sealed admission ticket, then waits for a publisher's device to let it
 * in. Adds the channel to the list. ChannelError: "link-invalid" (the link's
 * key is revoked, expired or used up), "offline", "too-many-today", "failed".
 */
export async function subscribeToChannel(
  idHex: string,
  access: ChannelAccessKey,
  facts: { name: string; description: string; handle?: string; avatar?: AvatarData | null },
): Promise<"reading" | "waiting"> {
  const keys = await ensureSubscriberKeys(idHex);
  const pubHex = bytesToHex(keys.public);
  const wasm = await loadWasm();
  try {
    if (access.kind === "public") {
      await subscribeWithAccess(idHex, pubHex, {});
    } else {
      const signer = wasm.ChannelInviteSigner.fromInviteSeed(access.inviteSeed);
      try {
        const channelId = channelIdFromHex(idHex);
        const ticket = signer.sealTicket(access.admissionPublicKey, access.admissionVersion, channelId, keys.public);
        await subscribeWithAccess(idHex, pubHex, {
          sealedTicketB64Url: bytesToB64Url(ticket),
          inviteProof: () => ({
            headerName: wasm.channelInviteProofHeaderName(),
            value: signer.signProof(channelId, keys.public),
          }),
        });
      } finally {
        signer.free();
      }
    }
  } catch (err) {
    throw new ChannelError(serviceErrorCode(err), { cause: err });
  }
  if (!channelsState.channels().some((c) => c.id === idHex)) {
    channelsActions.addChannel({
      id: idHex,
      localName: facts.name.trim(),
      localDescription: facts.description.trim(),
      addedAtMs: Date.now(),
      ...(facts.handle === undefined || facts.handle.length === 0 ? {} : { localHandle: facts.handle.replace(/^@/, "") }),
    });
  }
  await channelsActions.updateAccessDurable(idHex, (r) => ({
    ...r,
    visibility: access.kind,
    readKeyB64: access.kind === "public" ? bytesToB64(access.readKey) : r.readKeyB64,
    waiting: access.kind === "private",
    avatar: facts.avatar ?? r.avatar,
    handle: facts.handle === undefined ? r.handle : facts.handle.replace(/^@/, ""),
  }));
  await catchUpChannel(idHex).catch(() => undefined);
  return channelReadStateOf(idHex) === "reading" ? "reading" : "waiting";
}

/** What `catchUpChannel` does when the service says this device's key was
 *  removed from a private channel (UX 9.5): `MembershipEndContext` installs
 *  the channel end. */
type ChannelRemoved = (idHex: string) => void | Promise<void>;
let onRemoved: ChannelRemoved | null = null;

/** Install what happens when a publisher removed this device's key. */
export function installChannelRemovedHandler(fn: ChannelRemoved | null): void {
  onRemoved = fn;
}

/** How far back a public reader asks for seeds: the 30 days of posts at a
 *  rotation a day, with room (chosen). */
const PUBLIC_SEED_LOOKBACK = 40;

/**
 * Pull the seeds the service holds for this device now; idempotent. A reader
 * opens its retained per-key envelopes (ADR-0031) and, on a public channel,
 * the public seeds of the epochs it lacks, newest first. A `410` on the
 * envelope read is a removal: the installed handler ends the channel.
 */
export async function catchUpChannel(idHex: string): Promise<void> {
  const epoch = (await fetchEpoch(idHex)).epoch;
  channelsActions.setEpoch(idHex, epoch);
  // A publisher reads the epochs before it became one the way a reader
  // does: the envelopes it still holds as a subscriber, and on a public
  // channel the public seeds under the read key the owner sent it. Without
  // them a new publisher's view of the channel opened with "Earlier posts
  // aren't available." over the owner's record at creation.
  const publisher = isPublisherHere(idHex);
  const kp = channelsState.subscriberKeysByChannel().get(idHex);
  if (kp !== undefined) {
    try {
      const retained = await fetchEnvelopes(idHex, bytesToHex(kp.public));
      for (const { epoch: e, envelope } of retained) {
        if (channelsActions.currentSeed(idHex, e) !== undefined) continue;
        try {
          const seed = await openSubscriberSeedEnvelope(kp.secret, idHex, e, envelope);
          channelsActions.setSubscribedSeedForEpoch(idHex, e, seed);
        } catch {
          // One bad envelope does not stop the others.
        }
      }
    } catch (err) {
      if (apiError.is(err) && (err.code === SUBSCRIBER_REMOVED || err.status === 410)) {
        // A publisher's subscription row going is not the end of the
        // channel for it: it still posts and reads its own epochs.
        if (!publisher) {
          await onRemoved?.(idHex);
          return;
        }
      } else if (!(apiError.is(err) && err.status === 404)) {
        if (!publisher) throw err;
      }
    }
  }
  const readKeyB64 = accessOf(idHex).readKeyB64;
  if (readsPublicSeeds(publisher, readKeyB64 !== null, channelVisibilityOf(idHex)) && readKeyB64 !== null) {
    const readKey = b64ToBytes(readKeyB64);
    const wasm = await loadWasm();
    const channelId = channelIdFromHex(idHex);
    try {
      for (let e = epoch; e >= 0 && e > epoch - PUBLIC_SEED_LOOKBACK; e -= 1) {
        if (channelsActions.currentSeed(idHex, e) !== undefined) continue;
        const wire = await fetchPublicSeed(idHex, e);
        if (wire === null) break;
        try {
          channelsActions.setSubscribedSeedForEpoch(idHex, e, wasm.openPublicSeedEnvelope(readKey, channelId, e, wire));
        } catch {
          // A seed that does not open under this key is not the channel's.
        }
      }
    } catch (err) {
      // A publisher posts and reads its own epochs without these; a failed
      // read is tried again at the next catch-up.
      if (!publisher) throw err;
    }
  }
  if (accessOf(idHex).waiting && channelsActions.currentSeed(idHex, epoch) !== undefined) {
    channelsActions.updateAccess(idHex, (r) => ({ ...r, waiting: false }));
  }
}

// ---------------------------------------------------------------------------
// Publish (UX 13)
// ---------------------------------------------------------------------------

/**
 * Publish one post: `plaintext` is a chat envelope, or an op envelope (edit /
 * delete of a post's cursor). Returns the cursor. ChannelError
 * "not-publisher", "offline", "failed".
 */
export async function publishChannelPost(identity: WasmIdentity, idHex: string, plaintext: Uint8Array): Promise<number> {
  const keys = channelsState.keysByChannel().get(idHex);
  if (keys === undefined) throw new ChannelError("not-publisher");
  const sealing = composerSeedFor(keys, channelsState.epochByChannel().get(idHex), (e) =>
    channelsActions.currentSeed(idHex, e),
  );
  // The channel has not reached the composer's epoch and the earlier seed is
  // not on this device (a reload in that window): nothing is sealed under an
  // epoch the readers do not hold yet. A retry posts once the reseal lands.
  if (sealing === null) throw new ChannelError("failed");
  try {
    const epoch = sealing.epoch;
    const counter = channelsActions.nextCounter(idHex);
    const wire = await composePost({ ...keys, seed: sealing.seed }, idHex, epoch, counter, plaintext);
    const view = await publishPost(idHex, wire, identity);
    const state = loadRotationState(idHex);
    if (state !== null) {
      const wasm = await loadWasm();
      saveRotationState(idHex, wasm.channelRotationStateAfterPost(state));
    }
    return view.cursor;
  } catch (err) {
    throw new ChannelError(serviceErrorCode(err), { cause: err });
  }
}

// ---------------------------------------------------------------------------
// Subscribers (UX 15.1, 7.3)
// ---------------------------------------------------------------------------

export async function channelSubscriberCount(identity: WasmIdentity, idHex: string): Promise<number> {
  try {
    const n = await fetchSubscriberCount(idHex, identity);
    rememberSubscriberCount(idHex, n);
    return n;
  } catch (err) {
    throw new ChannelError(serviceErrorCode(err), { cause: err });
  }
}

export interface ChannelSubscriberRow {
  readonly pkHex: string;
  readonly via: { kind: "link"; label: string } | { kind: "invitation" } | null;
}

/** Rows a page of the subscriber list shows (chosen). */
const SUBSCRIBER_LIST_PAGE = 50;

/** One page of the subscriber rows, each with the link it came by when this
 *  device minted that link (a private channel's ticket). */
export async function channelSubscriberPage(
  identity: WasmIdentity,
  idHex: string,
  after: number,
): Promise<{ rows: readonly ChannelSubscriberRow[]; nextAfter: number; done: boolean }> {
  try {
    const page = await listSubscriberPage(idHex, identity, after, SUBSCRIBER_LIST_PAGE);
    const links = accessOf(idHex).inviteLinks;
    const rows: ChannelSubscriberRow[] = [];
    for (const e of page.entries) {
      const invitePk = await ticketInviteKeyHex(idHex, e.subscriberId, e.sealedTicket).catch(() => null);
      rows.push({ pkHex: e.subscriberId, via: subscriberVia(invitePk, links) });
    }
    return { rows, nextAfter: page.nextAfter, done: page.entries.length < SUBSCRIBER_LIST_PAGE };
  } catch (err) {
    throw new ChannelError(serviceErrorCode(err), { cause: err });
  }
}

type RequestRotation = (idHex: string) => Promise<void>;
let requestRotation: RequestRotation | null = null;

/** `ChannelPublisherLoops` installs how a co-publisher asks the owner's
 *  device to rotate (the rotate request on the PublisherGroup). */
export function installChannelRotateRequest(fn: RequestRotation | null): void {
  requestRotation = fn;
}

/** Private channel only: remove the key, then rotate at once (UX 7.3), so
 *  the removed device gets nothing from the next epoch. Only the owner's
 *  device rotates (maintainer, 2026-11-08); a co-publisher's device asks it
 *  to, with a rotate request on the PublisherGroup, and the owner's device
 *  rotates at its next run. */
export async function removeChannelSubscriber(identity: WasmIdentity, idHex: string, pkHex: string): Promise<void> {
  if (channelVisibilityOf(idHex) !== "private") throw new ChannelError("failed");
  for (const step of removalSteps(false)) {
    if (step === "remove") {
      try {
        await removeSubscriberAsPublisher(idHex, pkHex, identity);
      } catch (err) {
        throw new ChannelError(serviceErrorCode(err), { cause: err });
      }
    } else if (channelRotatesHere(idHex)) {
      await rotateChannelNow(idHex);
    } else {
      // The row is already gone: the request is owed from here on, written
      // durably before it is published, and retried by the publisher pass
      // until it goes out. The removal itself succeeded for the person.
      try {
        await channelsActions.updateAccessDurable(idHex, (r) => ({ ...r, rotateRequestOwed: true }));
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("[channels] owed rotate request not written durably", err instanceof Error ? err.name : "");
        channelsActions.updateAccess(idHex, (r) => ({ ...r, rotateRequestOwed: true }));
      }
      await settleOwedRotateRequest(idHex);
    }
  }
}

/**
 * Send the owed rotate request of `idHex` once, if one is owed (a
 * co-publisher's device after removing a subscriber). Cleared once it went
 * out; a failure keeps it owed for the next publisher pass. Never throws.
 */
export async function settleOwedRotateRequest(idHex: string): Promise<"none" | "sent" | "failed"> {
  if (accessOf(idHex).rotateRequestOwed !== true) return "none";
  if (requestRotation === null) return "failed";
  try {
    await requestRotation(idHex);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channels] rotate request not sent; retried next pass", err instanceof Error ? err.name : "");
    return "failed";
  }
  channelsActions.updateAccess(idHex, (r) => {
    if (r.rotateRequestOwed !== true) return r;
    const { rotateRequestOwed: _sent, ...rest } = r;
    return rest;
  });
  return "sent";
}


// ---------------------------------------------------------------------------
// Reactions setting, delete (UX 13.1, 11 / 12.1)
// ---------------------------------------------------------------------------

/** Owner: which reactions posts take (ADR-0087 section 13a). Re-issues the
 *  metadata record v3 with a new stamp. */
export async function saveChannelReactions(
  identity: WasmIdentity,
  idHex: string,
  allowed: AllowedSetting,
): Promise<void> {
  if (!isOwnerHere(idHex)) throw new ChannelError("not-publisher");
  const held = channelsState.metaByChannel().get(idHex);
  const local = channelsState.channels().find((c) => c.id === idHex);
  try {
    await publishChannelMeta(
      idHex,
      {
        name: held?.name ?? local?.localName ?? "",
        description: held?.description ?? local?.localDescription ?? "",
        reactions: allowed.mode === "only" ? { mode: "only", emoji: [...allowed.emoji] } : { mode: allowed.mode, emoji: [] },
      },
      identity,
    );
  } catch (err) {
    if (typeof err === "object" && err !== null && "channelMetaError" in err) throw err;
    throw new ChannelError(serviceErrorCode(err), { cause: err });
  }
}

/** What `deleteChannel` stops for a channel: `ChannelPublisherLoops`
 *  installs it (it drops the shared handle and forgets the channel's timers). */
type StopChannel = (idHex: string) => void;
let stopChannel: StopChannel | null = null;

/** `ChannelPublisherLoops` installs how a deleted channel is stopped. */
export function installChannelStop(fn: StopChannel | null): void {
  stopChannel = fn;
}

/**
 * Owner, when no other publisher remains (UX 11 / 12.1): retire the `@name`
 * (signed with the handle key), then, best effort, delete the Discover
 * listing, revoke the invite keys and the ledger rows of the links this
 * device minted, and stop the loops for it. The local wipe is the caller's
 * (`membershipEnd.endChannel`). ChannelError "offline" when the name could
 * not be retired, so nothing changed; "not-publisher" when not the owner, or
 * a publisher remains.
 */
export async function deleteChannel(lineage: WasmIdentity, current: WasmIdentity, idHex: string): Promise<void> {
  if (!isOwnerHere(idHex)) throw new ChannelError("not-publisher");
  if (accessOf(idHex).publishers.length > 0) throw new ChannelError("not-publisher");
  const handle = channelHandleOf(idHex);
  if (handle !== null) {
    const wasm = await loadWasm();
    try {
      await retireChannelHandle({
        wasm,
        lineageIdentity: lineage,
        currentIdentity: current,
        channelId: channelIdFromHex(idHex),
        handle,
      });
    } catch (err) {
      // A name already retired (or never held) is not a reason to keep the
      // channel; the network is.
      const code = handleErrorCode(err);
      if (code === "offline" || code === "too-many-today") throw new ChannelError("offline", { cause: err });
    }
  }
  // ADR-0087 phase B — the channel itself on the service: every later
  // request for it (subscribers', publishers') is `410 channel-deleted`,
  // which ends it on their devices. Nothing local goes before this lands.
  try {
    await deleteChannelOnService(idHex, lineage);
  } catch (err) {
    throw new ChannelError(serviceErrorCode(err), { cause: err });
  }
  await revokeOwnLinks(lineage, idHex);
  await forgetHandle("channel", idHex).catch(() => undefined);
  stopChannel?.(idHex);
}

/** Delete the ledger rows of the channel's links (this device's, and those
 *  the account's link register holds from its other devices), and revoke the
 *  register records. The channel is already deleted on the service, so its
 *  invite keys went with it. Best effort: a row that is not deleted stays
 *  dead with the channel. */
async function revokeOwnLinks(_identity: WasmIdentity, idHex: string): Promise<void> {
  const discovery = new DiscoveryClient(undefined, { sessionToken: () => sessionTokens.peek() });
  const done = new Set<string>();
  for (const r of registeredLinks()) {
    if (r.revoked || (r.kind !== "channel" && r.kind !== "publisher")) continue;
    if (bytesToHex(r.targetId) !== idHex.toLowerCase()) continue;
    try {
      await revokeRegisteredLink(r);
      done.add(bytesToHex(r.ledgerId));
    } catch {
      // Best effort.
    }
  }
  for (const link of accessOf(idHex).inviteLinks) {
    if (link.revoked || done.has(ledgerHexOf(link.ledgerIdB64Url))) continue;
    try {
      const std = link.ledgerIdB64Url.replace(/-/g, "+").replace(/_/g, "/");
      const blobId = b64ToBytes(std.padEnd(std.length + ((4 - (std.length % 4)) % 4), "="));
      const seed = b64ToBytes(link.revocationSeedB64);
      try {
        await discovery.revokeGroupInvite(blobId, seed);
      } finally {
        seed.fill(0);
      }
    } catch {
      // Best effort.
    }
  }
}

function ledgerHexOf(ledgerIdB64Url: string): string {
  const std = ledgerIdB64Url.replace(/-/g, "+").replace(/_/g, "/");
  return bytesToHex(b64ToBytes(std.padEnd(std.length + ((4 - (std.length % 4)) % 4), "=")));
}

/** For the loops: the access record of a channel (also `channelsActions`). */
export function channelAccessRecordOf(idHex: string): ChannelAccessRecord {
  return accessOf(idHex);
}

/** For the loops: file the public seed of the current epoch (idempotent). */
export async function ensurePublicSeed(identity: WasmIdentity, idHex: string): Promise<void> {
  const keys = channelsState.keysByChannel().get(idHex);
  if (keys === undefined || channelVisibilityOf(idHex) !== "public") return;
  const current = channelsState.epochByChannel().get(idHex);
  if (current === undefined) return;
  // The composer's seed under the epoch it serves, and only once the channel
  // reached that epoch (the reseal files the next epoch's seed itself).
  const epoch = keys.seedEpoch ?? current;
  if (epoch > current) return;
  keepPublisherSeed(idHex, epoch, keys.seed);
  await filePublicSeed(identity, idHex, epoch, keys.seed);
}

/** For the loops: after a rotation landed at `epoch` with `seed`. */
export function noteChannelRotated(idHex: string, epoch: number, seed: Uint8Array): void {
  channelsActions.setEpoch(idHex, epoch);
  keepPublisherSeed(idHex, epoch, seed);
}

/**
 * A PublisherGroup commit of this device finished (an approve, a removal, a
 * rotation): its steps, the reseal included, landed the channel at
 * `newEpoch` with `newSeed`. The seed being replaced stays filed under the
 * epoch it served (`newEpoch - 1`), the composer takes the new seed (the
 * signing seed kept), and the new epoch is the channel's. Before this the
 * owner's approve left the composer on the outgoing seed, so its next posts
 * went out under the new epoch's number sealed with the old epoch's seed,
 * which nobody else could open.
 */
export function notePublisherCommitLanded(idHex: string, newEpoch: number, newSeed: Uint8Array): void {
  notePublisherSeedAt(idHex, newEpoch, newSeed);
  channelsActions.setEpoch(idHex, newEpoch);
}

/**
 * The composer's seed moves to `seed`, which serves channel epoch `epoch`
 * (the PublisherGroup epoch it was read at). The seed it replaces stays
 * filed under the epoch it served, and the new one is filed under its own.
 * The channel's current epoch is left alone: on a co-publisher the owner's
 * reseal may not have moved it yet, and `publishChannelPost` keeps sealing
 * under the committed epoch until it does (`composerSeedFor`). Idempotent.
 */
export function notePublisherSeedAt(idHex: string, epoch: number, seed: Uint8Array): void {
  const prev = channelsState.keysByChannel().get(idHex);
  const kept = outgoingPublisherSeed(prev?.seed, prev?.seedEpoch ?? epoch - 1, seed, (e) => channelsActions.currentSeed(idHex, e) !== undefined);
  if (kept !== null) channelsActions.setSubscribedSeedForEpoch(idHex, kept.epoch, kept.seed);
  if (prev !== undefined && (prev.seedEpoch !== epoch || !sameSeed(prev.seed, seed))) {
    channelsActions.setKeys(idHex, { seed, signingKeySeed: prev.signingKeySeed, seedEpoch: epoch });
  }
  keepPublisherSeed(idHex, epoch, seed);
}

function sameSeed(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

/** Re-exported for screens that must tell a wasm type apart. */
export type { Wasm };
