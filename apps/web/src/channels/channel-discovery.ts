// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `@channel` discovery — ADR-0087 sections 1 to 4 (one namespace).
 *
 * A channel's on-the-wire identity is a 16-byte `channel_id` (hex in every
 * `services/channels` route). Its `@name` lives in the same namespace as
 * people and groups: a handle claimed with the channel's handle key (derived
 * from the journal root of its PublisherGroup, so it names no account), with
 * a public record sealed under the name and its words. Finding a channel by
 * `@name` is the handle lookup plus `openSearchRecord` with the name's own
 * OPRF output. The old `/discovery/channel/*` routes are gone.
 *
 * Phase B (public / private channels, the read key's use, comments) is not
 * built: a named channel is public, found by its `@name` and its name.
 */

import { DiscoveryHttpError } from "../discovery/client";
import type { Identity as WasmIdentity } from "../../wasm/lakko_ffi_wasm";
import { claimChannelHandle, lookupChannelByHandle, type PublicChannelFacts } from "../chat/group-handle";
import { HandleWriteError } from "../discovery/handle-lookup";
import { noteHandleWrite } from "../chat/handle-owed";
import { loadWasm } from "../wasm";
import { bytesToHex, channelIdFromHex } from "./composer";

/** Raw 16-byte channel_id ⇒ 32 lowercase hex chars. */
const CHANNEL_ID_HEX_LEN = 32;

/**
 * `true` when `input` is a bare 32-char hex `channel_id` (16
 * bytes). Lets the subscribe entry point accept EITHER a pasted
 * raw id OR a typed `@handle` from one text field without a mode
 * switch — a hex id is taken verbatim, anything else is resolved
 * as a handle.
 */
export function looksLikeChannelIdHex(input: string): boolean {
  const trimmed = input.trim();
  return (
    trimmed.length === CHANNEL_ID_HEX_LEN && /^[0-9a-fA-F]+$/.test(trimmed)
  );
}

/**
 * Claim the channel's `@name` at channel-create time (ADR-0087 section 3):
 * one `discovery-register` token for the claim, one `discovery-lookup` token
 * per name term of the public record.
 *
 * @param handle        User-typed handle, with or without a leading `@`.
 * @param channelIdHex  The freshly minted 32-char hex channel id.
 * @param ownerIdentity The identity the PublisherGroup is founded under.
 * @param facts         The channel's name and description, for its record.
 * @returns The canonical name actually claimed.
 * @throws  `HandleWriteError` with `code === "name-taken"` (409) when the
 *          name is held.
 */
export async function registerChannelHandle(
  handle: string,
  channelIdHex: string,
  ownerIdentity: WasmIdentity,
  facts: PublicChannelFacts,
  accountIdB64?: string | null,
): Promise<{ readonly handle: string }> {
  const wasm = await loadWasm();
  const written = await claimChannelHandle({
    wasm,
    ownerIdentity,
    channelId: channelIdFromHex(channelIdHex),
    handle,
    facts,
  });
  if (accountIdB64 !== undefined && accountIdB64 !== null) {
    await noteHandleWrite({
      kind: "channel",
      id: channelIdHex,
      handle: written.handle,
      isPublic: true,
      lastWriteMs: Date.now(),
      leaseUntilMs: written.leaseUntilMs,
    }).catch(() => undefined);
  }
  return { handle: written.handle };
}

/**
 * Resolve a typed `@handle` to its channel id hex, or `null` when no public
 * channel holds that name.
 */
export async function resolveChannelHandle(handle: string): Promise<string | null> {
  const wasm = await loadWasm();
  const id = await lookupChannelByHandle(wasm, handle);
  return id === null ? null : bytesToHex(id);
}

/**
 * Convenience for the subscribe entry point: accept EITHER a
 * pasted 32-char hex `channel_id` OR a typed `@handle`, and return
 * the resolved channel id hex (or `null` when a handle does not
 * resolve). A bare hex id is returned verbatim (lowercased) with
 * no network round-trip; anything else is OPRF-resolved.
 */
export async function resolveChannelIdOrHandle(
  input: string,
): Promise<string | null> {
  const trimmed = input.trim();
  if (trimmed.length === 0) return null;
  if (looksLikeChannelIdHex(trimmed)) {
    return trimmed.toLowerCase();
  }
  return resolveChannelHandle(trimmed);
}

/** Re-export so callers can branch on a refused claim without re-importing
 *  from the discovery module. */
export { DiscoveryHttpError, HandleWriteError };
