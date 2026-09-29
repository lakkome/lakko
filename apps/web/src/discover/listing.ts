// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The owner's side of Discover (ADR-0059 sections 2-3, docs/ux/feed.md 6.4):
 * list a channel, withdraw it, and keep the listing in step with a rename.
 *
 * Listing writes two things, in this order:
 *
 *   1. the public listing (`PUT /v1/channels/{id}/listing`, owner-signed): if
 *      the server refuses it the switch goes back to off and nothing else
 *      happens ("Couldn't list this channel. Try again.");
 *   2. channel metadata v2 with `listed = true` and the topic and language,
 *      an ordinary encrypted post, so subscribers learn the channel is
 *      listed (what lets them share it in a group, section 6).
 *
 * Withdrawing deletes the listing and posts metadata with `listed = false`.
 */

import type { Wasm } from "../wasm";
import { loadWasm } from "../wasm";
import { deleteListing, fetchListing, putListing } from "../channels/api";
import { publishChannelMeta } from "../channels/channel-meta";
import { channelIdFromHex } from "../channels/composer";
import { channelsState } from "../state/channels-state";

type Identity = InstanceType<Wasm["Identity"]>;

/** The UX spec's copy. */
export const LISTING_REFUSED = "Couldn't list this channel. Try again.";
export const LISTING_CONFIRM =
  "Anyone can find this channel's name, description and subscriber count. Posts stay encrypted.";
export const LISTING_NEEDS_HANDLE = "A channel needs an @handle to be listed in Discover.";

/** What the server currently lists for a channel: its stamp, or `null`. */
export async function listedStamp(channelIdHex: string): Promise<bigint | null> {
  const item = await fetchListing(channelIdHex);
  if (item === null) return null;
  const wasm = await loadWasm();
  try {
    const bytes = base64UrlToBytes(item.listing_b64);
    const record = wasm.decodeChannelListing(bytes);
    try {
      return record.updatedAtMs;
    } finally {
      record.free();
    }
  } catch {
    // A listing this client cannot decode is still a listing.
    return 0n;
  }
}

/** A stamp strictly after the stored one. */
export function nextListingStamp(nowMs: number, stored: bigint | null): bigint {
  const now = BigInt(Math.max(0, Math.floor(nowMs)));
  if (stored === null || now > stored) return now;
  return stored + 1n;
}

function base64UrlToBytes(b64: string): Uint8Array {
  const std = b64.replace(/-/g, "+").replace(/_/g, "/").padEnd(b64.length + ((4 - (b64.length % 4)) % 4), "=");
  const bin = atob(std);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

/** The name, description and handle the listing publishes. */
function listingSource(channelIdHex: string): { handle: string | undefined; name: string; description: string } {
  const meta = channelsState.metaByChannel().get(channelIdHex);
  const local = channelsState.channels().find((c) => c.id === channelIdHex);
  return {
    handle: local?.localHandle,
    name: (meta?.name ?? local?.localName ?? "").trim(),
    description: (meta?.description ?? local?.localDescription ?? "").trim(),
  };
}

/** Put the listing for the channel as it is now, stamped after `stored`. */
async function putCurrentListing(
  channelIdHex: string,
  identity: Identity,
  topic: number,
  language: string,
  stored: bigint | null,
): Promise<void> {
  const src = listingSource(channelIdHex);
  if (src.handle === undefined || src.handle.length === 0) throw new Error(LISTING_NEEDS_HANDLE);
  const wasm = await loadWasm();
  const bytes = wasm.composeChannelListing(
    channelIdFromHex(channelIdHex),
    src.handle,
    src.name,
    src.description,
    topic,
    language,
    nextListingStamp(Date.now(), stored),
  );
  await putListing(channelIdHex, identity, bytes);
}

/**
 * List the channel. Throws `LISTING_REFUSED`-worded errors: the listing PUT
 * failing is "Couldn't list"; the metadata post failing after it is reported
 * separately, since the listing is then live.
 */
export async function listChannel(
  channelIdHex: string,
  identity: Identity,
  topic: number,
  language: string,
): Promise<void> {
  const stored = await listedStamp(channelIdHex).catch(() => null);
  await putCurrentListing(channelIdHex, identity, topic, language, stored);
  const src = listingSource(channelIdHex);
  await publishChannelMeta(channelIdHex, {
    name: src.name,
    description: src.description,
    topic,
    language,
    listed: true,
  });
}

/** Withdraw the listing and tell subscribers the channel is no longer listed. */
export async function unlistChannel(channelIdHex: string, identity: Identity): Promise<void> {
  await deleteListing(channelIdHex, identity);
  const src = listingSource(channelIdHex);
  await publishChannelMeta(channelIdHex, { name: src.name, description: src.description, listed: false });
}

/** After a rename (ADR-0051 edit) of a listed channel: re-put the listing so
 *  it carries the new name and description. No-op when not listed. */
export async function refreshListingAfterEdit(channelIdHex: string, identity: Identity): Promise<void> {
  const meta = channelsState.metaByChannel().get(channelIdHex);
  if (meta?.listed !== true || meta.language === undefined || meta.language === "") return;
  const stored = await listedStamp(channelIdHex);
  if (stored === null) return;
  await putCurrentListing(channelIdHex, identity, meta.topic ?? 0, meta.language, stored);
}
