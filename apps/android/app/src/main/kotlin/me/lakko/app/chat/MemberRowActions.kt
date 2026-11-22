// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * What one row of a group's member list shows (`docs/ux/conversation-info.md`
 * section 2.2, ADR-0022): the person's role mark, and which of `Remove` and
 * `Make admin` / `Remove admin` this reader gets on that row. Android twin of
 * `apps/web/src/chat/member-row-actions.ts` and iOS `MemberRowActions.swift`;
 * a change here changes all three.
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
public enum class MemberRole { OWNER, ADMIN, MEMBER }

/** `MAKE` offers `Make admin`, `TAKE` offers `Remove admin`. */
public enum class AdminToggle { MAKE, TAKE }

/** One member row's mark and controls. */
public data class MemberRowActions(
    /** `OWNER` or `ADMIN`, or null for no mark. */
    val mark: MemberRole?,
    val remove: Boolean,
    val adminToggle: AdminToggle?,
)

/** The one member-row rule; see the file comment. */
public fun memberRowActions(
    viewerRole: MemberRole?,
    rowRole: MemberRole,
    ownRow: Boolean,
    membershipEnded: Boolean,
): MemberRowActions {
    val hasRoles = viewerRole != null
    val mark = if (!hasRoles || membershipEnded || rowRole == MemberRole.MEMBER) null else rowRole
    if (ownRow || membershipEnded) return MemberRowActions(mark, remove = false, adminToggle = null)
    if (!hasRoles) return MemberRowActions(mark, remove = true, adminToggle = null)
    if (rowRole == MemberRole.OWNER) return MemberRowActions(mark, remove = false, adminToggle = null)
    return when (viewerRole) {
        MemberRole.OWNER -> MemberRowActions(
            mark,
            remove = true,
            adminToggle = if (rowRole == MemberRole.ADMIN) AdminToggle.TAKE else AdminToggle.MAKE,
        )
        MemberRole.ADMIN -> MemberRowActions(mark, remove = rowRole == MemberRole.MEMBER, adminToggle = null)
        else -> MemberRowActions(mark, remove = false, adminToggle = null)
    }
}

/** A row's role read from the role table (the person's account key). */
public fun rowRoleOf(personKey: ByteArray, owner: ByteArray?, admins: List<ByteArray>): MemberRole = when {
    owner != null && owner.contentEquals(personKey) -> MemberRole.OWNER
    admins.any { it.contentEquals(personKey) } -> MemberRole.ADMIN
    else -> MemberRole.MEMBER
}
