// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The name on a remote call tile (`docs/ux/calls.md`, "Every tile carries the
/// person's name"). iOS mirror of `apps/web/src/calls/tile-name.ts`.
///
/// A tile always carries the person's name. One rule for both call kinds, in
/// this order:
///
///   1. what the one resolver (``resolveDisplayName(signatureKey:localRename:profile:verification:)``)
///      calls the person behind the tile: the viewer's rename, the person's
///      profile name, their verified `@username`. The resolver's neutral
///      placeholder is not a name and is skipped;
///   2. the display name the person announced inside the call (link calls),
///      for the moment before the profile store has filed it;
///   3. the username the person claims while it is not verified, as plain text
///      without `@`. The `@` form is shown only for a verified username
///      (ADR-0048); a bare word claims nothing a typed display name could not.
///
/// `nil` only while nothing has reached the device from that person. The
/// roster then carries an empty name and the tile draws the call screen's
/// unnamed state (``CallPeerName/tileLabel(displayName:isLocal:)``). That
/// state is meant to last moments: everyone has a name or a username to
/// announce, and a link call keeps the announcements that arrive before it
/// can read them (``EarlyRelays``).
///
/// Never a piece of an account id or of a key. The SFU hands every client an
/// `account_pseudonym` (the first characters of the account id); a tile
/// labelled with it reads like something the viewer ought to recognise and
/// names nobody, and it is what a person without a profile name was shown as
/// until 2026-10-02.
enum CallTileName {

    /// The name the sources give, or `nil` while the person is still unnamed.
    ///
    /// - Parameters:
    ///   - resolved: the one resolver's name for the person (``personName(_:)``).
    ///   - announcedName: the display name announced inside the call.
    ///   - claimedUsername: the username the person claims, verified or not,
    ///     without `@`.
    static func name(resolved: String?, announcedName: String?, claimedUsername: String?) -> String? {
        for candidate in [resolved, announcedName, claimedUsername] {
            guard let trimmed = candidate?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !trimmed.isEmpty
            else { continue }
            return trimmed
        }
        return nil
    }

    /// A resolved person's name for a tile: `nil` when the resolver had only
    /// its placeholder to offer.
    static func personName(_ resolved: ResolvedDisplayName) -> String? {
        resolved.isPlaceholder ? nil : resolved.name
    }

    /// Rules 1 and 3 for a person the profile store may hold: what the one
    /// resolver calls them, else the username their stored profile claims.
    /// What names a group-call tile, and a link-call tile before its member
    /// has announced itself.
    @MainActor
    static func storedName(signatureKey: Data, directory: ProfileDirectory) -> String? {
        name(
            resolved: personName(directory.resolve(signatureKey: signatureKey)),
            announcedName: nil,
            claimedUsername: directory.storedProfile(signatureKeyB64: base64Encode(signatureKey))?.username)
    }

    /// Whether the account a sealed profile claims is the account the SFU
    /// admitted the relaying peer as.
    ///
    /// A profile in a link call comes from a stranger, and a username
    /// certificate binds a username to an ACCOUNT ID, not to the key that sent
    /// the profile. The ADR-0021 peer tag is derived from the account id in
    /// the peer's call-admission token, so a profile is filed in the profile
    /// store, and its username may ever read as verified, only when the
    /// claimed account id gives that peer's tag. A participant therefore
    /// cannot present another account's id together with that account's
    /// certificate.
    static func profileClaimMatchesPeer(
        claimedAccountId: Data?,
        peerTag: String?,
        tagOf: (_ accountIdHex: String) -> String
    ) -> Bool {
        guard let claimedAccountId, claimedAccountId.count == 16,
              let peerTag, !peerTag.isEmpty
        else { return false }
        return tagOf(hexEncode(claimedAccountId)) == peerTag
    }
}

/// The relays that reached a joiner before its Welcome.
///
/// A member's sealed profile and the committer's Welcome come from different
/// senders, so nothing orders them: the profile can arrive first, when there
/// is no group to open it with. Dropped, that member's tile stayed unnamed
/// until someone else was admitted. Kept here, bounded, and handed back once
/// the group exists.
struct EarlyRelays<Relay> {
    /// How many relays a link call keeps while it cannot read them yet.
    static var limit: Int { 64 }

    private var held: [Relay] = []

    /// Keep one; past the limit the oldest is dropped.
    mutating func keep(_ relay: Relay) {
        held.append(relay)
        if held.count > Self.limit { held.removeFirst(held.count - Self.limit) }
    }

    /// Everything kept, oldest first; the queue is empty afterwards.
    mutating func drain() -> [Relay] {
        defer { held = [] }
        return held
    }
}
