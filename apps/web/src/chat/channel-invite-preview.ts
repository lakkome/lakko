// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Channel invitations in chats (ADR-0087 section 14): what a message that is
 * a channel invitation reads as, in the timeline (`ChannelInviteCard.tsx`)
 * and on the Chats row's second line.
 *
 * A message composed as an invitation carries the card in its envelope
 * (`composeChannelInviteEnvelope(url, card)`); the card is kept with the
 * message in the log (`PersistedMessage.channelInviteB64`) and opened HERE,
 * on this device, with no network, as iOS and Android do. The card stays
 * readable after its link is used up, revoked or expired (seen live
 * 2026-11-13: an owner's used publisher invitation came back after a reload
 * as the raw `/g/` link, secret included).
 *
 * A message with no card (an older message, a link pasted as text) is opened
 * through the ledger, once per session. Whatever the outcome, neither the row
 * nor a bubble that is not a live link ever shows what follows `#`.
 */
import { createSignal } from "solid-js";

import { openChannelInviteCardWith, openChannelLink, type OpenedChannelInvite } from "../channels/channel-invites";
import { base64Decode } from "../auth/account-id";
import { channelTitle } from "../channels/channel-row";
import { t } from "../l10n";
import { channelsState } from "../state/channels-state";
import { wasmSync } from "../wasm";
import { groupLinkOf } from "./group-link";

/** What opening a `/g/` link through the ledger came to. */
export type LinkOpening =
  /** A channel's invitation. */
  | { readonly kind: "channel"; readonly invite: OpenedChannelInvite }
  /** The link opened and is a group's (the group flow): a live link. */
  | { readonly kind: "not-channel" }
  /** The ledger row is gone (used up, revoked, expired) or the link is
   *  damaged: nothing it says can be shown. */
  | { readonly kind: "dead" }
  /** The ledger did not answer. */
  | { readonly kind: "offline" };

/** Ledger openings, by link, for the session: a timeline redraw does not
 *  open the same link again. */
const opened = new Map<string, Promise<LinkOpening>>();

/** The links whose opening finished, with what they came to. Reactive, for
 *  the Chats row and the bubble's text. */
const [settled, setSettled] = createSignal<ReadonlyMap<string, LinkOpening>>(new Map());

function classify(err: unknown): LinkOpening {
  const code = (err as { readonly code?: unknown } | null)?.code;
  return code === "offline" ? { kind: "offline" } : { kind: "dead" };
}

/** Open a `/g/` link through the ledger, once per session. */
export function openLinkOnce(url: string): Promise<LinkOpening> {
  let p = opened.get(url);
  if (p === undefined) {
    p = openChannelLink(url).then(
      (o): LinkOpening => (o === null ? { kind: "not-channel" } : { kind: "channel", invite: o }),
      classify,
    );
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

/** {@link openLinkOnce}, as the channel invitation it is or `null`. */
export async function openOnce(url: string): Promise<OpenedChannelInvite | null> {
  const o = await openLinkOnce(url);
  return o.kind === "channel" ? o.invite : null;
}

/** What a link's ledger opening came to, or `undefined` while it has not
 *  finished (the call starts it). Reactive. */
export function linkOpening(url: string): LinkOpening | undefined {
  const m = settled();
  if (!m.has(url)) {
    void openLinkOnce(url);
    return undefined;
  }
  return m.get(url);
}

/** Cards opened on this device, by their base64. A card's bytes never
 *  change what it opens to. */
const openedCards = new Map<string, OpenedChannelInvite | null>();

/**
 * The invitation a stored card opens to, on this device and offline, or
 * `null` for a card that does not open (or is not a channel's) and while the
 * core is not loaded yet. Reactive on the core's load.
 */
export function openStoredCard(cardB64: string | undefined, ownOrigin: string): OpenedChannelInvite | null {
  if (cardB64 === undefined || cardB64.length === 0) return null;
  const known = openedCards.get(cardB64);
  if (known !== undefined) return known;
  const wasm = wasmSync();
  try {
    // A call that throws while the module is not initialised: the read runs
    // again when it is (`wasmSync` is reactive).
    wasm.maxChannelInviteBytes();
  } catch {
    return null;
  }
  let card: Uint8Array;
  try {
    card = base64Decode(cardB64);
  } catch {
    openedCards.set(cardB64, null);
    return null;
  }
  const o = openChannelInviteCardWith(wasm, card, ownOrigin);
  openedCards.set(cardB64, o);
  return o;
}

/** A link without its secret: what follows `#` is never drawn. */
export function linkWithoutSecret(url: string): string {
  const at = url.indexOf("#");
  return at < 0 ? url : url.slice(0, at);
}

/** The invitation's line: "Invitation to {channel}" or "Invitation to
 *  publish in {channel}". */
function invitationLine(o: OpenedChannelInvite): string {
  return t(o.publisherInvite ? "chats.preview.publisher_invite" : "chats.preview.channel_invite", {
    channel: cardTitle(o),
  });
}

/**
 * The Chats row's second line for a message that is a channel invitation
 * (UX `docs/ux/group-and-channel-privacy.md` 14.1 and 15.2): "Invitation to
 * {channel}" or "Invitation to publish in {channel}", never the link.
 *
 * The stored card decides first, offline. Without one, a text that is one
 * `/g/` link of this deployment is opened through the ledger; until that
 * finishes, and when it does not open (dead, offline) or opens to a group,
 * the line is the link WITHOUT its secret, or the catalog's "invalid link"
 * sentence for a link that is gone. The row, and a desktop notification
 * taken from it, never carry what follows `#`. `null` for any other text.
 * Reactive.
 */
export function channelInviteRowPreview(
  text: string | null,
  ownOrigin: string,
  cardB64?: string,
): string | null {
  const card = openStoredCard(cardB64, ownOrigin);
  if (card !== null) return invitationLine(card);
  if (text === null) return null;
  const link = groupLinkOf(text, ownOrigin);
  if (link === null) return null;
  const o = linkOpening(link.url);
  if (o === undefined) return linkWithoutSecret(link.url);
  switch (o.kind) {
    case "channel":
      return invitationLine(o.invite);
    case "dead":
      return t("invite.link.invalid");
    default:
      return linkWithoutSecret(link.url);
  }
}

/** The card's title: the name the link carries, else the name this device
 *  knows for the channel, else the generic word. */
export function cardTitle(o: Pick<OpenedChannelInvite, "name" | "channelIdHex">): string {
  const carried = o.name.trim();
  if (carried.length > 0) return carried;
  const c = channelsState.channels().find((x) => x.id === o.channelIdHex);
  return channelTitle(channelsState.metaByChannel().get(o.channelIdHex)?.name, c?.localName ?? "", c?.localHandle);
}
