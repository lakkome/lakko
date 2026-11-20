// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Channel cards (docs/ux/feed.md section 6): what one suggestion is, how the
 * three sources become cards, how this device orders them, and the check a
 * card must pass before it can be subscribed to.
 *
 * **Ranking for the person happens here, not in the ADR-0063 engine.** The
 * engine ranks messages of conversations the device already holds (its
 * candidates carry a conversation, an author, reactions and a time); a card
 * is none of those, so it cannot be handed to `rankFeed` without inventing
 * fields. {@link scoreCard} is a small local scorer over what a card does
 * carry: whether its topic is one the person picked, its language, the
 * people in the person's groups who follow it, the banded subscriber count
 * and the server's public order. Integers only, so it orders the same on
 * every run. The weights are chosen, not measured.
 *
 * Pure: the wasm decoder is injected.
 */
import { t } from "../l10n";

/** The published subscriber bands, smallest first (ADR-0059 section 4). */
export const SUBSCRIBER_BANDS = ["<10", "10+", "100+", "1k+", "10k+", "100k+"] as const;

/** Where a card came from. */
export type CardSource = "bucket" | "search" | "group";

/** People in one group who follow a channel. */
export interface GroupFollowers {
  readonly groupIdB64: string;
  readonly groupTitle: string;
  /** Distinct members of that group sharing it. */
  readonly count: number;
  /** Display names of those among them who are the person's contacts. */
  readonly contactNames: readonly string[];
}

/** One suggestion. */
export interface DirectoryCard {
  /** 32-char lowercase hex. */
  readonly channelIdHex: string;
  /** The claimed handle. Never shown as a link until verified. */
  readonly handle: string;
  readonly name: string;
  /** `null` until loaded (a group-shared card fetches it when opened). */
  readonly description: string | null;
  /** `null` when not known (a group-shared card before it is opened). */
  readonly topic: number | null;
  readonly language: string | null;
  /** One of {@link SUBSCRIBER_BANDS}, or `null` when not known. */
  readonly band: string | null;
  readonly source: CardSource;
  /** Position in the server's list (bucket or search), 0 = first. */
  readonly serverRank: number;
  /** Set for a card that came from groups: the group with the most
   *  followers, and the total across groups. */
  readonly followers: GroupFollowers | null;
  /** ADR-0089 section 2 - the listing as the directory served it (base64),
   *  which a report on the card carries. */
  readonly listingB64?: string;
}

/** One item of a bucket or search response, as the server sends it. */
export interface DirectoryItemWire {
  readonly channel_id: string;
  readonly listing_b64: string;
  readonly subscribers: string;
}

/** A decoded listing, structurally (the wasm `ChannelListingRecord`). */
export interface DecodedListing {
  readonly channelIdHex: string;
  readonly handle: string;
  readonly name: string;
  readonly description: string;
  readonly topic: number;
  readonly language: string;
}

/**
 * Directory items to cards. An item whose listing does not decode, or names
 * another channel than the item's id, is dropped: the listing bytes are the
 * owner's signed-by-proxy statement, and a mismatch means the server served
 * something it should not have.
 */
export function cardsFromItems(
  items: readonly DirectoryItemWire[],
  source: "bucket" | "search",
  decode: (listingB64: string) => DecodedListing | null,
): DirectoryCard[] {
  const out: DirectoryCard[] = [];
  const seen = new Set<string>();
  items.forEach((item, i) => {
    const listing = decode(item.listing_b64);
    if (listing === null) return;
    const id = item.channel_id.toLowerCase();
    if (listing.channelIdHex !== id || seen.has(id)) return;
    seen.add(id);
    out.push({
      channelIdHex: id,
      handle: listing.handle,
      name: listing.name,
      description: listing.description,
      topic: listing.topic,
      language: listing.language,
      band: (SUBSCRIBER_BANDS as readonly string[]).includes(item.subscribers) ? item.subscribers : null,
      source,
      serverRank: i,
      followers: null,
      listingB64: item.listing_b64,
    });
  });
  return out;
}

/** What the scorer and the filter read about the person. */
export interface CardContext {
  readonly pickedTopics: ReadonlySet<number>;
  readonly language: string;
  readonly notInterested: ReadonlySet<string>;
  readonly hiddenTopics: ReadonlySet<number>;
  /** Channels this device already follows or publishes. */
  readonly following: ReadonlySet<string>;
  /** Cards found gone this session (subscribe refused). */
  readonly gone: ReadonlySet<string>;
}

/** Whether a card may be shown at all. */
export function cardVisible(card: DirectoryCard, ctx: CardContext): boolean {
  if (ctx.notInterested.has(card.channelIdHex)) return false;
  if (ctx.following.has(card.channelIdHex)) return false;
  if (ctx.gone.has(card.channelIdHex)) return false;
  if (card.topic !== null && ctx.hiddenTopics.has(card.topic)) return false;
  return true;
}

