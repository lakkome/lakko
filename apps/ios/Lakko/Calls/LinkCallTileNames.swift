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
///   * a peer this device already knows (ADR-0021 peer tag over the accepted
///     contacts and the stored profiles) is named before it announces itself;
///   * a sealed profile is filed in the profile store, and its peer bound to
///     the person who sealed it, only when the account id the profile claims
///     is the account the SFU admitted that peer as
///     (``CallTileName/profileClaimMatchesPeer(claimedAccountId:peerTag:tagOf:)``).
///     Filed, it is a profile like any received in a chat: a claimed username
///     is verified and the viewer's own name for a contact applies;
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
        /// ADR-0021 — the person (base64 account key) behind a peer tag, when
        /// this device already knows them.
        var knownPerson: @MainActor (_ peerTag: String) -> MemberIdentity? = { _ in nil }
        /// What the one resolver calls a person (rename, profile name,
        /// verified `@username`), or `nil` when it has only its placeholder.
        var personName: @MainActor (_ person: MemberIdentity) -> String? = { _ in nil }
        /// The username the stored profile of a person claims, or `nil`.
        var storedUsername: @MainActor (_ person: MemberIdentity) -> String? = { _ in nil }
        /// File a member's profile under the person who sealed it: the entry
        /// point a profile received in a chat goes through.
        var fileProfile: @MainActor (_ profile: ProfileClaim, _ person: MemberIdentity) -> Void = { _, _ in }
        /// The ADR-0021 peer tag of an account id (lowercase hex) in this call.
        var tagOf: (_ accountIdHex: String) -> String = { _ in "" }
        /// Fires when anything a resolved name depends on changed (the profile
        /// directory's `revision` in production).
        var changes: AnyPublisher<UInt64, Never>?

        /// The production ports: the per-call peer-tag map the group call
        /// uses, the app's one display-name resolver and its profile store.
        @MainActor
        static func live(callIdHex: String, directory: ProfileDirectory) -> Ports {
            let known = GroupCallNameResolver.live(callIdHex: callIdHex, directory: directory)
            return Ports(
                knownPerson: { known.person(peerTag: $0).map { base64Encode($0) } },
                personName: { directory.resolve(signatureKeyB64: $0).flatMap(CallTileName.personName) },
                storedUsername: { directory.storedProfile(signatureKeyB64: $0)?.username },
                fileProfile: { profile, person in
                    directory.acceptReceived(profile, signatureKeyB64: person)
                },
                tagOf: { sfuPeerTag(callId: callIdHex, accountId: $0) },
                changes: directory.$revision.eraseToAnyPublisher()
            )
        }
    }

    private let ports: Ports
    /// SFU peer id -> the `peer_tag` the SFU reported for it. Every peer on
    /// the roster has an entry; it is what a tile is named again from.
    private var peerTags: [String: String] = [:]
    /// SFU peer id -> the display name that peer's sealed profile carries.
    private var announcedNames: [String: String] = [:]
    /// SFU peer id -> the username that peer's sealed profile claims.
    private var claimedUsernames: [String: String] = [:]
    /// SFU peer id -> the person whose sealed profile that peer relayed.
    private var persons: [String: MemberIdentity] = [:]
    private var subscription: AnyCancellable?

    init(ports: Ports) {
        self.ports = ports
    }

    /// The tile name for a roster event; empty while the person is unnamed.
    func name(peerId: String, peerTag: String) -> String {
        peerTags[peerId] = peerTag
        return currentName(peerId)
    }

    /// A member announced its profile inside the call. `sender` is the
    /// MLS-authenticated sender resolved to the person at receipt (C-68).
    func memberProfile(from peerId: String, sender: MemberIdentity?, profile: ProfileClaim) {
        announcedNames[peerId] = Self.nonBlank(profile.displayName)
        claimedUsernames[peerId] = Self.nonBlank(profile.username)
        if let sender,
           CallTileName.profileClaimMatchesPeer(
               claimedAccountId: profile.accountId, peerTag: peerTags[peerId], tagOf: ports.tagOf) {
            persons[peerId] = sender
            // A username with no certificate can never verify, and the store
            // keeps the first profile it sees for a stamp: filing this one
            // would shut out the certified one that follows it. The tile shows
            // the username as plain text either way.
            if profile.username == nil || profile.usernameCertificate != nil {
                ports.fileProfile(profile, sender)
            }
        } else {
            persons.removeValue(forKey: peerId)
        }
    }

    /// The peer left: a return under the same id starts unnamed again.
    func peerLeft(_ peerId: String) {
        peerTags.removeValue(forKey: peerId)
        announcedNames.removeValue(forKey: peerId)
        claimedUsernames.removeValue(forKey: peerId)
        persons.removeValue(forKey: peerId)
    }

    /// Re-run the naming rule for every remote tile on `ui`.
    func relabel(_ ui: CallUiState) {
        for peerId in peerTags.keys {
            ui.setParticipantName(SfuCallUi.participantId(forPeer: peerId), currentName(peerId))
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
        let person = persons[peerId] ?? peerTags[peerId].flatMap { ports.knownPerson($0) }
        return CallTileName.name(
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
