// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The key a channel's photo is looked up under by the one avatar component
 * (UX `docs/ux/group-and-channel-privacy.md` section 4.5.5): `channel:` and
 * the channel id in hex, so it can never be taken for a group id (base64).
 */

const PREFIX = "channel:";

export function channelAvatarKey(channelIdHex: string): string {
  return `${PREFIX}${channelIdHex}`;
}

export function isChannelAvatarKey(key: string): boolean {
  return key.startsWith(PREFIX);
}

export function channelIdOfAvatarKey(key: string): string {
  return key.slice(PREFIX.length);
}
