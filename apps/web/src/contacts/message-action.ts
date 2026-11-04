// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What Message does on a contact's card with no chat on this device.
 *
 * Right after an accept the chat is on its way: the Welcome arrives within
 * seconds, so the card says the chat is being set up, and Message waits
 * rather than founding a second conversation beside the one coming. After
 * that a contact with a known account can start a chat, and one without
 * cannot.
 */

/** How long a contact just accepted waits for its chat. Chosen, not measured:
 *  a Welcome that has not landed by then is not "in the seconds after". */
export const CHAT_STARTING_WINDOW_MS = 60_000;

export type MessageAction = "go" | "starting" | "unavailable";

export function messageAction(facts: {
  readonly hasChat: boolean;
  readonly canStartChat: boolean;
  /** When the contact was accepted; `null` when the caller has no such row. */
  readonly acceptedAtMs: number | null;
  readonly nowMs: number;
}): MessageAction {
  if (facts.hasChat) return "go";
  if (facts.acceptedAtMs !== null) {
    const age = facts.nowMs - facts.acceptedAtMs;
    if (age >= 0 && age < CHAT_STARTING_WINDOW_MS) return "starting";
  }
  return facts.canStartChat ? "go" : "unavailable";
}
