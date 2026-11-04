// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/// Where the group-info screens go on a tab's stack
/// (`docs/ux/conversation-info.md` sections 3 and 3.1). A group's info is a
/// push over its conversation, the Add members screen a push over the info,
/// and a member's contact screen a push over the info; each back returns to
/// the screen it covered, which on a path-based stack is simply the route
/// below it. Pure, so the destinations run in a test.
enum GroupInfoNavigation {

    /// Open a group's info over its conversation. Already on top: nothing.
    static func openInfo(_ path: [TabRoute], groupIdHex: String) -> [TabRoute] {
        let info = TabRoute.groupInfo(groupIdHex: groupIdHex)
        return path.last == info ? path : path + [info]
    }

    /// Open the Add members screen over the info.
    static func openAddMembers(_ path: [TabRoute], groupIdHex: String) -> [TabRoute] {
        let add = TabRoute.addMembers(groupIdHex: groupIdHex)
        return path.last == add ? path : path + [add]
    }

    /// Add pressed: back to the info at once (the Add members screen and
    /// anything above it go; the info stays).
    static func afterAdd(_ path: [TabRoute], groupIdHex: String) -> [TabRoute] {
        dropping(.addMembers(groupIdHex: groupIdHex), from: path)
    }

    /// The chat was deleted from its info: the conversation is gone, so the
    /// info and the conversation under it both go.
    static func afterDelete(_ path: [TabRoute], groupIdHex: String) -> [TabRoute] {
        dropping(.chat(groupIdHex: groupIdHex), from: dropping(.groupInfo(groupIdHex: groupIdHex), from: path))
    }

    /// The last occurrence of `route` and everything above it removed; the
    /// path unchanged when it is not there.
    private static func dropping(_ route: TabRoute, from path: [TabRoute]) -> [TabRoute] {
        guard let index = path.lastIndex(of: route) else { return path }
        return Array(path[..<index])
    }
}
