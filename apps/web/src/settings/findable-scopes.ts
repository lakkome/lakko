// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/who-can-find-me.md` section 4 - every group and channel the
 * account is in, as the "who can find me" list and the `Members can find me`
 * switch read them.
 *
 * A one-to-one conversation is not a group here: it is the same MLS object,
 * but the person already reaches the other participant as a contact, and the
 * list is about groups and channels. The split is the one the Chats and
 * Groups lists use (the kind the conversation carries, ADR-0062,
 * `chat/conversation-kind.ts`), so a conversation is a group here exactly
 * when it is listed under Groups.
 *
 * Must be called inside the providers (`useChats`,
 * `useGroupNames`, `useProfiles`).
 */
import { type Accessor } from "solid-js";

import { base64Decode } from "../auth/account-id";
import { channelTitle } from "../channels/channel-row";
import { useChats, type ActiveGroup } from "../chat/ChatContext";
import { conversationKindOf } from "../chat/conversation-kind";
import { groupTitle } from "../chat/group-title";
import { useGroupNames } from "../chat/GroupNameContext";
import { ownPersonKeyB64, personKeysOf } from "../chat/person-key";
import { useProfiles } from "../profile/ProfileContext";
import { channelsState } from "../state/channels-state";
import { channelIdBytes, scopeKey, type ScopeEntry } from "./who-can-find-me";

/** Reactive: the groups and channels the account is in. */
export function useFindableScopes(): Accessor<readonly ScopeEntry[]> {
  const chats = useChats();
  const groupNames = useGroupNames();
  const profiles = useProfiles();

  const isGroup = (g: ActiveGroup): boolean => conversationKindOf(g.group) === "group";

  const groupName = (g: ActiveGroup): string => {
    const own = ownPersonKeyB64(chats.identity());
    const peers = personKeysOf(g.group).filter((k) => k !== own);
    return groupTitle(
      groupNames.nameOf(g.groupIdB64) ?? g.label,
      peers.map((k) => profiles.resolve(k).name),
    );
  };

  return () => {
    const out: ScopeEntry[] = [];
    for (const g of chats.groups()) {
      if (!isGroup(g)) continue;
      let id: Uint8Array;
      try {
        id = base64Decode(g.groupIdB64);
      } catch {
        continue;
      }
      out.push({ kind: "group", id, key: scopeKey("group", id), name: groupName(g) });
    }
    const meta = channelsState.metaByChannel();
    for (const c of channelsState.channels()) {
      const id = channelIdBytes(c.id);
      if (id === null) continue;
      out.push({
        kind: "channel",
        id,
        key: scopeKey("channel", id),
        name: channelTitle(meta.get(c.id)?.name, c.localName, c.localHandle),
      });
    }
    return out;
  };
}
