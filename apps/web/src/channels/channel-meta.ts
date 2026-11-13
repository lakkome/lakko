// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0051 §4 — the channel metadata record (name + description) that rides
 * as the PLAINTEXT of an ordinary channel post.
 *
 * Publisher side: {@link publishChannelMeta} composes the record with the
 * core's `composeChannelMeta`, seals + signs it through the normal post
 * path (consuming a counter like every post), publishes it, and keeps the
 * publisher's own copy in the channel log — the source of the next record.
 * {@link republishChannelMeta} re-issues that copy with its ORIGINAL stamp
 * after a rotation, so the newest epoch always holds a record a late
 * subscriber can read.
 *
 * Reader side: {@link newestChannelMeta} picks the newest record out of a
 * decoded post page by the core's `channelMetaIsNewer` (strictly greater,
 * per channel), and {@link foldChannelMeta} installs it when it beats what
 * the device holds. A record is never rendered as a post — `PostCard.ts`'s
 * `toPostDecode` classifies it out of the list before this runs.
 *
 * Authenticity is the post signature (only a publisher can post) and
 * confidentiality the epoch seed; nothing here adds a second check.
 */

import { t } from "../l10n";
import { KeyedError } from "../l10n/keyed-error";
import type { PostDecode } from "./post-decode";
import { composePost, type WasmIdentity } from "./composer";
import { publishPost } from "./api";
import { composerSeedFor } from "./publisher-seed";
import type { ChannelSharedMeta } from "./channel-log-codec";
import type { ChannelAccessRecord } from "./channel-access-record";
import { channelsActions, channelsState } from "../state/channels-state";
import { notePublicRecordChangedHere } from "../chat/handle-owed";
import { loadWasm, type Wasm } from "../wasm";
import { avatarFromWasm, avatarToWasm, b64ToBytes, bytesToB64, type AvatarData } from "../profile/avatar-model";

/** UX spec §4 — an empty channel name is refused. */
export const CHANNEL_NEEDS_NAME = (): string => t("channels.meta.error.name_empty");

/** The one wasm export the fold needs, structurally. */
export type ChannelMetaIsNewer = (incomingUpdatedAtMs: bigint, storedUpdatedAtMs: bigint) => boolean;

/**
 * The newest metadata record among `decodes`, or `null` when the page
 * carried none. Ties keep the first seen — a duplicate record is the same
 * record.
 */
export function newestChannelMeta(
  decodes: Iterable<PostDecode>,
  isNewer: ChannelMetaIsNewer,
): ChannelSharedMeta | null {
  return newestChannelMetaRecord(decodes, isNewer)?.meta ?? null;
}

/** {@link newestChannelMeta} with the record's plaintext, when the decode
 *  kept it (the v3 fields are re-read from it). */
export function newestChannelMetaRecord(
  decodes: Iterable<PostDecode>,
  isNewer: ChannelMetaIsNewer,
): { readonly meta: ChannelSharedMeta; readonly plaintext: Uint8Array | undefined } | null {
  let best: { meta: ChannelSharedMeta; plaintext: Uint8Array | undefined } | null = null;
  for (const d of decodes) {
    if (d.kind !== "meta") continue;
    if (best === null || isNewer(d.meta.updatedAtMs, best.meta.updatedAtMs)) {
      best = { meta: d.meta, plaintext: d.plaintext };
    }
  }
  return best;
}

/** The ADR-0087 metadata v3 fields of a record's plaintext (`handle`, photo,
 *  allowed reactions), each absent when the record does not carry it. The
 *  handle is the SIGNED envelope as received: a reader shows it, a re-issue
 *  carries it verbatim. Never throws. */
export function metaV3FromPlaintext(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
  plaintext: Uint8Array,
): Pick<ChannelSharedMeta, "handle" | "avatar" | "reactions"> {
  const out: { handle?: NonNullable<ChannelSharedMeta["handle"]>; avatar?: AvatarData; reactions?: NonNullable<ChannelSharedMeta["reactions"]> } = {};
  let c: ReturnType<typeof wasm.classifyChannelPostPlaintext> | undefined;
  try {
    c = wasm.classifyChannelPostPlaintext(plaintext);
    if (c.kind !== "meta") return out;
    const h = c.handle;
    if (h !== undefined) {
      try {
        out.handle = {
          handle: h.handle,
          isPublic: h.public,
          envelopeB64: bytesToB64(wasm.composeConversationHandleEnvelope(h)),
        };
      } finally {
        h.free();
      }
    }
    const a = avatarFromWasm(c.avatar);
    if (a !== null) out.avatar = a;
    const r = c.reactions;
    if (r !== undefined) {
      try {
        const mode = r.mode;
        if (mode === "all" || mode === "off" || mode === "only") out.reactions = { mode, emoji: [...r.emoji] };
      } finally {
        r.free();
      }
    }
  } catch {
    /* a record the core refuses carries nothing */
  } finally {
    try {
      c?.free();
    } catch {
      /* best effort */
    }
  }
  return out;
}

