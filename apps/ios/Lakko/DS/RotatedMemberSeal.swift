// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0074 section 3.7 (2026-11-20) — envelopes sealed to a member's account
/// after that member rotated its account keys.
///
/// The roster names a rotated person by its CONVERSATION key (the oldest own
/// predecessor: every leaf binding is under it), while the directory serves
/// the CURRENT key and the current identity's X25519 key, plus the two-way
/// conversation link signed by both keys.
///
/// Sender (the owner sealing a registrar seed, a root handover or a knock
/// seed): the envelope is ADDRESSED to the roster key and SEALED to the X25519
/// key of the bundle, and the bundle is accepted only by
/// ``bundleServesRosterKey(servedKey:conversationLink:rosterKey:verifyLink:)``.
/// Receiver: the address is checked against the conversation identity; the
/// envelope opens with the conversation identity's own X25519 key, else with
/// the current identity's
/// (``openSealedToOwnAccount(conversation:current:openOwn:openSealedTo:)``).
enum RotatedMemberSeal {
    /// Neither the conversation identity's X25519 key nor the current one's
    /// opened the envelope.
    struct NotOpened: Error {}

    /// Whether a fetched identity bundle is the bundle of the person the MLS
    /// roster names by `rosterKey`: its served key IS the roster key, or its
    /// conversation link verifies against the served key and names exactly
    /// the roster key as the conversation key. Anything else (no link, a link
    /// that does not verify, a link naming another key, a malformed key) is
    /// refused.
    ///
    /// `verifyLink` is the core's `verifyConversationLink(link:trustedCurrentKey:)`:
    /// it answers the 32-byte conversation key and throws on anything wrong.
    static func bundleServesRosterKey(
        servedKey: Data,
        conversationLink: Data?,
        rosterKey: Data,
        verifyLink: (_ link: Data, _ trustedCurrentKey: Data) throws -> Data
    ) -> Bool {
        guard servedKey.count == 32, rosterKey.count == 32 else { return false }
        if servedKey == rosterKey { return true }
        guard let conversationLink, !conversationLink.isEmpty,
              let conversationKey = try? verifyLink(conversationLink, servedKey)
        else { return false }
        return conversationKey == rosterKey
    }

    /// Open an envelope addressed to this account's conversation identity:
    /// first sealed to its own X25519 key (`openOwn`), then, when the account
    /// rotated and the directory serves `current`, sealed to the current
    /// identity's X25519 key (`openSealedTo` with the conversation identity as
    /// the addressee). `nil` when neither opens.
    static func openSealedToOwnAccount<I: AnyObject>(
        conversation: I,
        current: I?,
        openOwn: (_ addressee: I) throws -> Data,
        openSealedTo: (_ addressee: I, _ sealedTo: I) throws -> Data
    ) -> Data? {
        if let opened = try? openOwn(conversation) { return opened }
        guard let current, current !== conversation else { return nil }
        return try? openSealedTo(conversation, current)
    }
}
