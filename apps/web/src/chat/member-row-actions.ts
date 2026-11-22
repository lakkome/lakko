// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What one row of a group's member list shows (`docs/ux/conversation-info.md`
 * section 2.2, ADR-0022): the person's role mark, and which of `Remove` and
 * `Make admin` / `Remove admin` this reader gets on that row.
 *
 * The same table lives in iOS `MemberRowActions.swift` and Android
 * `MemberRowActions.kt`; a change here changes all three.
 *
 * - Nobody removes, promotes or demotes the owner, and nobody acts on their
 *   own row (`Leave group` is the way out).
 * - The owner removes anyone else and makes or takes back admins.
 * - An admin removes plain members only: ADR-0022 v1 lets an admin remove
 *   non-owner members but not another admin, and a role change is the owner's
 *   alone.
 * - A plain member acts on no row.
 * - A group with no role table (`viewerRole` null) is open: anyone removes
 *   anyone else, nobody has a role, there is no admin toggle.
 * - Once this reader's membership has ended the roster is read-only and no
 *   role is shown: a removal rewrites no role table, so a frozen roster could
 *   still name someone an admin who no longer is.
 *
 * Seen live 2026-11-22: an admin's iOS list offered `Remove` on the owner's
 * row, and no client marked the owner or the admins.
 */

/** A person's role in a group with a role table. */
export type GroupRole = "owner" | "admin" | "member";

/** The role mark after a member's name. */
export type MemberRoleMark = "owner" | "admin";

export interface MemberRowInput {
  /** This reader's role; `null` when the group has no role table. */
  readonly viewerRole: GroupRole | null;
  /** The row's person's role; ignored when `viewerRole` is `null`. */
  readonly rowRole: GroupRole;
  /** The row is this account. */
  readonly ownRow: boolean;
  /** This reader's membership has ended (left or removed). */
  readonly membershipEnded: boolean;
}

export interface MemberRowActions {
  readonly mark: MemberRoleMark | null;
  readonly remove: boolean;
  /** `make` offers `Make admin`, `take` offers `Remove admin`. */
  readonly adminToggle: "make" | "take" | null;
}

/** The one member-row rule; see the module comment. */
export function memberRowActions(input: MemberRowInput): MemberRowActions {
  const hasRoles = input.viewerRole !== null;
  const mark: MemberRoleMark | null =
    !hasRoles || input.membershipEnded || input.rowRole === "member" ? null : input.rowRole;
  if (input.ownRow || input.membershipEnded) return { mark, remove: false, adminToggle: null };
  if (!hasRoles) return { mark, remove: true, adminToggle: null };
  if (input.rowRole === "owner") return { mark, remove: false, adminToggle: null };
  switch (input.viewerRole) {
    case "owner":
      return { mark, remove: true, adminToggle: input.rowRole === "admin" ? "take" : "make" };
    case "admin":
      return { mark, remove: input.rowRole === "member", adminToggle: null };
    default:
      return { mark, remove: false, adminToggle: null };
  }
}

/** A row's role read from the role table (hex keys of the person's account key). */
export function rowRoleOf(
  personHex: string,
  ownerHex: string | null,
  adminHexes: ReadonlySet<string>,
): GroupRole {
  if (ownerHex !== null && personHex === ownerHex) return "owner";
  return adminHexes.has(personHex) ? "admin" : "member";
}
