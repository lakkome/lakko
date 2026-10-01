// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The group invite page's pure rules (ADR-0028 v2, `docs/ux/group-invite-page.md`).
///
/// Two things live here, both kept out of SwiftUI so a unit test can drive
/// them:
///
///   1. **The link shape.** `https://lakko.me/g/<slug>#<link secret>` - the
///      12-byte secret the sealed preview's key is derived from rides the URL
///      fragment (16 base64url characters, ADR-0028 amendment of 2026-09-27),
///      which a browser never sends and this app never uploads (the discovery
///      ledger and every access log stay ciphertext-only). ``GroupInviteLink``
///      appends the fragment on mint and reads it back on open.
///   2. **The headline.** What the card says for a given preview - or for
///      none. ``GroupInviteHeadline`` is a value the view renders line by line,
///      so the copy rules of the spec (section 3) are asserted once, in a test,
///      rather than re-derived inside SwiftUI where nothing can reach them.
public enum GroupInviteLink {
    /// The link secret is 12 bytes - `LINK_SECRET_LEN` in `lakko-contacts`,
    /// read from the core.
    public static var linkSecretLen: Int { Int(groupInviteLinkSecretLen()) }

    /// The link secret is exactly 16 characters in the URL.
    public static var linkSecretUrlLen: Int { Int(groupInviteLinkSecretUrlLen()) }

    /// What a link's fragment amounts to.
    public enum Fragment: Equatable {
        /// No fragment: the link carries no preview secret (a chat app that
        /// strips fragments, a hand-typed URL). The page shows no preview.
        case absent
        /// A well-formed 12-byte link secret.
        case secret(Data)
        /// A fragment that is not exactly 16 base64url characters decoding to
        /// 12 bytes - the retired 43-character key included. The link is
        /// refused as damaged, fail-closed: nothing is fetched for it.
        case malformed
    }

    /// Append the link secret to a `/g/` URL as its fragment (base64url, no
    /// padding, 16 characters for 12 bytes). Any fragment the URL already
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

    /// Read a fragment: absent, a 12-byte secret, or malformed. Only exactly
    /// 16 base64url characters (no padding) decoding to 12 bytes is a secret.
    public static func readFragment(_ fragment: String?) -> Fragment {
        guard let fragment, !fragment.isEmpty else { return .absent }
        guard fragment.count == linkSecretUrlLen,
              isBase64Url(fragment),
              let bytes = base64UrlDecodeNoPad(fragment),
              bytes.count == linkSecretLen else { return .malformed }
        return .secret(bytes)
    }
}

/// Why a `/g/` link resolved to nothing: both read as "invalid or expired".
public enum GroupInviteLinkError: Error, Equatable {
    /// The slug is neither a 6-byte pointer nor an inline capability (the
    /// retired 43-character id included). Refused before any request.
    case malformed
    /// The ledger has no row for the id (never issued, revoked or expired).
    case notFound
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

    /// What the page says without a preview: the link had no fragment.
    /// Nothing is invented and the page does not say why. (A fragment under
    /// which the preview does not open is not this state: that link is refused
    /// as damaged, see ``groupInviteDamagedMessage``.) In the interface
    /// language of the moment it is read (ADR-0061).
    public static var noPreview: GroupInviteHeadline {
        GroupInviteHeadline(
            lead: Localization.text("invite.group.lead_anonymous_unnamed"),
            groupName: nil, members: nil)
    }

    /// Derive the headline. A blank name (after trimming) counts as missing,
    /// so a preview can never put an empty line on the card. One catalog
    /// sentence per shape (`invite.group.lead_*`), never a sentence built
    /// from pieces, so each language words it itself.
    public static func derive(from facts: GroupInviteFacts?) -> GroupInviteHeadline {
        guard let facts else { return noPreview }
        let inviter = nonBlank(facts.inviterName)
        let group = nonBlank(facts.groupName)
        let lead: String
        switch (inviter, group) {
        case let (name?, _?):
            lead = Localization.text("invite.group.lead_named", texts: ["name": name])
        case let (name?, nil):
            lead = Localization.text("invite.group.lead_named_unnamed", texts: ["name": name])
        case (nil, _?):
            lead = Localization.text("invite.group.lead_anonymous")
        case (nil, nil):
            lead = Localization.text("invite.group.lead_anonymous_unnamed")
        }
        let members = Localization.text(
            "chat.members.count", counts: ["count": UInt64(facts.memberCount)])
        return GroupInviteHeadline(lead: lead, groupName: group, members: members)
    }

    /// The foot of the card: the one thing on it the inviter cannot fake.
    public static func footer(fingerprint: String) -> String {
        Localization.text("invite.group.invited_by", texts: ["fingerprint": fingerprint])
    }

    private static func nonBlank(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines),
              !trimmed.isEmpty else { return nil }
        return trimmed
    }
}

/// The copy of the invalid / expired state (`invite.link.invalid`), in the
/// interface language of the moment it is read.
public var groupInviteInvalidMessage: String { Localization.text("invite.link.invalid") }

/// The copy of a `/g/` link whose fragment is present but whose preview does
/// not open under it (or whose fragment is not a link secret at all). Since the
/// ledger id stopped being a hash of the capability, the preview key - bound to
/// the capability's group id and issuer - is what tells the joiner that the
/// capability it was served is the one the link was made for, so a preview
/// that does not open refuses the link rather than showing a card without one.
public var groupInviteDamagedMessage: String { Localization.text("invite.link.damaged") }

/// The copy of a `429` from the discovery ledger on an invite fetch or consume:
/// the link may be fine, the server is asking this network to slow down. Never
/// shown as "link invalid". Shared by the `/i/`, `/c/` and `/g/` landings.
public var inviteRateLimitedMessage: String { Localization.text("invite.link.rate_limited") }
