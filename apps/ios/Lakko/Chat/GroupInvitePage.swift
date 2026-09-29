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
///      12-byte secret the whole capability is sealed under (and its preview,
///      under an independent derivation) rides the URL fragment (16 base64url
///      characters, ADR-0028 amendments of 2026-09-27 and 2026-09-28), which a
///      browser never sends and this app never uploads (the discovery ledger
///      and every access log stay ciphertext-only). ``GroupInviteLink``
///      appends the fragment on mint and reads it back on open; a link without
///      it opens nothing and is damaged.
///   2. **The headline.** What the card says for a given preview - or for
///      none. ``GroupInviteHeadline`` is a value the view renders line by line,
///      so the copy rules of the spec (section 3) are asserted once, in a test,
///      rather than re-derived inside SwiftUI where nothing can reach them.
public enum GroupInviteLink {
    /// The link secret is 12 bytes - shared by every invite kind since the
    /// sealed-links amendment of 2026-09-28 (``InviteLink``).
    public static var linkSecretLen: Int { InviteLink.secretLen }

    /// The link secret is exactly 16 characters in the URL.
    public static var linkSecretUrlLen: Int { InviteLink.secretUrlLen }

    /// What a link's fragment amounts to (see ``InviteLink/Fragment``). Since
    /// the capability is sealed under the secret, an absent fragment is as
    /// damaged as a malformed one.
    public typealias Fragment = InviteLink.Fragment

    /// Append the link secret to a `/g/` URL as its fragment. Any fragment the
    /// URL already carried is replaced, never doubled.
    public static func withLinkKey(_ url: String, linkKey: Data) -> String {
        InviteLink.withSecret(url, secret: linkKey)
    }

    /// Split a link into the part before `#` and the fragment after it.
    public static func split(_ link: String) -> (base: String, fragment: String?) {
        InviteLink.split(link)
    }

    /// Read a fragment: absent, a 12-byte secret, or malformed.
    public static func readFragment(_ fragment: String?) -> Fragment {
        InviteLink.readFragment(fragment)
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

    /// What the page says without a preview: the capability was minted
    /// without one (a channel co-publisher link). Nothing is invented and the
    /// page does not say why. (A link whose fragment is missing or does not
    /// open the sealed capability is not this state: it is refused as damaged,
    /// see ``groupInviteDamagedMessage``.)
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

/// The copy of a `/g/` (and `/i/`) link that is damaged: no fragment, a
/// fragment that is not a link secret, or a secret the sealed capability does
/// not open under (ADR-0028 amendment of 2026-09-28). The AEAD open is what
/// tells the joiner that the blob it was served is the one the link was made
/// for, so a link that does not open is refused, never shown without a card.
public let groupInviteDamagedMessage = "This invite link is damaged. Ask for a new link."

/// The copy of a `429` from the discovery ledger on an invite fetch or consume:
/// the link may be fine, the server is asking this network to slow down. Never
/// shown as "link invalid". Shared by the `/i/`, `/c/` and `/g/` landings.
public let inviteRateLimitedMessage =
    "Too many requests from this network right now. Try again in a minute."
