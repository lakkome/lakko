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

import type { PostDecode } from "./PostCard";
import { composePost } from "./composer";
import { publishPost } from "./api";
import type { ChannelSharedMeta } from "./channel-log-codec";
import { channelsActions, channelsState } from "../state/channels-state";
import { loadWasm } from "../wasm";

/** UX spec §4 — an empty channel name is refused. */
export const CHANNEL_NEEDS_NAME = "A channel needs a name";

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
  let best: ChannelSharedMeta | null = null;
  for (const d of decodes) {
    if (d.kind !== "meta") continue;
    if (best === null || isNewer(d.meta.updatedAtMs, best.updatedAtMs)) best = d.meta;
  }
  return best;
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
    const newest = newestChannelMeta(decodes, isNewer);
    if (newest === null) return;
    if (channelMetaSupersedes(newest, channelsState.metaByChannel().get(channelIdHex), isNewer)) {
      channelsActions.setSharedMeta(channelIdHex, newest);
    }
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("channel meta fold failed", err);
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
      return CHANNEL_NEEDS_NAME;
    case "name.tooLong":
    case "name.tooManyBytes":
      return "The name is too long.";
    case "name.lineBreak":
      return "The name must be one line.";
    case "name.controlOrFormatCharacter":
      return "The name contains a character that cannot be shown.";
    case "description.tooLong":
    case "description.tooManyBytes":
      return "The description is too long.";
    case "description.controlOrFormatCharacter":
      return "The description contains a character that cannot be shown.";
    default:
      return "That name or description cannot be used.";
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
  input: { readonly name: string; readonly description: string },
  stampMs: bigint = BigInt(Date.now()),
): Promise<ChannelSharedMeta> {
  const keys = channelsState.keysByChannel().get(channelIdHex);
  if (keys === undefined) throw new Error("This device cannot post to this channel.");
  const name = input.name.trim();
  const description = input.description.trim();
  if (name.length === 0) {
    throw Object.assign(new Error(CHANNEL_NEEDS_NAME), { channelMetaError: "name.empty" });
  }
  const wasm = await loadWasm();
  const plaintext = wasm.composeChannelMeta(name, description, stampMs);
  const epoch = channelsState.epochByChannel().get(channelIdHex) ?? 0;
  const counter = channelsActions.nextCounter(channelIdHex);
  const wire = await composePost(keys, channelIdHex, epoch, counter, plaintext);
  await publishPost(channelIdHex, wire);
  const meta: ChannelSharedMeta = { name, description, updatedAtMs: stampMs };
  channelsActions.setSharedMeta(channelIdHex, meta);
  return meta;
}

/**
 * Re-issue the record this device holds for `channelIdHex` with its
 * ORIGINAL stamp (after a rotation, so the new epoch carries one). A
 * channel created before records existed has no copy; its local name is
 * posted as a first record with a fresh stamp instead. A channel with
 * neither posts nothing (`false`). Throws what {@link publishChannelMeta}
 * throws; the rotation caller logs and moves on.
 */
export async function republishChannelMeta(channelIdHex: string): Promise<boolean> {
  const held = channelsState.metaByChannel().get(channelIdHex);
  if (held !== undefined) {
    await publishChannelMeta(channelIdHex, held, held.updatedAtMs);
    return true;
  }
  const local = channelsState.channels().find((c) => c.id === channelIdHex);
  if (local === undefined || local.localName.trim().length === 0) return false;
  await publishChannelMeta(channelIdHex, {
    name: local.localName,
    description: local.localDescription,
  });
  return true;
}
