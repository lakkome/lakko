// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Channel invitations in chats (ADR-0087 section 14): the session's cache of
 * opened `/g/` links, shared by the invitation card in the timeline
 * (`ChannelInviteCard.tsx`) and the Chats row's second line, which reads the
 * invitation instead of the link.
 */
import { createSignal } from "solid-js";

import { openChannelLink, type OpenedChannelInvite } from "../channels/channel-invites";
import { channelTitle } from "../channels/channel-row";
import { t } from "../l10n";
import { channelsState } from "../state/channels-state";
import { groupLinkOf } from "./group-link";

/** Opened channel invitations, by link, for the session: a timeline redraw
 *  does not open the same link again. */
const opened = new Map<string, Promise<OpenedChannelInvite | null>>();

/** The links whose opening finished, with what they opened to (`null`: not
 *  a channel's, or did not open). Reactive, for the Chats row. */
const [settled, setSettled] = createSignal<ReadonlyMap<string, OpenedChannelInvite | null>>(new Map());

export function openOnce(url: string): Promise<OpenedChannelInvite | null> {
  let p = opened.get(url);
  if (p === undefined) {
    p = openChannelLink(url).catch(() => null);
    opened.set(url, p);
    void p.then((o) =>
      setSettled((m) => {
        const next = new Map(m);
        next.set(url, o);
        return next;
      }),
    );
  }
  return p;
}

/**
 * The Chats row's second line for a message that is a channel invitation
 * (UX `docs/ux/group-and-channel-privacy.md` 14.1 and 15.2): "Invitation to
 * {channel}" or "Invitation to publish in {channel}", never the link. `null`
 * when the text is not one `/g/` link of this deployment and when the link is
 * not a channel's. While it is being opened (the first call opens it) the
 * line is the link WITHOUT its secret: the row, and a desktop notification
 * taken from it at that moment, never carry what follows `#`. Reactive.
 */
export function channelInviteRowPreview(text: string | null, ownOrigin: string): string | null {
  if (text === null) return null;
  const link = groupLinkOf(text, ownOrigin);
  if (link === null) return null;
  const m = settled();
  if (!m.has(link.url)) {
    void openOnce(link.url);
    return link.url.split("#")[0] ?? null;
  }
  const o = m.get(link.url);
  if (o === null || o === undefined) return null;
  return t(o.publisherInvite ? "chats.preview.publisher_invite" : "chats.preview.channel_invite", {
    channel: cardTitle(o),
  });
}

/** The card's title: the name the link carries, else the name this device
 *  knows for the channel, else the generic word. */
export function cardTitle(o: Pick<OpenedChannelInvite, "name" | "channelIdHex">): string {
  const carried = o.name.trim();
  if (carried.length > 0) return carried;
  const c = channelsState.channels().find((x) => x.id === o.channelIdHex);
  return channelTitle(channelsState.metaByChannel().get(o.channelIdHex)?.name, c?.localName ?? "", c?.localHandle);
}

