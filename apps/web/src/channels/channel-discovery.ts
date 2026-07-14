// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `@channel` discovery — web client wiring for G4-5 / ADR-0010.
 *
 * A channel's on-the-wire identity is a 16-byte `channel_id`
 * pseudonym (hex-encoded in every `services/channels` REST route).
 * That is unfriendly to type or share, so — exactly like
 * `@username` — a channel may claim a human-readable `@handle`.
 * The mapping lives in the zero-trust discovery service behind the
 * SAME OPRF mechanism as usernames (`services/discovery/src/
 * channel_names.rs`): the server stores only
 * `OPRF(K, normalize(@handle)) -> channel_id`, never the plaintext
 * handle.
 *
 * This module is a thin channel-facing wrapper over
 * `DiscoveryClient.registerChannelName` / `lookupChannel` (which
 * own the multi-round-trip PoW → blind → evaluate → finalize →
 * PoW → register/lookup dance and reuse the wasm `OprfBlinder`
 * blind/finalize primitives — no OPRF is reimplemented here). It
 * only bridges the hex `channel_id` the channels UI speaks to the
 * raw-16-byte form the discovery client speaks.
 */

import {
  DiscoveryClient,
  DiscoveryHttpError,
  type ChannelRegisterResult,
} from "../discovery/client";
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
 * Register the `@handle → channel_id` mapping at channel-create
 * time. Session-token-gated write (a DoS rate-limit role — the
 * token's account is NOT compared against the channel; ownership
 * lives at the PublisherGroup layer, see the service module docs).
 *
 * @param handle       User-typed handle, with or without a leading
 *                     `@` — normalised inside `OprfBlinder.blind`.
 * @param channelIdHex The freshly-minted 32-char hex channel id.
 * @param sessionToken Bearer token for the gated write (from
 *                     `IdentitySecrets.sessionToken`).
 * @returns The canonical normalised handle actually claimed, so
 *          the UI can echo it back.
 * @throws  `DiscoveryHttpError` with `status === 409` when the
 *          handle is already taken (first-come-first-served).
 */
export async function registerChannelHandle(
  handle: string,
  channelIdHex: string,
  sessionToken: string,
): Promise<ChannelRegisterResult> {
  const channelId = channelIdFromHex(channelIdHex);
  const client = new DiscoveryClient();
  client.setSessionToken(sessionToken);
  return client.registerChannelName(channelId, handle);
}

/**
 * Resolve a typed `@handle` to its channel id hex, or `null` when
 * no channel has claimed that handle. Unauthenticated (the lookup
 * path is open); anonymous-safe.
 *
 * @param handle User-typed handle, with or without a leading `@`.
 * @returns 32-char hex `channel_id`, or `null` on an unknown
 *          handle (the server's 404 not-found path).
 */
export async function resolveChannelHandle(
  handle: string,
): Promise<string | null> {
  const client = new DiscoveryClient();
  const result = await client.lookupChannel(handle);
  if (result.channel === null) return null;
  return bytesToHex(result.channel);
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

/** Re-export so callers can branch on a 409-taken register without
 *  re-importing from the discovery module. */
export { DiscoveryHttpError };
