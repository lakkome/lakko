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
import type { ChannelSharedMeta } from "./channel-log-codec";
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
  const topic = input.topic ?? held?.topic ?? 0;
  const language = input.language ?? held?.language ?? "";
  const listed = input.listed ?? held?.listed ?? false;
  const handle = input.handle === undefined ? held?.handle : (input.handle ?? undefined);
  const avatar = input.avatar === undefined ? held?.avatar : (input.avatar ?? undefined);
  const reactions = input.reactions ?? held?.reactions;
  const plaintext = composeMetaV3(wasm, { name, description, stampMs, topic, language, listed, handle, avatar, reactions });
  const epoch = channelsState.epochByChannel().get(channelIdHex) ?? 0;
  const counter = channelsActions.nextCounter(channelIdHex);
  const wire = await composePost(keys, channelIdHex, epoch, counter, plaintext);
  const meta: ChannelSharedMeta = {
    name,
    description,
    updatedAtMs: stampMs,
    topic,
    language,
    listed,
    ...(handle === undefined ? {} : { handle }),
    ...(avatar === undefined ? {} : { avatar }),
    ...(reactions === undefined ? {} : { reactions }),
  };
  await owePublicChannelRecord(channelIdHex, held, meta);
  await publishPost(channelIdHex, wire, identity);
  channelsActions.setSharedMeta(channelIdHex, meta);
  await owePublicChannelRecord(channelIdHex, held, meta);
  return meta;
}

/** Compose a v3 record, freeing every wasm handle it builds. */
function composeMetaV3(
  wasm: Awaited<ReturnType<typeof loadWasm>>,
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
): Uint8Array {
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
    return wasm.composeChannelMeta(m.name, m.description, m.stampMs, m.topic, m.language, m.listed, h, a, r);
  } finally {
    h?.free();
    a?.free();
    r?.free();
  }
}

/**
 * Re-issue the record this device holds for `channelIdHex` with its
 * ORIGINAL stamp (after a rotation, so the new epoch carries one). A
 * channel created before records existed has no copy; its local name is
 * posted as a first record with a fresh stamp instead. A channel with
 * neither posts nothing (`false`). Throws what {@link publishChannelMeta}
 * throws; the rotation caller logs and moves on.
 */
export async function republishChannelMeta(
  channelIdHex: string,
  identity: WasmIdentity,
): Promise<boolean> {
  const held = channelsState.metaByChannel().get(channelIdHex);
  if (held !== undefined) {
    await publishChannelMeta(channelIdHex, held, identity, held.updatedAtMs);
    return true;
  }
  const local = channelsState.channels().find((c) => c.id === channelIdHex);
  if (local === undefined || local.localName.trim().length === 0) return false;
  await publishChannelMeta(
    channelIdHex,
    { name: local.localName, description: local.localDescription },
    identity,
  );
  return true;
}
