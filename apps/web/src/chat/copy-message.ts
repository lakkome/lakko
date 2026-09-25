// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Copy, from `docs/ux/message-actions.md` section 3: "Puts the text on the
 * clipboard. Nothing leaves the device."
 *
 * The whole of what that action needs, which section 6 states as "Copy -
 * nothing. Clipboard only." No envelope field, no wire surface, no server.
 * It is here rather than inline in the component for one reason: it is the
 * place the platform is reached, and a component that reaches the platform
 * directly cannot be asked what it did without a browser.
 *
 * **No fallback is written.** The only one available to a page is a hidden
 * textarea plus `document.execCommand("copy")`, which is deprecated, which
 * nothing in this codebase uses, and which would be new machinery introduced
 * on the strength of a guess about which browsers need it. Where
 * `navigator.clipboard` is unreachable this reports that it did nothing rather
 * than pretending, and the caller can carry that when there is a surface to
 * carry it to. `navigator.clipboard` is secure-context-only, which the app is.
 */

/**
 * Put `text` on the system clipboard.
 *
 * `false` when there was no clipboard to write to, or the browser refused —
 * a denied permission and an unreachable API are the same outcome to a caller,
 * which is that the text is not on the clipboard.
 */
export async function copyMessageText(text: string): Promise<boolean> {
  const clipboard = globalThis.navigator?.clipboard;
  if (clipboard === undefined || typeof clipboard.writeText !== "function") return false;
  try {
    await clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
