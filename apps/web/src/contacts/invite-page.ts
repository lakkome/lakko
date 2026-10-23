// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The contact invite page's states (`docs/ux/contact-invite-link.md`
 * section 3, ADR-0079), as one pure decision so the page and its tests read
 * the same rule. The page shows who invites, by the name the link states; it
 * shows no fingerprint, no safety number, no expiry and no use count.
 *
 * | the visitor | the link                       | state          |
 * |-------------|--------------------------------|----------------|
 * | signed out  | any                            | `sign-in`      |
 * | signed in   | loading                        | `loading`      |
 * | signed in   | rate-limited                   | `rate-limited` |
 * | signed in   | does not open / expired / gone | `dead`         |
 * | signed in   | opens, made by this account    | `own`          |
 * | signed in   | opens, inviter a contact       | `already`      |
 * | signed in   | opens                          | `default`      |
 *
 * Amended 2026-10-19 (ADR-0079): a signed-out visitor sees no inviter card at
 * all, only the sign-in screen, and the link is checked again once signed in;
 * a link that does not work has no page: `dead` takes the visitor to the
 * Contacts screen. A damaged link (a missing or wrong secret) reads exactly
 * like one that no longer works.
 */

/** What the page shows. `name` is what the link states (display name, else
 *  `@username`), or `null` for the catalog's "this person". */
export type InvitePageView =
  | { readonly kind: "loading" }
  | { readonly kind: "rate-limited" }
  /** The link could not be fetched for another reason (the network, a server
   *  error): not a link that no longer works, and offered again. */
  | { readonly kind: "open-failed" }
  | { readonly kind: "dead" }
  | { readonly kind: "sign-in" }
  | { readonly kind: "own" }
  | { readonly kind: "already"; readonly name: string | null; readonly contactKeyB64: string }
  | {
      readonly kind: "default";
      readonly name: string | null;
      readonly username: string | null;
      readonly label: string | null;
    };

/** The name a link states for its owner: the display name, else
 *  `@username`, else `null`. Pure. */
export function inviterNameOf(
  displayName: string | null | undefined,
  username: string | null | undefined,
): string | null {
  const name = (displayName ?? "").trim();
  if (name.length > 0) return name;
  const user = (username ?? "").trim();
  return user.length > 0 ? `@${user}` : null;
}

export interface InvitePageInput {
  /** What opening the link gave. */
  readonly link: "loading" | "rate-limited" | "dead" | "opened";
  /** The capability's expiry has passed. */
  readonly expired: boolean;
  readonly inviterDisplayName: string | null;
  readonly inviterUsername: string | null;
  readonly label: string;
  readonly signedIn: boolean;
  /** The link's owner account is the visitor's own account. */
  readonly isOwn: boolean;
  /** The visitor's contact row for the inviter, when the inviter already is
   *  one. */
  readonly existingContactKeyB64: string | null;
}

/** The page's state for `input`. Pure. */
export function invitePageView(input: InvitePageInput): InvitePageView {
  if (!input.signedIn) return { kind: "sign-in" };
  if (input.link === "loading") return { kind: "loading" };
  if (input.link === "rate-limited") return { kind: "rate-limited" };
  if (input.link === "dead" || input.expired) return { kind: "dead" };
  const name = inviterNameOf(input.inviterDisplayName, input.inviterUsername);
  const username =
    input.inviterUsername !== null && input.inviterUsername.length > 0 ? input.inviterUsername : null;
  const label = input.label.trim().length > 0 ? input.label : null;
  if (input.isOwn) return { kind: "own" };
  if (input.existingContactKeyB64 !== null) {
    return { kind: "already", name, contactKeyB64: input.existingContactKeyB64 };
  }
  return { kind: "default", name, username, label };
}

/** What a failure to open an invite link means to the page. */
export type InviteOpenFailure = "rate-limited" | "dead" | "open-failed";

/**
 * Classify a failure to fetch or open an invite link. A rate limit says
 * nothing about the link and is offered again; a damaged link (missing or
 * wrong secret, malformed slug) is a link that does not work; anything else
 * (the network, a server error) says nothing about the link either and is
 * offered again, as on iOS and Android (`InviteOpenResult.Failed`). Until
 * 2026-10-23 the web read that last case as a dead link and went to Contacts
 * on its own. The predicates are passed in: `discovery/client.ts` imports the
 * link module, so this one does not import it back.
 */
export function inviteOpenFailure(
  err: unknown,
  is: { readonly rateLimited: (err: unknown) => boolean; readonly damaged: (err: unknown) => boolean },
): InviteOpenFailure {
  if (is.rateLimited(err)) return "rate-limited";
  if (is.damaged(err)) return "dead";
  return "open-failed";
}
