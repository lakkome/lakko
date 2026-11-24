// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The one Subscribe of a public channel found by its `@name` record (ADR-0087
 * sections 4 and 6), from a search result and a Discover card alike: the
 * record's read key opens the channel's public seeds, so it reads at once with
 * no waiting line (`docs/ux/group-and-channel-privacy.md` 7.2), titled with
 * the record's name and photo.
 */
import type { AvatarData } from "../profile/avatar-model";
import { subscribeToChannel } from "./channel-access";

export interface PublicChannelRecord {
  readonly idHex: string;
  readonly readKey: Uint8Array;
  readonly handle: string;
  readonly name: string;
  readonly description: string;
  readonly avatar: AvatarData | null;
}

export function subscribeToPublicChannel(found: PublicChannelRecord): Promise<"reading" | "waiting"> {
  return subscribeToChannel(found.idHex, { kind: "public", readKey: found.readKey }, {
    name: found.name,
    description: found.description,
    handle: found.handle,
    avatar: found.avatar,
  });
}
