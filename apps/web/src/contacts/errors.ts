// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Error copy for the Contacts screens. Moved out of `routes/Contacts.tsx`
 * unchanged so the list, detail and invite panels share one formatter.
 */
import { anonTokenMessage } from "../tokens/token-copy";
import { ContactsError, SupersessionRefused } from "./client";
import { supersessionRefusedMessage } from "./supersession-message";

/** Render a typed username as an `@handle` for error copy; `undefined` for
 *  an empty input, so the formatter falls back to its neutral subject. */
export function atLabel(raw: string): string | undefined {
  const trimmed = raw.trim().replace(/^@+/, "");
  return trimmed.length > 0 ? `@${trimmed}` : undefined;
}

export function formatContactsError(err: unknown, peerLabel?: string): string {
  // ADR-0054 S3 — a lookup or a link that could not spend an anonymous token.
  const token = anonTokenMessage(
    err,
    "Could not reach Lakko. Check your connection and try again.",
  );
  if (token !== null) return token;
  // KT-log S4 — a fail-closed supersession refusal (an unproven CHANGED
  // key) is the moment the reset-vs-compromise distinction matters most.
  // Render a reset-aware message driven by the best-effort log marker the
  // client attached (`loggedReset`). The refusal itself is unchanged.
  if (err instanceof SupersessionRefused) {
    return supersessionRefusedMessage(err.loggedReset, peerLabel);
  }
  if (err instanceof ContactsError) {
    switch (err.variant.kind) {
      case "no-identity-secrets":
        return "Sign in again to continue.";
      case "username-unknown":
        return `No account is registered as @${err.variant.username}.`;
      case "no-identity-bundle":
        return "This person has not finished setting up Lakko yet. Try again later.";
      case "wrapped":
        return err.message;
    }
  }
  return err instanceof Error ? err.message : String(err);
}
