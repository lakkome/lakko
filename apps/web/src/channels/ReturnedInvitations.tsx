// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0101 section 7 - the channel invitations a return carried whose old
 * card binding did not verify (`returned-invitations.ts`), drawn above the
 * chat list as the ordinary invitation card (`chat/ChannelInviteCard.tsx`):
 * Subscribe, or Accept for a publisher invitation, by the person. An
 * invitation leaves once the account reads its channel.
 */
import { createEffect, createResource, For, type JSX } from "solid-js";

import { base64Decode } from "../auth/account-id";
import { ChannelInviteCard } from "../chat/ChannelInviteCard";
import { publicOrigin } from "../platform";
import { channelsState } from "../state/channels-state";
import { loadWasm } from "../wasm";
import { openChannelInviteCardWith, type OpenedChannelInvite } from "./channel-invites";
import {
  dropReturnedInvitations,
  invitationsToShow,
  readReturnedInvitations,
  returnedInvitationsVersion,
} from "./returned-invitations";

export function ReturnedInvitations(props: { readonly accountId: string | null }): JSX.Element {
  const source = (): { account: string; reading: Set<string>; held: Record<string, string> } | null => {
    returnedInvitationsVersion();
    const account = props.accountId;
    if (account === null) return null;
    return {
      account,
      reading: new Set(channelsState.channels().map((c) => c.id)),
      held: readReturnedInvitations(account),
    };
  };
  // An invitation whose channel the account reads now (subscribed through
  // it, or another return) leaves the list.
  createEffect(() => {
    const s = source();
    if (s === null) return;
    const read = Object.keys(s.held).filter((id) => s.reading.has(id));
    if (read.length > 0) dropReturnedInvitations(s.account, read);
  });
  const [cards] = createResource(source, async (s): Promise<OpenedChannelInvite[]> => {
    const wasm = await loadWasm();
    const out: OpenedChannelInvite[] = [];
    for (const i of invitationsToShow(s.held, s.reading)) {
      let card: Uint8Array;
      try {
        card = base64Decode(i.cardB64);
      } catch {
        continue;
      }
      const opened = openChannelInviteCardWith(wasm, card, publicOrigin());
      if (opened !== null && opened.channelIdHex === i.channelIdHex) out.push(opened);
    }
    return out;
  });
  return (
    <For each={cards() ?? []}>
      {(opened) => (
        <div class="lk-returned-invitation">
          <ChannelInviteCard opened={opened} />
        </div>
      )}
    </For>
  );
}