/**
 * Whether `candidate` must replace `held` for a channel: anything beats
 * nothing; otherwise the core's strict order decides.
 */
export function channelMetaSupersedes(
  candidate: ChannelSharedMeta,
  held: ChannelSharedMeta | undefined,
  isNewer: ChannelMetaIsNewer,
): boolean {
  return held === undefined || isNewer(candidate.updatedAtMs, held.updatedAtMs);
}

/**
 * Reader side: install the newest record of a decoded page when it beats
 * what this device holds for `channelIdHex`. Never throws.
 */
export async function foldChannelMeta(
  channelIdHex: string,
  decodes: Iterable<PostDecode>,
): Promise<void> {
  try {
    const wasm = await loadWasm();
    const isNewer: ChannelMetaIsNewer = (a, b) => wasm.channelMetaIsNewer(a, b);
    const record = newestChannelMetaRecord(decodes, isNewer);
    if (record === null) return;
    const newest: ChannelSharedMeta =
      record.plaintext === undefined ? record.meta : { ...record.meta, ...metaV3FromPlaintext(wasm, record.plaintext) };
    const held = channelsState.metaByChannel().get(channelIdHex);
    if (channelMetaSupersedes(newest, held, isNewer)) {
      // Owed before the change is kept (no crash loses it) and after (a
      // republish that ran in between published the old value).
      await owePublicChannelRecord(channelIdHex, held, newest);
      channelsActions.setSharedMeta(channelIdHex, newest);
      await owePublicChannelRecord(channelIdHex, held, newest);
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("channel meta fold failed", err);
  }
}

/**
 * UX 4.5.4 — a public channel's record carries its name and description: the
 * owner's device (it holds the handle key; a co-publisher does not) owes a
 * republish when it keeps a change to either, whoever made it. Never throws.
 */
/** Whether keeping `after` over `before` changes what a channel's public
 *  record carries (its name or description). Pure. */
export function channelRecordChanged(
  before: Pick<ChannelSharedMeta, "name" | "description"> | undefined,
  after: Pick<ChannelSharedMeta, "name" | "description">,
): boolean {
  return before === undefined || before.name !== after.name || before.description !== after.description;
}

async function owePublicChannelRecord(
  channelIdHex: string,
  before: ChannelSharedMeta | undefined,
  after: ChannelSharedMeta,
): Promise<void> {
  if (!channelRecordChanged(before, after)) return;
  const channel = channelsState.channels().find((c) => c.id === channelIdHex);
  if (channel === undefined || (channel.localHandle ?? "").length === 0) return;
  if (!channelsState.keysByChannel().has(channelIdHex)) return;
  if (channelsActions.coPublisherGroupIdOf(channelIdHex) !== undefined) return;
  try {
    await notePublicRecordChangedHere("channel", channelIdHex);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[channel-handle] republish not recorded", err instanceof Error ? err.name : "");
  }
}

/** The `channelMetaError` code off a composer throw, or `undefined`. */
export function channelMetaErrorCode(err: unknown): string | undefined {
  if (typeof err !== "object" || err === null) return undefined;
  const code = (err as { channelMetaError?: unknown }).channelMetaError;
  return typeof code === "string" ? code : undefined;
}

/** Plain copy for a `channelMetaError` code. */
export function channelMetaErrorText(code: string | undefined): string {
  switch (code) {
    case "name.empty":
      return CHANNEL_NEEDS_NAME();
    case "name.tooLong":
    case "name.tooManyBytes":
      return t("channels.meta.error.name_too_long");
    case "name.lineBreak":
      return t("channels.meta.error.name_line_break");
    case "name.controlOrFormatCharacter":
      return t("channels.meta.error.name_bad_character");
    case "description.tooLong":
    case "description.tooManyBytes":
      return t("channels.meta.error.description_too_long");
    case "description.controlOrFormatCharacter":
      return t("channels.meta.error.description_bad_character");
    default:
      return t("channels.meta.error.generic");
  }
}

/**
 * Publisher side: post a metadata record for `channelIdHex` and keep it as
 * this device's copy. `stampMs` defaults to now; a re-issue passes the
 * stored stamp so it never out-orders a later edit.
 *
 * Throws the composer's `channelMetaError` (see {@link channelMetaErrorCode})
 * when the inputs fail a bound, and the publish failure otherwise. The
 * inputs are trimmed here: the composer refuses untrimmed text exactly as
 * every reader's decoder does.
 */
export async function publishChannelMeta(
  channelIdHex: string,
  input: {
    readonly name: string;
    readonly description: string;
    /** ADR-0059 metadata v2; each absent field keeps what this device holds. */
    readonly topic?: number;
    readonly language?: string;
    readonly listed?: boolean;
    /** ADR-0087 v3; each absent field keeps what this device holds. `null`
     *  clears the photo / the name. */
    readonly handle?: ChannelSharedMeta["handle"] | null;
    readonly avatar?: AvatarData | null;
    readonly reactions?: ChannelSharedMeta["reactions"];
  },
  identity: WasmIdentity,
  stampMs: bigint = BigInt(Date.now()),
): Promise<ChannelSharedMeta> {
  const keys = channelsState.keysByChannel().get(channelIdHex);
  if (keys === undefined) throw new KeyedError("channels.publish.error.not_publisher");
  const name = input.name.trim();
  const description = input.description.trim();
  if (name.length === 0) {
    throw Object.assign(new Error("channel name is empty"), { channelMetaError: "name.empty" });
  }
  const wasm = await loadWasm();
  // ADR-0059 metadata v2: the Discover fields the owner set, kept across a
  // rename; a channel never listed is topic "Other", language unset, not
  // listed.
  const held = channelsState.metaByChannel().get(channelIdHex);
  // No record held yet (the first one never landed): the owner's device
  // states its channel from its own access record, never "not public" by
  // default.
  const own =
    held === undefined && channelsActions.coPublisherGroupIdOf(channelIdHex) === undefined
      ? planRecordReissue({
          held: undefined,
          access: channelsActions.accessOf(channelIdHex),
          localName: name,
          localDescription: description,
          isOwner: true,
          nowMs: 0,
        })
      : undefined;
  const fallback = own?.kind === "fresh" ? own.input : undefined;
  const topic = input.topic ?? held?.topic ?? fallback?.topic ?? 0;
  const language = input.language ?? held?.language ?? fallback?.language ?? "";
  const listed = input.listed ?? held?.listed ?? fallback?.listed ?? false;
  const handle =
    input.handle === undefined ? (held?.handle ?? fallback?.handle ?? undefined) : (input.handle ?? undefined);
  const avatar =
    input.avatar === undefined ? (held?.avatar ?? fallback?.avatar ?? undefined) : (input.avatar ?? undefined);
  const reactions = input.reactions ?? held?.reactions;
  const composed = composeChannelMetaRecord(wasm, { name, description, stampMs, topic, language, listed, handle, avatar, reactions });
  const plaintext = composed.plaintext;
  // Sealed as every post is (`publishChannelPost`): the composer's seed under
  // the epoch it serves, never the composer's seed under the channel's
  // current epoch number. After a commit the two differ, and a record sealed
  // with the new seed under the old epoch opens for nobody (seen live
  // 2026-11-13: "This post isn't available." on the owner and a subscriber).
  const sealing = composerSeedFor(keys, channelsState.epochByChannel().get(channelIdHex), (e) =>
    channelsActions.currentSeed(channelIdHex, e),
  );
  if (sealing === null) throw new KeyedError("channels.publish.error.generic");
  const counter = channelsActions.nextCounter(channelIdHex);
  const wire = await composePost({ ...keys, seed: sealing.seed }, channelIdHex, sealing.epoch, counter, plaintext);
  const meta: ChannelSharedMeta = {
    name,
    description,
    updatedAtMs: stampMs,
    topic,
    language,
    listed,
    ...(handle === undefined || !composed.handleKept ? {} : { handle }),
    ...(avatar === undefined ? {} : { avatar }),
    ...(reactions === undefined ? {} : { reactions }),
  };
  await owePublicChannelRecord(channelIdHex, held, meta);
  await publishPost(channelIdHex, wire, identity);
  channelsActions.setSharedMeta(channelIdHex, meta);
  await owePublicChannelRecord(channelIdHex, held, meta);
  return meta;
}

/**
 * Compose a v3 record, freeing every wasm handle it builds and does not hand
 * to the core.
 *
 * `composeChannelMeta` takes its handle, photo and reactions BY VALUE: the
 * call moves each object into Rust and leaves its JS wrapper empty, and a
 * `free()` on an emptied wrapper throws "null pointer passed to rust". Each
 * reference is therefore cleared before the call and only what was not handed
 * over is freed (seen live 2026-11-13: every record carrying a signed `@name`
 * threw after composing, so a public channel's first record never posted).
 *
 * A handle envelope that does not decode, or whose signed flag is not
 * `listed`, is left out (the core refuses the pair), so a stale envelope
 * never blocks the record; `handleKept` says whether it went in.
 */
export function composeChannelMetaRecord(
  wasm: Wasm,
  m: {
    readonly name: string;
    readonly description: string;
    readonly stampMs: bigint;
    readonly topic: number;
    readonly language: string;
    readonly listed: boolean;
    readonly handle: ChannelSharedMeta["handle"] | undefined;
    readonly avatar: AvatarData | undefined;
    readonly reactions: ChannelSharedMeta["reactions"] | undefined;
  },
): { readonly plaintext: Uint8Array; readonly handleKept: boolean } {
  let h: Wasm["ConversationHandle"]["prototype"] | undefined;
  let a: Wasm["ProfileAvatar"]["prototype"] | undefined;
  let r: Wasm["AllowedReactions"]["prototype"] | undefined;
  try {
    if (m.handle !== undefined) {
      try {
        h = wasm.decodeConversationHandleEnvelope(b64ToBytes(m.handle.envelopeB64));
      } catch {
        h = undefined;
      }
      if (h !== undefined && h.public !== m.listed) {
        h.free();
        h = undefined;
      }
    }
    if (m.avatar !== undefined) a = avatarToWasm<Wasm["ProfileAvatar"]["prototype"]>(wasm, m.avatar);
    if (m.reactions !== undefined) {
      r =
        m.reactions.mode === "off"
          ? wasm.AllowedReactions.off()
          : m.reactions.mode === "only"
            ? wasm.AllowedReactions.only([...m.reactions.emoji])
            : wasm.AllowedReactions.all();
    }
    // Moved into the core by the call, whether it succeeds or throws.
    const handle = h;
    const avatar = a;
    const reactions = r;
    h = undefined;
    a = undefined;
    r = undefined;
    const plaintext = wasm.composeChannelMeta(
      m.name,
      m.description,
      m.stampMs,
      m.topic,
      m.language,
      m.listed,
      handle,
      avatar,
      reactions,
    );
    return { plaintext, handleKept: handle !== undefined };
  } finally {
    h?.free();
    a?.free();
    r?.free();
  }
}

/** What a record re-issue is built from: the held record, the owner's own
 *  access record (visibility, signed `@name`, topic, language, photo), and
 *  the channel's local name. */
export interface RecordReissueSources {
  readonly held: ChannelSharedMeta | undefined;
  readonly access: Pick<
    ChannelAccessRecord,
    "visibility" | "handle" | "handleEnvelopeB64" | "topic" | "language" | "avatar"
  >;
  readonly localName: string | undefined;
  readonly localDescription: string | undefined;
  /** This device owns the channel: its own access record states the flag. */
  readonly isOwner: boolean;
  readonly nowMs: number;
}

/** The fields {@link publishChannelMeta} takes. */
export type RecordInput = Parameters<typeof publishChannelMeta>[1];

export type RecordReissuePlan =
  | { readonly kind: "none" }
  | { readonly kind: "verbatim"; readonly input: RecordInput; readonly stampMs: bigint }
  | { readonly kind: "fresh"; readonly input: RecordInput; readonly stampMs: bigint };

/**
 * Which record a re-issue posts (ADR-0051 section 4, ADR-0087 sections 11
 * and 16). Pure.
 *
 * - The held record, verbatim with its ORIGINAL stamp, when it agrees with
 *   what the owner's device holds (or this device is not the owner).
 * - The owner's device holding a record that disagrees with its own setting
 *   (a public channel recorded as not public, or missing the signed `@name`
 *   the access record holds): a corrected record under a fresh stamp, so it
 *   replaces the wrong one at every reader that already holds it (newest
 *   stamp wins).
 * - No held record (the first record never landed): the record composed
 *   from the access record and the local name, under a fresh stamp.
 *
 * Never a record whose flag this device does not know: with no held record
 * and no known visibility nothing is posted (`none`), so a public channel is
 * never published as not public.
 */
export function planRecordReissue(src: RecordReissueSources): RecordReissuePlan {
  const { held, access } = src;
  const visibility = access.visibility;
  const listed = visibility === null ? undefined : visibility === "public";
  const accessHandle =
    access.handle !== null && access.handleEnvelopeB64 !== null && listed !== undefined
      ? { handle: access.handle, isPublic: listed, envelopeB64: access.handleEnvelopeB64 }
      : undefined;
  const fresh = (base: bigint | undefined): bigint => {
    const now = BigInt(Math.max(0, Math.floor(src.nowMs)));
    return base !== undefined && now <= base ? base + 1n : now;
  };
  if (held !== undefined) {
    const agrees =
      !src.isOwner ||
      listed === undefined ||
      ((held.listed ?? false) === listed && (accessHandle === undefined || held.handle?.isPublic === listed));
    if (agrees) return { kind: "verbatim", input: held, stampMs: held.updatedAtMs };
    return {
      kind: "fresh",
      stampMs: fresh(held.updatedAtMs),
      input: {
        name: held.name,
        description: held.description,
        topic: access.topic ?? held.topic ?? 0,
        language: access.language ?? held.language ?? "",
        listed,
        handle: accessHandle ?? (held.handle?.isPublic === listed ? held.handle : null),
        avatar: held.avatar ?? access.avatar ?? null,
        ...(held.reactions === undefined ? {} : { reactions: held.reactions }),
      },
    };
  }
  const name = (src.localName ?? "").trim();
  if (name.length === 0 || listed === undefined) return { kind: "none" };
  return {
    kind: "fresh",
    stampMs: fresh(undefined),
    input: {
      name,
      description: (src.localDescription ?? "").trim(),
      topic: access.topic ?? 0,
      language: access.language ?? "",
      listed,
      handle: accessHandle ?? null,
      avatar: access.avatar,
    },
  };
}

/**
 * Owner's device: when the record it holds contradicts its own flag (a
 * public channel recorded as not public), mark the record owed, so the publisher pass posts the corrected
 * one under a fresh stamp and every reader holding the wrong record replaces
 * it. Returns whether it marked one.
 */
export function oweRecordIfContradicted(channelIdHex: string): boolean {
  const held = channelsState.metaByChannel().get(channelIdHex);
  if (held === undefined) return false;
  const access = channelsActions.accessOf(channelIdHex);
  if (access.recordOwed === true) return false;
  const isOwner =
    channelsState.keysByChannel().has(channelIdHex) && channelsActions.coPublisherGroupIdOf(channelIdHex) === undefined;
  if (!isOwner) return false;
  // The flag only: a corrected record always carries the owner's flag, so
  // this converges after one post. (A missing signed name is not checked
  // here: an envelope the core leaves out would be owed again every pass.)
  if (access.visibility === null || (held.listed ?? false) === (access.visibility === "public")) return false;
  channelsActions.updateAccess(channelIdHex, (r) => ({ ...r, recordOwed: true }));
  return true;
}

/**
 * Re-issue the record this device holds for `channelIdHex` (after a
 * rotation, so the new epoch carries one), as {@link planRecordReissue}
 * decides: verbatim with its original stamp, or corrected / composed from
 * what the owner's device holds under a fresh stamp. Resolves `false` when
 * there is nothing to post. Throws what {@link publishChannelMeta} throws;
 * the caller keeps it owed.
 */
export async function republishChannelMeta(
  channelIdHex: string,
  identity: WasmIdentity,
): Promise<boolean> {
  const local = channelsState.channels().find((c) => c.id === channelIdHex);
  const plan = planRecordReissue({
    held: channelsState.metaByChannel().get(channelIdHex),
    access: channelsActions.accessOf(channelIdHex),
    localName: local?.localName,
    localDescription: local?.localDescription,
    isOwner: channelsState.keysByChannel().has(channelIdHex) && channelsActions.coPublisherGroupIdOf(channelIdHex) === undefined,
    nowMs: Date.now(),
  });
  if (plan.kind === "none") return false;
  await publishChannelMeta(channelIdHex, plan.input, identity, plan.stampMs);
  return true;
}
