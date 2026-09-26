// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The group invite page's pure rules (ADR-0028 v2, `docs/ux/group-invite-page.md`).
///
/// Two things live here, both FFI-free so a unit test can drive them without
/// the core framework:
///
///   1. **The link shape.** `https://lakko.me/g/<slug>#<link key>` - the
///      sealed preview's key rides the URL fragment, which a browser never
///      sends and this app never uploads (the discovery ledger and every access
///      log stay ciphertext-only). ``GroupInviteLink`` appends the fragment on
///      mint and splits it back out on open.
///   2. **The headline.** What the card says for a given preview - or for
///      none. ``GroupInviteHeadline`` is a value the view renders line by line,
///      so the copy rules of the spec (section 3) are asserted once, in a test,
///      rather than re-derived inside SwiftUI where nothing can reach them.
public enum GroupInviteLink {
    /// The link key is 32 bytes - `LINK_KEY_LEN` in `lakko-contacts`.
    public static let linkKeyLen: Int = 32

    /// Append the link key to a `/g/` URL as its fragment (base64url, no
    /// padding, 43 characters for 32 bytes). Any fragment the URL already
    /// carried is replaced, never doubled.
    public static func withLinkKey(_ url: String, linkKey: Data) -> String {
        let base = split(url).base
        return base + "#" + base64UrlEncodeNoPad(linkKey)
    }

    /// Split a link into the part before `#` and the fragment after it (`nil`
    /// when there is none). The slug parsers must only ever see `base`.
    public static func split(_ link: String) -> (base: String, fragment: String?) {
        guard let hash = link.firstIndex(of: "#") else { return (link, nil) }
        let fragment = String(link[link.index(after: hash)...])
        return (String(link[..<hash]), fragment.isEmpty ? nil : fragment)
    }

    /// Decode a fragment into a link key: base64url (padded or not), exactly
    /// 32 bytes. Anything else is `nil` - the page then shows no preview,
    /// which is the spec's answer to a lost or mangled fragment, never an
    /// error.
    public static func linkKey(fromFragment fragment: String?) -> Data? {
        guard let fragment, !fragment.isEmpty,
              let bytes = base64UrlDecodeNoPad(fragment),
              bytes.count == linkKeyLen else { return nil }
        return bytes
    }
}

/// The three facts a preview carries, decoupled from the FFI record so the
/// headline rule can be tested without `LakkoCore`.
public struct GroupInviteFacts: Equatable {
    public var groupName: String?
    public var inviterName: String?
    public var memberCount: UInt16

    public init(groupName: String?, inviterName: String?, memberCount: UInt16) {
        self.groupName = groupName
        self.inviterName = inviterName
        self.memberCount = memberCount
    }
}

/// The card's headline, line by line (spec section 3).
///
/// - ``lead``: `<Inviter> invites you to` / `You are invited to`, with
///   ` a group` folded onto the same line when there is no group name.
/// - ``groupName``: the second line, present only when the preview names the
///   group.
/// - ``members``: `1 member` / `N members`, present only with a preview.
public struct GroupInviteHeadline: Equatable {
    public let lead: String
    public let groupName: String?
    public let members: String?

    /// What the page says without a preview: the fragment was absent, the
    /// preview did not open, or the link was minted without one. Nothing is
    /// invented and the page does not say why.
    public static let noPreview = GroupInviteHeadline(
        lead: "You are invited to a group", groupName: nil, members: nil)

    /// Derive the headline. A blank name (after trimming) counts as missing,
    /// so a preview can never put an empty line on the card.
    public static func derive(from facts: GroupInviteFacts?) -> GroupInviteHeadline {
        guard let facts else { return noPreview }
        let inviter = nonBlank(facts.inviterName)
        let group = nonBlank(facts.groupName)
        let subject = inviter.map { "\($0) invites you to" } ?? "You are invited to"
        let lead = group == nil ? subject + " a group" : subject
        let members = facts.memberCount == 1 ? "1 member" : "\(facts.memberCount) members"
        return GroupInviteHeadline(lead: lead, groupName: group, members: members)
    }

    /// The foot of the card: the one thing on it the inviter cannot fake.
    public static func footer(fingerprint: String) -> String {
        "Invited by " + fingerprint
    }

    private static func nonBlank(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines),
              !trimmed.isEmpty else { return nil }
        return trimmed
    }
}

/// The copy of the invalid / expired state, verbatim from the spec.
public let groupInviteInvalidMessage = "This invite link is invalid or has expired."
