// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// A call invitation sent as a message (`docs/ux/call-links.md` section 3).
///
/// `Invite a contact` sends the call's link into the conversation with that
/// person. The message is the link and nothing else, so every client can tell
/// it from ordinary text without a new wire field: a message whose whole text
/// is one Lakko call link is drawn as an invitation with a `Join` button,
/// never as a line of characters to copy.
///
/// The check is strict on purpose. Only a link to THIS deployment counts: the
/// `Join` button opens the call inside the app, and a link that merely looks
/// like one but names another host must stay plain text, not gain a button
/// that suggests it is ours.
///
/// Pure: the caller passes the app's own link origin
/// (``CallInviteUrl/defaultOrigin``, the origin this app mints call links
/// with). The twin of `apps/web/src/calls/call-invite-link.ts`.
enum CallInviteLink {

    /// Where `Join` goes.
    struct Target: Equatable {
        /// `/c/<slug>#<secret>`: the in-app path and fragment of the link.
        let href: String
        /// The whole link, as the app's `/c/` link handling takes it.
        let url: URL
    }

    private static let token: Set<Character> = Set(
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-")
    private static let slugMax = 512
    private static let secretMax = 128

    /// The invitation a message text is, or `nil` when it is ordinary text.
    ///
    /// `text` must be exactly one link (surrounding whitespace aside), on
    /// `ownOrigin`, of the shape `/c/<slug>#<secret>`: same scheme, host and
    /// port, no user info, no query, nothing else in the text.
    static func callInviteOf(_ text: String, ownOrigin: String) -> Target? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed.contains(where: { $0.isWhitespace }) { return nil }
        // The link is taken apart by hand rather than by a URL parser: a
        // parser normalises (percent-decoding, a dropped default port, a
        // trailing dot), and the rule here is about the characters sent.
        guard let own = origin(of: ownOrigin) else { return nil }
        guard let schemeEnd = trimmed.range(of: "://") else { return nil }
        let afterScheme = trimmed[schemeEnd.upperBound...]
        guard let pathStart = afterScheme.firstIndex(of: "/") else { return nil }
        let authority = afterScheme[..<pathStart]
        // User info, a query or a second fragment are refused below; the
        // origin must match character for character, case aside.
        if authority.contains("@") { return nil }
        let linkOrigin = (String(trimmed[..<schemeEnd.lowerBound]) + "://" + authority).lowercased()
        guard linkOrigin == own else { return nil }

        let rest = afterScheme[pathStart...]
        guard let hash = rest.firstIndex(of: "#") else { return nil }
        let path = rest[..<hash]
        let secret = rest[rest.index(after: hash)...]
        guard path.hasPrefix(CallInviteUrl.urlPrefix) else { return nil }
        let slug = path.dropFirst(CallInviteUrl.urlPrefix.count)
        guard isToken(slug, max: slugMax), isToken(secret, max: secretMax) else { return nil }

        let href = "\(CallInviteUrl.urlPrefix)\(slug)#\(secret)"
        guard let url = URL(string: trimmed) else { return nil }
        return Target(href: href, url: url)
    }

    /// The origin of `value` (`scheme://host[:port]`), lower-cased, or `nil`
    /// when it is not one.
    private static func origin(of value: String) -> String? {
        var text = value.trimmingCharacters(in: .whitespacesAndNewlines)
        while text.hasSuffix("/") { text.removeLast() }
        guard let schemeEnd = text.range(of: "://") else { return nil }
        let authority = text[schemeEnd.upperBound...]
        if authority.isEmpty || authority.contains(where: { "/@?#".contains($0) }) { return nil }
        return text.lowercased()
    }

    private static func isToken<S: StringProtocol>(_ value: S, max: Int) -> Bool {
        !value.isEmpty && value.count <= max && value.allSatisfy { token.contains($0) }
    }
}
