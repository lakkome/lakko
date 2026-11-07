// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a channel's header and info say about it (UX
 * `docs/ux/group-and-channel-privacy.md` sections 4 and 12): its name and
 * description from the newest metadata record this device holds, then the
 * device's own fields; and the one line under the name.
 */
import { t } from "../l10n";
import { channelsState } from "../state/channels-state";
import { channelTitle } from "./channel-row";

export interface ChannelFacts {
  readonly name: string;
  readonly description: string;
}

/** The name and description: the shared record first, then the local fields. */
export function channelFacts(id: string): ChannelFacts {
  const h = channelsState.channels().find((c) => c.id === id);
  const shared = channelsState.metaByChannel().get(id);
  return {
    name: channelTitle(shared?.name, h?.localName ?? "", h?.localHandle),
    description: (shared?.description ?? h?.localDescription ?? "").trim(),
  };
}

const BANDS = ["<10", "10+", "100+", "1k+", "10k+", "100k+"] as const;

/** The ADR-0059 band of an exact count. */
export function subscriberBand(count: number): string {
  if (count >= 100_000) return BANDS[5];
  if (count >= 10_000) return BANDS[4];
  if (count >= 1_000) return BANDS[3];
  if (count >= 100) return BANDS[2];
  if (count >= 10) return BANDS[1];
  return BANDS[0];
}

/** `Private channel`, `Public channel`, or `Public channel · 10k+ subscribers`
 *  when this device holds a count (the owner and publishers). `null`
 *  visibility (not yet known) reads as nothing. */
export function channelHeaderLine(visibility: "public" | "private" | null, count: number | null): string {
  if (visibility === "private") return t("channels.header.private");
  if (visibility === "public") {
    return count === null ? t("channels.header.public") : t("channels.header.public_count", { band: subscriberBand(count) });
  }
  return "";
}
