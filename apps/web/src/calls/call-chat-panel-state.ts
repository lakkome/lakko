// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The decisions of the group-call chat's screen (`docs/ux/calls.md` section
 * 4.1.4), kept out of the view so they can be tested.
 *
 * - The unread count on the `Chat` circle (`1` to `9`, then `9+`), counting
 *   only messages from others that arrived while the panel was closed.
 * - The 4-second one-line preview above the control bar: a second message
 *   replaces the first and restarts the time; opening the panel removes it.
 * - The `New messages` pill while the person has scrolled up.
 * - The circle's spoken name, the sender name above a run of bubbles, the
 *   panel's shape by width, and the composer's handling of the text bound.
 *
 * Pure: no Solid, no wasm, no DOM.
 */

/** How long the preview stays above the control bar. */
export const CHAT_PREVIEW_MS = 4_000;

/** From this width (px) the panel is a column at the end side of the call
 *  screen; below it, a panel over the whole call screen. */
export const CHAT_COLUMN_MIN_WIDTH = 900;

/** The column's width (px). */
export const CHAT_COLUMN_WIDTH = 360;

/** What a message needs to say for the panel's decisions. */
export interface ChatPanelMessage {
  readonly id: string;
  readonly own: boolean;
  readonly peerId: string | null;
  readonly text: string;
}

/** The one-line preview of a message from someone else. */
export interface ChatPreview {
  readonly messageId: string;
  readonly peerId: string;
  readonly text: string;
  readonly expiresAtMs: number;
}

/** The panel's state. */
export interface ChatPanelState {
  readonly open: boolean;
  /** Messages from others since the panel was last open. */
  readonly unread: number;
  readonly preview: ChatPreview | null;
  /** The list is scrolled to the newest message. */
  readonly atBottom: boolean;
  /** A message arrived while the person was scrolled up. */
  readonly newBelow: boolean;
}

export function initialChatPanel(): ChatPanelState {
  return { open: false, unread: 0, preview: null, atBottom: true, newBelow: false };
}

/** A message from someone else is now shown. This device's own messages
 *  change nothing here. */
export function panelIncoming(
  state: ChatPanelState,
  message: ChatPanelMessage,
  nowMs: number,
): ChatPanelState {
  if (message.own || message.peerId === null) return state;
  if (state.open) {
    return state.atBottom || state.newBelow ? state : { ...state, newBelow: true };
  }
  return {
    ...state,
    unread: state.unread + 1,
    preview: {
      messageId: message.id,
      peerId: message.peerId,
      text: message.text,
      expiresAtMs: nowMs + CHAT_PREVIEW_MS,
    },
  };
}

/** The panel opens, scrolled to the newest message: the count and the
 *  preview go. */
export function panelOpen(state: ChatPanelState): ChatPanelState {
  return { ...state, open: true, unread: 0, preview: null, atBottom: true, newBelow: false };
}

export function panelClose(state: ChatPanelState): ChatPanelState {
  if (!state.open) return state;
  return { ...state, open: false, newBelow: false, atBottom: true };
}

/** The person scrolled; reaching the newest message takes the pill away. */
export function panelScrolled(state: ChatPanelState, atBottom: boolean): ChatPanelState {
  if (state.atBottom === atBottom && (!atBottom || !state.newBelow)) return state;
  return { ...state, atBottom, newBelow: atBottom ? false : state.newBelow };
}

/** The person sent a message: the list goes to the newest. */
export function panelOwnSent(state: ChatPanelState): ChatPanelState {
  return { ...state, atBottom: true, newBelow: false };
}

/** The preview once its time is up. */
export function panelExpirePreview(state: ChatPanelState, nowMs: number): ChatPanelState {
  if (state.preview === null || nowMs < state.preview.expiresAtMs) return state;
  return { ...state, preview: null };
}

/** The preview to draw at `nowMs`, or `null`. */
export function previewAt(state: ChatPanelState, nowMs: number): ChatPreview | null {
  if (state.open || state.preview === null) return null;
  return nowMs < state.preview.expiresAtMs ? state.preview : null;
}

/** The badge on the `Chat` circle: `null` for none, `1`..`9`, then `9+`. */
export function unreadBadge(unread: number): string | null {
  if (unread <= 0) return null;
  return unread > 9 ? "9+" : String(unread);
}

/** The catalog key (and count) of the `Chat` circle's spoken name. */
export function chatControlName(
  open: boolean,
  unread: number,
): { readonly key: string; readonly count?: number } {
  if (open) return { key: "call.control.chat_hide_a11y" };
  if (unread > 0) return { key: "call.control.chat_unread_a11y", count: unread };
  return { key: "call.control.chat_show_a11y" };
}

/** Whether the bubble at `index` starts a run from another person, so the
 *  person's name goes above it. */
export function startsOthersRun(messages: readonly ChatPanelMessage[], index: number): boolean {
  const m = messages[index];
  if (m === undefined || m.own) return false;
  const prev = messages[index - 1];
  return prev === undefined || prev.own || prev.peerId !== m.peerId;
}

/** The panel's shape at a call-screen width. */
export function chatPanelShape(width: number): "sheet" | "column" {
  return width >= CHAT_COLUMN_MIN_WIDTH ? "column" : "sheet";
}

/** Whether the preview may be drawn now: never over the settings sheet, the
 *  participants list or a menu, and not while the control bar is faded. */
export function previewAllowed(input: {
  readonly barFaded: boolean;
  readonly settingsOpen: boolean;
  readonly participantsOpen: boolean;
  readonly menuOpen: boolean;
}): boolean {
  return !input.barFaded && !input.settingsOpen && !input.participantsOpen && !input.menuOpen;
}

/** The core's text rule, as the composer uses it (`checkCallChatText` /
 *  `clampCallChatText` from the wasm). */
export interface ChatTextRule {
  /** `undefined` when sendable, else `empty` / `blank` / `tooLong` /
   *  `controlCharacter`. */
  checkCallChatText(text: string): string | undefined;
  clampCallChatText(text: string): string;
}

/** What the field holds after an input. */
export interface ComposerText {
  /** The text to put back into the field. */
  readonly text: string;
  /** Show the too-long line. */
  readonly tooLong: boolean;
  /** The send button is enabled. */
  readonly sendable: boolean;
}

/**
 * The composer after the person typed or pasted `raw`. Line breaks are made
 * LF; the text is cut at the bound (the field takes no more), and the
 * too-long line shows from the moment something was cut until the text gets
 * shorter again.
 */
export function composerInput(raw: string, previous: ComposerText, rule: ChatTextRule): ComposerText {
  const normalised = raw.replace(/\r\n?/g, "\n");
  const text = rule.clampCallChatText(normalised);
  const cut = text !== normalised;
  const tooLong = cut || (previous.tooLong && text.length >= previous.text.length);
  return { text, tooLong, sendable: rule.checkCallChatText(text) === undefined };
}

/** The empty composer. */
export const EMPTY_COMPOSER: ComposerText = { text: "", tooLong: false, sendable: false };

/** What the Enter key does in the composer: on a hardware keyboard (a
 *  pointer that hovers) Enter sends and Shift+Enter starts a line; on a phone
 *  the return key starts a line and only the button sends. */
export function enterSends(input: {
  readonly key: string;
  readonly shiftKey: boolean;
  readonly isComposing: boolean;
  readonly touchKeyboard: boolean;
}): boolean {
  return (
    input.key === "Enter" && !input.shiftKey && !input.isComposing && !input.touchKeyboard
  );
}