/** The device-side score (chosen weights). Higher is better. */
export function scoreCard(card: DirectoryCard, ctx: CardContext): number {
  let score = 0;
  if (card.topic !== null && ctx.pickedTopics.has(card.topic)) score += 100;
  if (card.language !== null && card.language === ctx.language) score += 50;
  if (card.followers !== null) score += 40 * Math.min(card.followers.count, 5);
  const band = card.band === null ? -1 : (SUBSCRIBER_BANDS as readonly string[]).indexOf(card.band);
  if (band >= 0) score += 10 * band;
  score += Math.max(0, 100 - card.serverRank);
  return score;
}

/**
 * Merge cards from every source (a channel seen twice becomes one card with
 * the richest fields and the best server rank), drop the hidden ones and
 * order the rest: score, then name, then id, so the order is total.
 */
export function rankCards(cards: readonly DirectoryCard[], ctx: CardContext): DirectoryCard[] {
  const merged = new Map<string, DirectoryCard>();
  for (const c of cards) {
    const held = merged.get(c.channelIdHex);
    if (held === undefined) {
      merged.set(c.channelIdHex, c);
      continue;
    }
    const withListing = held.description !== null ? held : c;
    merged.set(c.channelIdHex, {
      ...withListing,
      serverRank: Math.min(held.serverRank, c.serverRank),
      followers: held.followers ?? c.followers,
      source: held.followers !== null || c.followers !== null ? "group" : withListing.source,
    });
  }
  return [...merged.values()]
    .filter((c) => cardVisible(c, ctx))
    .map((c) => ({ c, s: scoreCard(c, ctx) }))
    .sort(
      (a, b) =>
        b.s - a.s ||
        (a.c.name < b.c.name ? -1 : a.c.name > b.c.name ? 1 : 0) ||
        (a.c.channelIdHex < b.c.channelIdHex ? -1 : 1),
    )
    .map((x) => x.c);
}

/** How this device stands to a card's channel (UX spec section 6.2). */
export type CardRelation = "own" | "following" | "none";

/** A channel this device can post to is its own (owner or co-publisher);
 *  one it only holds in its list is followed. */
export function cardRelation(
  channelIdHex: string,
  publishes: ReadonlySet<string>,
  inList: ReadonlySet<string>,
): CardRelation {
  const id = channelIdHex.toLowerCase();
  if (publishes.has(id)) return "own";
  if (inList.has(id)) return "following";
  return "none";
}

/** The reason line under a card (UX spec section 6). Names only contacts. */
export function reasonLine(card: DirectoryCard, topicName: (id: number) => string): string {
  const f = card.followers;
  if (f !== null) {
    const named = f.contactNames.slice(0, 1);
    if (named.length === 1) {
      const others = f.count - 1;
      const name = named[0] ?? "";
      if (others <= 0) return t("discover.reason.followed_by", { name, group: f.groupTitle });
      return t("discover.reason.followed_by_others", { count: others, name, group: f.groupTitle });
    }
    return t("discover.reason.followed_count", { count: f.count, group: f.groupTitle });
  }
  if (card.source === "search") return t("discover.reason.search");
  return t("discover.reason.popular", {
    topic: card.topic === null ? t("discover.topic.other") : topicName(card.topic),
  });
}

/**
 * Before subscribing from a card (ADR-0059 section 3): the card's handle,
 * resolved through the normal OPRF channel lookup, must name the card's
 * channel. Anything else (no such handle, another channel) is "gone".
 */
export function verifyDecision(cardChannelIdHex: string, resolvedHex: string | null): "ok" | "gone" {
  if (resolvedHex === null) return "gone";
  return resolvedHex.toLowerCase() === cardChannelIdHex.toLowerCase() ? "ok" : "gone";
}

/** How often a suggestion appears in a feed (the rule's `suggestions`). */
export type SuggestionRate = "never" | "rarely" | "sometimes";

/** One card after this many feed items (chosen). */
export const SUGGESTION_EVERY: Readonly<Record<Exclude<SuggestionRate, "never">, number>> = {
  rarely: 20,
  sometimes: 8,
};

/** A slot in a feed that mixes cards in. */
export type MixedSlot =
  | { readonly kind: "item"; readonly index: number }
  | { readonly kind: "card"; readonly index: number };

/**
 * Interleave up to `cardCount` cards into `itemCount` feed items at the
 * rule's rate: one card after every N items, never at the very top and never
 * after the last item (a feed that ends on a suggestion reads as an ad).
 */
export function mixSuggestions(itemCount: number, cardCount: number, rate: SuggestionRate): MixedSlot[] {
  const out: MixedSlot[] = [];
  const every = rate === "never" ? 0 : SUGGESTION_EVERY[rate];
  let card = 0;
  for (let i = 0; i < itemCount; i += 1) {
    out.push({ kind: "item", index: i });
    const placed = i + 1;
    if (every > 0 && card < cardCount && placed % every === 0 && placed < itemCount) {
      out.push({ kind: "card", index: card });
      card += 1;
    }
  }
  return out;
}
