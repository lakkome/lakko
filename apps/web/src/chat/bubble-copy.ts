// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Kept apart from `./copy-message.ts`, which reaches the clipboard and which
 * component tests mock. Pure.
 *
 * The text Copy takes from a bubble's text: "the text I see" (UX 3.2). A
 * bubble that draws a channel invitation card shows no text, so there is
 * nothing to copy (its text is the `/g/` link with its secret, ADR-0087
 * section 14); a `/g/` link drawn as words without its secret copies those
 * words. `null` when there is nothing to copy.
 */
export function bubbleCopyText(shownText: string, cardDrawn: boolean, linkText: string | null): string | null {
  if (cardDrawn) return null;
  if (linkText !== null) return linkText;
  return shownText.length > 0 ? shownText : null;
}

/** Whether a bubble offers Edit (with Delete) given that it is drawn as a
 *  channel invitation card: never, since its text is the `/g/` link with its
 *  secret and Edit would put that link in the composer (ADR-0087 section
 *  14). Delete stays. */
export function bubbleOffersEdit(cardDrawn: boolean): boolean {
  return !cardDrawn;
}

/** Whether a bubble offers Forward given that it is drawn as a channel
 *  invitation card: never (maintainer, 2026-11-13: a message carrying a
 *  channel invitation card cannot be forwarded). */
export function bubbleOffersForward(cardDrawn: boolean): boolean {
  return !cardDrawn;
}
