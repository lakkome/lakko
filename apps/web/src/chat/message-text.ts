// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Links in a message's text (`docs/ux/message-links.md`). Which runs are links
 * is the core's rule; what a click does is decided here, from the address
 * alone, so the web and the desktop app cannot disagree.
 */
import { appPathOf, parseLakkoLink } from "../platform/lakko-link";

export interface TextPiece {
  readonly text: string;
  readonly isLink: boolean;
}

interface RawPiece {
  readonly text: string;
  readonly isLink: boolean;
  free(): void;
}

/** The text as pieces. With no rule loaded yet, one plain piece: a link that
 *  is not clickable for an instant loses nothing. */
export function messageTextPieces(
  text: string,
  split: ((text: string) => RawPiece[]) | null,
): readonly TextPiece[] {
  if (split === null) return text === "" ? [] : [{ text, isLink: false }];
  return split(text).map((raw) => {
    const piece = { text: raw.text, isLink: raw.isLink };
    raw.free();
    return piece;
  });
}

export type LinkOpen =
  | { readonly kind: "in-app"; readonly path: string }
  | { readonly kind: "system-browser" }
  | { readonly kind: "new-tab" };

/** Where a click on `url` goes. A link of this deployment opens its screen in
 *  place: leaving the app to come back would show the same screen. */
export function linkOpenOf(url: string, ownOrigin: string, desktop: boolean): LinkOpen {
  const own = parseLakkoLink(url, ownOrigin);
  if (own !== null) return { kind: "in-app", path: appPathOf(own) };
  return desktop ? { kind: "system-browser" } : { kind: "new-tab" };
}
