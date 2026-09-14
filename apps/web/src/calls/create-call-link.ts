// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Decisions behind the "Create call link" form (`CreateCallLinkPanel.tsx`).
 *
 * A `/c/` call link is independent of any chat: its owner hosts the call from
 * the `/c/<slug>` landing (`JoinCall.tsx`, ADR-0032 owner-founds) and anyone
 * holding the link asks to be admitted. The composer that mints and uploads
 * the capability (`composer.ts`) existed with no UI calling it.
 *
 * Kept out of the `.tsx` because vitest here runs in plain node with no Solid
 * transform — anything in the component cannot be tested.
 */
import {
  CALL_INVITE_LABEL_MAX_LEN,
  CALL_INVITE_URL_PREFIX,
  parseCallInviteSlug,
  type ExpiryPreset,
  type MaxUsesPreset,
} from "./invite";

/** The expiry choices the form offers, in display order. A call link is for a
 *  call happening soon; `30d` / `never` are deliberately not offered. */
export const CALL_LINK_EXPIRY_OPTIONS: readonly {
  readonly value: ExpiryPreset;
  readonly label: string;
}[] = [
  { value: "1h", label: "1 hour" },
  { value: "1d", label: "24 hours" },
  { value: "7d", label: "7 days" },
];

/** Default expiry. */
export const CALL_LINK_DEFAULT_EXPIRY: ExpiryPreset = "1h";

/** The max-uses choices the form offers, in display order. */
export const CALL_LINK_MAX_USES_OPTIONS: readonly {
  readonly value: MaxUsesPreset;
  readonly label: string;
}[] = [
  { value: "single", label: "1 person" },
  { value: "ten", label: "Up to 10 people" },
  { value: "unlimited", label: "Unlimited" },
];

/** Default max uses. */
export const CALL_LINK_DEFAULT_MAX_USES: MaxUsesPreset = "ten";

/** Form-level message for a label the composer would refuse, or `null`. The
 *  limit is in UTF-8 bytes, so a short string of multi-byte characters can
 *  still be too long — the character counter a `maxlength` gives is not it. */
export function callLinkLabelError(label: string): string | null {
  const bytes = new TextEncoder().encode(label).length;
  if (bytes <= CALL_INVITE_LABEL_MAX_LEN) return null;
  return `Label is too long (${bytes} of ${CALL_INVITE_LABEL_MAX_LEN} bytes).`;
}

/**
 * The in-app route for a minted link — `/c/<slug>` — so "Start call" can
 * navigate there rather than reloading the page. `null` when the URL is not a
 * call link this client can open (wrong prefix, or a slug that does not
 * decode as a pointer / inline capability).
 */
export function callLinkRoutePath(url: string): string | null {
  let pathname: string;
  try {
    pathname = new URL(url).pathname;
  } catch {
    return null;
  }
  if (!pathname.startsWith(CALL_INVITE_URL_PREFIX)) return null;
  const slug = pathname.slice(CALL_INVITE_URL_PREFIX.length);
  if (slug.length === 0 || slug.includes("/")) return null;
  try {
    parseCallInviteSlug(slug);
  } catch {
    return null;
  }
  return `${CALL_INVITE_URL_PREFIX}${slug}`;
}
