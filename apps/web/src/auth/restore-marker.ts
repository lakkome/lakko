// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Remembers, for this browser session, that the account landed here through
 * a recovery route (three words, the full phrase, another device, a passkey
 * blob) rather than an ordinary sign-in. The chat list reads it to replace
 * the generic empty state with the honest one: a restored device holds the
 * identity, not the account's existing conversations (ADR-0049 section 5).
 */

const KEY = "lakko.restored-identity";

export type RestoredKind = "device" | "phrase";

export function markRestored(kind: RestoredKind): void {
  try {
    sessionStorage.setItem(KEY, kind);
  } catch {
    /* storage unavailable: the generic empty state shows instead */
  }
}

export function restoredKind(): RestoredKind | null {
  try {
    const v = sessionStorage.getItem(KEY);
    return v === "device" || v === "phrase" ? v : null;
  } catch {
    return null;
  }
}

export function clearRestored(): void {
  try {
    sessionStorage.removeItem(KEY);
  } catch {
    /* nothing to clear */
  }
}
