// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Error copy for the Contacts screens, shared by the list, detail and invite
 * panels.
 *
 * ADR-0061 section 6: an error renders from its kind, never from its
 * `.message`. A handful of well-known failures (a used-up allowance, a
 * changed identity key, an unknown username, no network) have their own
 * catalog messages; a sentence already written for a screen (a
 * {@link KeyedError} or {@link RenderedError} anywhere in the wrap chain)
 * keeps its words; anything else is logged and shown as the action's own
 * `fallbackKey`. The result is how to render the text, so a message already
 * on screen follows a language switch.
 */
import { isDsNetworkFailure } from "../ds/client";
import { DiscoveryNetworkError, DiscoveryRateLimitedError } from "../discovery/client";
import { t } from "../l10n";
import { keyedErrorText } from "../l10n/keyed-error";
import { SEARCH_NOT_FOUND } from "../search/search-state";
import { anonTokenMessage } from "../tokens/token-copy";
import { ContactsError, SupersessionRefused } from "./client";
import { supersessionRefusedMessage } from "./supersession-message";

/** Render a typed username as an `@handle` for error copy; `undefined` for
 *  an empty input, so the formatter falls back to its neutral subject. */
export function atLabel(raw: string): string | undefined {
  const trimmed = raw.trim().replace(/^@+/, "");
  return trimmed.length > 0 ? `@${trimmed}` : undefined;
}

/** Walk the wrap chain (`cause`, or `variant.cause` for the contacts
 *  client's wrapper) for the first value `pick` recognises. */
function findInChain<T>(err: unknown, pick: (value: unknown) => T | null): T | null {
  let current: unknown = err;
  for (let depth = 0; depth < 8 && current !== null && current !== undefined; depth += 1) {
    const found = pick(current);
    if (found !== null) return found;
    if (typeof current !== "object") return null;
    const wrapped = current as { cause?: unknown; variant?: { cause?: unknown } };
    current = wrapped.variant?.cause ?? wrapped.cause;
  }
  return null;
}

/** How to render a Contacts failure in the current language; see the module
 *  comment. `peerLabel` names the person in the identity-change copy. */
export function contactsErrorText(
  err: unknown,
  fallbackKey: string,
  peerLabel?: string,
): () => string {
  // ADR-0054 S3 — a lookup or a link that could not spend an anonymous token.
  if (anonTokenMessage(err, "") !== null) {
    return () => anonTokenMessage(err, t("common.error.offline")) ?? t(fallbackKey);
  }
  const shown = findInChain(err, (v) => (keyedErrorText(v) !== null ? v : null));
  if (shown !== null) return () => keyedErrorText(shown) ?? t(fallbackKey);
  // KT-log S4 — a fail-closed supersession refusal (an unproven CHANGED
  // key) is the moment the reset-vs-compromise distinction matters most.
  const refused = findInChain(err, (v) => (v instanceof SupersessionRefused ? v : null));
  if (refused !== null) {
    const loggedReset = refused.loggedReset;
    return () => supersessionRefusedMessage(loggedReset, peerLabel);
  }
  if (err instanceof ContactsError) {
    const variant = err.variant;
    switch (variant.kind) {
      case "no-identity-secrets":
        return () => t("common.error.session_ended");
      case "username-unknown":
        // `docs/ux/who-can-find-me.md` section 6.1: the same answer whether
        // the account does not exist or does not let this person find it
        // (ADR-0060), so it never names the account as unregistered.
        return SEARCH_NOT_FOUND;
      case "no-identity-bundle":
        return () => t("chat.members.add.error.no_bundle");
      default:
        break;
    }
  }
  if (findInChain(err, (v) => (v instanceof DiscoveryRateLimitedError ? v : null)) !== null) {
    return () => t("common.error.too_many_attempts");
  }
  if (
    isDsNetworkFailure(err) ||
    findInChain(err, (v) => (v instanceof DiscoveryNetworkError ? v : null)) !== null
  ) {
    return () => t("common.error.offline");
  }
  console.warn("[contacts] action failed", err);
  return () => t(fallbackKey);
}

/** {@link contactsErrorText} rendered now, for a caller that shows it once. */
export function formatContactsError(err: unknown, fallbackKey: string, peerLabel?: string): string {
  return contactsErrorText(err, fallbackKey, peerLabel)();
}
