// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// Who is behind each remote tile of a link call, and what the tile is called
/// (``CallTileName``).
///
/// In a link call the joiners are strangers to each other, so each participant
/// seals its own ADR-0048 profile into the call's own MLS group. This keeps,
/// per SFU peer, what that peer announced and the person it was bound to, and
/// names the tile from them:
///
///   * a peer is bound to a person by its own sealed, signed announcement,
///     verified against the call group's roster (ADR-0078 section 2,
///     ``CallRelay``): the SFU knows no account;
///   * a sealed profile is filed in the profile store under the person who
///     sealed it, its account claim kept only when bound
///     (``CallTileName/boundProfileClaim(_:fromOwner:ownerAccountId:)``), and
///     its announced name names the peer only when that peer announced the
///     leaf that sealed it;
///   * otherwise the tile shows what was announced, as plain text.
///
/// Owned by ``MultiJoinerCall``, which decodes the envelope and resolves its
/// MLS sender. Everything this reads from outside comes through ``Ports``, so
/// the rule is tested without MLS, the SFU or the profile directory.
@MainActor
final class LinkCallTileNames {

    /// What the names are resolved through. The defaults know nobody and file
    /// nothing, so a tile is named from what its peer announced alone.
    struct Ports {
        /// What the one resolver calls a person (rename, profile name,
        /// verified `@username`), or `nil` when it has only its placeholder.
        var personName: @MainActor (_ person: MemberIdentity) -> String? = { _ in nil }
        /// The username the stored profile of a person claims, or `nil`.
        var storedUsername: @MainActor (_ person: MemberIdentity) -> String? = { _ in nil }
        /// File a member's profile under the person who sealed it: the entry
        /// point a profile received in a chat goes through.
        var fileProfile: @MainActor (_ profile: ProfileClaim, _ person: MemberIdentity) -> Void = { _, _ in }
        /// Fires when anything a resolved name depends on changed (the profile
        /// directory's `revision` in production).
        var changes: AnyPublisher<UInt64, Never>?

        /// The production ports: the app's one display-name resolver and its
        /// profile store.
        @MainActor
        static func live(directory: ProfileDirectory) -> Ports {
            // The account this call runs under, taken when the call's names
            // are set up: a profile a peer sealed is filed under it, and under
            // no account signed in while the call was still winding down.
            let callAccountIdB64 = directory.accountIdB64
            return Ports(
                personName: { directory.resolve(signatureKeyB64: $0).flatMap(CallTileName.personName) },
                storedUsername: { directory.storedProfile(signatureKeyB64: $0)?.username },
                fileProfile: { profile, person in
                    directory.acceptReceived(
                        profile, signatureKeyB64: person, accountIdB64: callAccountIdB64)
                },
                changes: directory.$revision.eraseToAnyPublisher()
            )
        }
    }

    private let ports: Ports
    /// Every remote peer on the roster: what a tile is named again from.
    private var peers: Set<String> = []
    /// SFU peer id -> the leaf key its verified announcement names
    /// (ADR-0078 section 2).
    private var memberKeys: [String: String] = [:]
    /// SFU peer id -> the display name that peer's sealed profile carries.
    private var announcedNames: [String: String] = [:]
    /// SFU peer id -> the username that peer's sealed profile claims.
    private var claimedUsernames: [String: String] = [:]
    /// SFU peer id -> the person whose sealed profile that peer relayed.
    private var persons: [String: MemberIdentity] = [:]
    /// SFU peer id -> the name that peer typed for this call
    /// (``CallNameChannel``); it outranks every other source.
    private var callNames: [String: String] = [:]
    private var subscription: AnyCancellable?

    init(ports: Ports) {
        self.ports = ports
    }

    /// The tile name of `peerId`, which is now on the roster.
    func name(peerId: String) -> String {
        peers.insert(peerId)
        return currentName(peerId)
    }

    /// `peerId`'s sealed announcement verified (ADR-0078 section 2): it is the
    /// person `person`, through the leaf `memberKeyHex`.
    func peerIdentified(_ peerId: String, person: MemberIdentity, memberKeyHex: String) {
        persons[peerId] = person
        memberKeys[peerId] = memberKeyHex
    }

    /// A member sealed its profile into the call. The announced name and
    /// username name `peerId` only when `peerId` announced the leaf that
    /// sealed it (a relayed copy of someone else's profile names nobody); the
    /// profile is filed under the person who sealed it, its account claim kept
    /// only when bound (``CallTileName/boundProfileClaim(_:fromOwner:ownerAccountId:)``).
    func memberProfile(
        from peerId: String,
        sender: MemberIdentity?,
        senderLeafKeyHex: String?,
        fromOwner: Bool,
        ownerAccountId: Data,
        profile: ProfileClaim
    ) {
        if let senderLeafKeyHex, memberKeys[peerId] == senderLeafKeyHex {
            announcedNames[peerId] = Self.nonBlank(profile.displayName)
            claimedUsernames[peerId] = Self.nonBlank(profile.username)
        }
        guard let sender else { return }
        let bound = CallTileName.boundProfileClaim(
            profile, fromOwner: fromOwner, ownerAccountId: ownerAccountId)
        // A username with no certificate can never verify, and the store keeps
        // the first profile it sees for a stamp: filing this one would shut out
        // the certified one that follows it. The tile shows the username as
        // plain text either way.
        if bound.username == nil || bound.usernameCertificate != nil {
            ports.fileProfile(bound, sender)
        }
    }

    /// `peerId` typed `name` for this call (`nil`: back to its default).
    func setCallName(_ peerId: String, _ name: String?) {
        if let name, !name.isEmpty {
            callNames[peerId] = name
        } else {
            callNames.removeValue(forKey: peerId)
        }
    }

    /// The peer left: a return under the same id starts unnamed again.
    func peerLeft(_ peerId: String) {
        peers.remove(peerId)
        callNames.removeValue(forKey: peerId)
        memberKeys.removeValue(forKey: peerId)
        announcedNames.removeValue(forKey: peerId)
        claimedUsernames.removeValue(forKey: peerId)
        persons.removeValue(forKey: peerId)
    }

    /// Re-run the naming rule for every remote tile on `ui`.
    func relabel(_ ui: CallUiState) {
        for peerId in peers {
            let id = SfuCallUi.participantId(forPeer: peerId)
            ui.setParticipantName(id, currentName(peerId))
            // ADR-0084 — the person the verified announcement bound the peer
            // to shows their photo on the tile.
            ui.setPersonKey(persons[peerId].flatMap(base64Decode), forParticipant: id)
        }
    }

    /// ``relabel(_:)`` whenever the ports' `changes` fire: a username
    /// verified, a contact renamed. Weak on both sides: the subscription must
    /// not keep a finished call's screen alive.
    func follow(ui: CallUiState) {
        subscription = ports.changes?
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self, weak ui] _ in
                guard let self, let ui else { return }
                Task { @MainActor in self.relabel(ui) }
            }
    }

    private func currentName(_ peerId: String) -> String {
        let person = persons[peerId]
        return CallTileName.name(
            callName: callNames[peerId],
            resolved: person.flatMap { ports.personName($0) },
            announcedName: announcedNames[peerId],
            claimedUsername: claimedUsernames[peerId] ?? person.flatMap { ports.storedUsername($0) }
        ) ?? ""
    }

    private static func nonBlank(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty
        else { return nil }
        return trimmed
    }
}
