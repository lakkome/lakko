// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Light client-side contact-shape validation (ADR-0018 factor (d)).
 *
 * A PERMISSIVE mirror of the server's `normalize_contact` gate
 * (`services/auth/src/recovery_contact.rs`): just enough of a check
 * to catch an obviously-malformed contact (e.g. an email with no
 * `@`, a phone with no leading `+`) before spending a limited attempt and
 * a round-trip. The server is authoritative — it does the full
 * E.164 / RFC-ish normalization and rejects with a 400 — so this
 * deliberately stays loose and never blocks a borderline value the
 * server might accept.
 */
import { ContactKind } from "./wire";

/**
 * Return `true` when `contact` is plausibly shaped for `kind`:
 *
 *   - **email** (`ContactKind.Email`) — contains exactly one `@`
 *     with a non-empty local part and domain, no whitespace.
 *   - **phone** (`ContactKind.Phone`) — after stripping the common
 *     human separators (spaces, dashes, parentheses), starts with
 *     `+` followed by at least one digit (digits only thereafter).
 *
 * Whitespace is trimmed first. Anything else returns `false` so the
 * form can surface a precise message instead of burning an attempt.
 */
export function isPlausibleContact(
  kind: ContactKind,
  contact: string,
): boolean {
  const trimmed = contact.trim();
  if (trimmed.length === 0) {
    return false;
  }
  if (kind === ContactKind.Email) {
    if (/\s/.test(trimmed)) {
      return false;
    }
    const at = trimmed.indexOf("@");
    return (
      at > 0 &&
      at === trimmed.lastIndexOf("@") &&
      at < trimmed.length - 1 &&
      trimmed.length <= 254
    );
  }
  // Phone: strip the separators the server also strips, then require
  // a leading '+' and 1..=15 digits.
  const stripped = trimmed.replace(/[\s\-() ]/g, "");
  if (!stripped.startsWith("+")) {
    return false;
  }
  const digits = stripped.slice(1);
  return digits.length >= 1 && digits.length <= 15 && /^[0-9]+$/.test(digits);
}
