// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The desktop keyboard shortcuts the page handles (`docs/ux/desktop.md`
 * section 5). `Mod` is `Cmd` on macOS and `Ctrl` elsewhere.
 *
 * `Mod+,`, `Mod+W` and `Mod+Q` come from the native menu (the host);
 * `Mod+1..9` already selects a chat folder tab (`chat/ChatList.tsx`) and is
 * left as it is, so the destinations are `Mod+Shift+1..5`. `Up` in an empty composer is the composer's own
 * (`routes/Chat.tsx`). What is here: which key press is which command, and
 * the hand-off of a command to the conversation list, which owns its search
 * field, its create menu and its order.
 */
import { createSignal } from "solid-js";

/** A command the conversation list carries out. */
export type ListCommand = "search" | "create" | "previous" | "next";

/** The five destinations, `Mod+Shift+1` .. `Mod+Shift+5` in this order. */
export type Destination = "chats" | "feed" | "contacts" | "calls" | "settings";

const DESTINATIONS: readonly Destination[] = ["chats", "feed", "contacts", "calls", "settings"];

/** What a key press asks for: a list command, or a destination. */
export type Shortcut = ListCommand | { readonly destination: Destination };

/** The parts of a key press the classification reads. */
export interface KeyPress {
  readonly key: string;
  /** The physical key (`Digit1`): Shift changes `key` on many layouts. */
  readonly code: string;
  readonly metaKey: boolean;
  readonly ctrlKey: boolean;
  readonly altKey: boolean;
  readonly shiftKey: boolean;
}

/** Where the key press landed. */
export interface KeyTarget {
  /** A text field, a text area, a select or an editable element has focus. */
  readonly editable: boolean;
  /** That field is the message composer. */
  readonly composer: boolean;
}

/** Whether this computer is a Mac (`Cmd` rather than `Ctrl`). */
export function isMacPlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
  const p = nav.userAgentData?.platform ?? nav.platform ?? "";
  return /mac/i.test(p);
}

/**
 * Which command a key press is, or `null`.
 *
 * `Mod+K`, `Mod+N` and `Mod+Shift+1..5` work from any field; `Alt+Up` /
 * `Alt+Down` not while a field other than the composer has focus, so a
 * person moving through a text never changes conversation. `Mod+1..9`
 * without Shift is the folder tabs' (`chat/ChatList.tsx`), not this.
 */
export function shortcutFor(e: KeyPress, mac: boolean, target: KeyTarget): Shortcut | null {
  const mod = mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  if (mod && e.shiftKey && !e.altKey) {
    const m = /^Digit([1-5])$/.exec(e.code);
    const destination = m === null ? undefined : DESTINATIONS[Number(m[1]) - 1];
    return destination === undefined ? null : { destination };
  }
  if (mod && !e.altKey && !e.shiftKey) {
    const k = e.key.toLowerCase();
    if (k === "k") return "search";
    if (k === "n") return "create";
    return null;
  }
  if (e.altKey && !e.metaKey && !e.ctrlKey && !e.shiftKey) {
    if (target.editable && !target.composer) return null;
    if (e.key === "ArrowUp") return "previous";
    if (e.key === "ArrowDown") return "next";
  }
  return null;
}

/**
 * The folder tab `Ctrl+1..9` / `Cmd+1..9` selects (0-based), or `null`. On
 * the web as on the desktop, and unchanged by the destinations: with Shift
 * (`Mod+Shift+1..5`) or Alt it is never a folder.
 */
export function folderTabIndexFor(e: KeyPress): number | null {
  if (!(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey) return null;
  if (e.key.length !== 1 || e.key < "1" || e.key > "9") return null;
  return Number(e.key) - 1;
}

/** Read the target of a key event into a {@link KeyTarget}. */
export function keyTargetOf(target: EventTarget | null): KeyTarget {
  const el = target as (Element & { isContentEditable?: boolean }) | null;
  if (el === null || typeof el.tagName !== "string") return { editable: false, composer: false };
  const tag = el.tagName.toLowerCase();
  const editable =
    tag === "input" || tag === "textarea" || tag === "select" || el.isContentEditable === true;
  const composer = editable && el.hasAttribute("data-lk-composer");
  return { editable, composer };
}

// ----- the hand-off to the conversation list ---------------------------------

const [pending, setPending] = createSignal<ListCommand | null>(null);
const [listsMounted, setListsMounted] = createSignal(0);

/** The command waiting for a conversation list, or `null`. */
export function pendingListCommand(): ListCommand | null {
  return pending();
}

/** Hand a command to the conversation list (now, or when it mounts). */
export function requestListCommand(cmd: ListCommand): void {
  setPending(cmd);
}

/** The list took the command. */
export function clearListCommand(): void {
  setPending(null);
}

/** Whether a conversation list is on screen. */
export function conversationListMounted(): boolean {
  return listsMounted() > 0;
}

/** A conversation list mounted; returns its unmount. */
export function noteConversationListMounted(): () => void {
  setListsMounted((n) => n + 1);
  return () => setListsMounted((n) => Math.max(0, n - 1));
}

/** The conversation `step` away from the open one in `hrefs` (the list
 *  order); with none open, the first (next) or the last (previous). */
export function neighbourHref(
  hrefs: readonly string[],
  openHref: string | null,
  cmd: "previous" | "next",
): string | null {
  if (hrefs.length === 0) return null;
  const at = openHref === null ? -1 : hrefs.indexOf(openHref);
  if (at === -1) return cmd === "next" ? (hrefs[0] ?? null) : (hrefs[hrefs.length - 1] ?? null);
  const to = cmd === "next" ? at + 1 : at - 1;
  return to < 0 || to >= hrefs.length ? null : (hrefs[to] ?? null);
}

/** The shortcut list Settings > About shows: the key text per platform. */
export function shortcutKeys(mac: boolean): {
  readonly mod: string;
  readonly alt: string;
} {
  return mac ? { mod: "Cmd", alt: "Option" } : { mod: "Ctrl", alt: "Alt" };
}
