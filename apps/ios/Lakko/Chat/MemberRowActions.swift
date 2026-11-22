// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// What one row of a group's member list shows (`docs/ux/conversation-info.md`
// section 2.2, ADR-0022): the person's role mark, and which of `Remove` and
// `Make admin` / `Remove admin` this reader gets on that row. iOS twin of
// `apps/web/src/chat/member-row-actions.ts` and Android `MemberRowActions.kt`;
// a change here changes all three.
//
// - Nobody removes, promotes or demotes the owner, and nobody acts on their
//   own row (`Leave group` is the way out).
// - The owner removes anyone else and makes or takes back admins.
// - An admin removes plain members only: ADR-0022 v1 lets an admin remove
//   non-owner members but not another admin, and a role change is the
//   owner's alone.
// - A plain member acts on no row.
// - A group with no role table (`viewerRole` nil) is open: anyone removes
//   anyone else, nobody has a role, there is no admin toggle.
// - Once this reader's membership has ended the roster is read-only and no
//   role is shown: a removal rewrites no role table, so a frozen roster could
//   still name someone an admin who no longer is.
//
// Seen live 2026-11-22: an admin's iOS list offered `Remove` on the owner's
// row, and no client marked the owner or the admins.

/// A person's role in a group with a role table.
public enum MemberRole: Equatable, CaseIterable, Sendable {
    case owner, admin, member

    /// The local member's role as the core reports it.
    public init(_ role: Role) {
        switch role {
        case .owner: self = .owner
        case .admin: self = .admin
        case .member: self = .member
        }
    }

    /// A row's role read from the role table (the person's account key).
    public static func of(person: Data, owner: Data?, admins: [Data]) -> MemberRole {
        if let owner, owner == person { return .owner }
        return admins.contains(person) ? .admin : .member
    }
}

/// One member row's mark and controls.
public struct MemberRowActions: Equatable, Sendable {
    /// `make` offers `Make admin`, `take` offers `Remove admin`.
    public enum AdminToggle: Equatable, Sendable { case make, take }

    /// `.owner` or `.admin`, or nil for no mark.
    public let mark: MemberRole?
    public let remove: Bool
    public let adminToggle: AdminToggle?

    /// The one member-row rule; see the file comment.
    public static func of(
        viewerRole: MemberRole?,
        rowRole: MemberRole,
        ownRow: Bool,
        membershipEnded: Bool
    ) -> MemberRowActions {
        let hasRoles = viewerRole != nil
        let mark: MemberRole? = (!hasRoles || membershipEnded || rowRole == .member) ? nil : rowRole
        if ownRow || membershipEnded { return .init(mark: mark, remove: false, adminToggle: nil) }
        guard let viewerRole else { return .init(mark: mark, remove: true, adminToggle: nil) }
        if rowRole == .owner { return .init(mark: mark, remove: false, adminToggle: nil) }
        switch viewerRole {
        case .owner:
            return .init(mark: mark, remove: true, adminToggle: rowRole == .admin ? .take : .make)
        case .admin:
            return .init(mark: mark, remove: rowRole == .member, adminToggle: nil)
        case .member:
            return .init(mark: mark, remove: false, adminToggle: nil)
        }
    }
}
