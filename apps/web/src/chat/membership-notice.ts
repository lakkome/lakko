// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which notice a membership action leaves under the members panel
 * (`docs/ux/proposal-based-membership.md` section 2, "What 'tap, done'
 * shows", and the maintainer's amendment of 2026-10-29).
 *
 * - Add, remove, make / remove admin and ownership transfer (the actions only
 *   an owner or an admin can take): no notice at all, whether the change is
 *   already in effect or still waiting for a committer. Until it lands the
 *   only sign is the member row's own annotation (UX section 5.2); once it
 *   lands the row changes in place and the timeline gets its named row.
 * - Leave: `chat.leave.requested` only when, after the attempt, this device
 *   is still a member; and it clears by itself the moment the departure
 *   lands.
 *
 * Whether a change is in effect is read from the GROUP after the attempt,
 * never from which code path committed it: a background sweep can commit the
 * request before the screen's own sweep looks, and the screen would then call
 * a done change "requested" (seen live 2026-10-29).
 *
 * Errors keep their own copy and are not decided here. Pure.
 */

/** A membership action this device just attempted. */
export type MembershipAction = "add" | "remove" | "set-admin" | "transfer-ownership" | "leave";

/** The catalog key of the notice to show after an attempt, or `null`. */
export function membershipNoticeAfterAttempt(
  action: MembershipAction,
  inEffect: boolean,
): "chat.leave.requested" | null {
  if (action !== "leave") return null;
  return inEffect ? null : "chat.leave.requested";
}

/**
 * Whether a notice still on screen for `action` should go, now that the group
 * reads `inEffect`. Only the leave notice is ever shown, and it goes once the
 * departure landed.
 */
export function membershipNoticeLanded(action: MembershipAction, inEffect: boolean): boolean {
  return action === "leave" && inEffect;
}
