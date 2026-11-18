// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A channel row's pin, mute and archive (`docs/ux/group-and-channel-privacy.md`
 * section 12: a channel's row in Chats behaves as a group's).
 *
 * They are the account's synced flags (ADR-0058), the same register a chat or
 * a group uses, so they reach the account's other devices the same way. The
 * register is keyed on bytes: a conversation's MLS group id (32 bytes) for a
 * chat or a group, the channel id (16 bytes) for a channel. The two lengths
 * differ, so a channel's flags can never be read as a group's. iOS and Android
 * key a channel the same way.
 */
import { hexToBytesExact } from "./channel-log-codec";

/** A channel id: 16 bytes, 32 hex characters. */
const CHANNEL_ID_HEX_LEN = 32;

/** The flags register key of a channel, as bytes; `null` for a string that
 *  is not a channel id. */
export function channelFlagsKey(channelIdHex: string): Uint8Array | null {
  return hexToBytesExact(channelIdHex, CHANNEL_ID_HEX_LEN);
}

/** The flags map key of a channel: its id in lower-case hex, the form the
 *  synced state writes every key in. */
export function channelFlagsKeyHex(channelIdHex: string): string {
  return channelIdHex.toLowerCase();
}

/** The three flags a row draws. */
export interface RowFlags {
  readonly pinned: boolean;
  readonly muted: boolean;
  readonly archived: boolean;
}

/** A channel's flags, read from the synced flags map; all `false` when the
 *  account never set any. */
export function channelFlagsOf(
  flags: Readonly<Record<string, { readonly pinned: boolean; readonly muted: boolean; readonly archived: boolean } | undefined>>,
  channelIdHex: string,
): RowFlags {
  const f = flags[channelFlagsKeyHex(channelIdHex)];
  return { pinned: f?.pinned === true, muted: f?.muted === true, archived: f?.archived === true };
}
