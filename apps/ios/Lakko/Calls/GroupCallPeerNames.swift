// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// Who is behind each remote tile of an SFU group call, and what it is called.
///
/// ADR-0078 section 2: the SFU knows no account and says nothing about who a
/// peer is. Each participant seals a signed announcement under the call's
/// relay key; ``SfuCallSession`` verifies it against the chat group's roster
/// (``CallPeerAnnouncer``) and reports the person it names. This keeps, per
/// roster participant, the person that peer was verified as, and asks the
/// tile-name rule (``CallTileName``) about that person whenever the tile is
/// drawn. Until a peer's announcement verifies its tile stays unnamed.
///
/// A profile can reach this device after the peer it names joined, and a
/// contact can be renamed mid-call, so the names follow the profile
/// directory's `revision` and every named tile is re-labelled when it moves.
/// Owned by the call handle; dropping the handle drops the subscription.
@MainActor
final class GroupCallPeerNames {
    private let resolve: (_ personKeyB64: String) -> String?
    private let changes: AnyPublisher<UInt64, Never>?
    /// Roster participant id -> the person its announcement verified as
    /// (`nil` while not yet verified). Every remote tile on the roster has an
    /// entry.
    private var persons: [String: String?] = [:]
    /// Roster participant id -> the name that peer typed for this call
    /// (``CallNameChannel``); it outranks every other source
    /// (``CallTileName``).
    private var callNames: [String: String] = [:]
    private weak var ui: CallUiState?
    private var subscription: AnyCancellable?

    init(
        name: @escaping (_ personKeyB64: String) -> String?,
        changes: AnyPublisher<UInt64, Never>? = nil
    ) {
        self.resolve = name
        self.changes = changes
    }

    /// The production names: the tile-name rule over the app's one
    /// display-name resolver (``CallTileName/storedName(signatureKey:directory:)``),
    /// following the profile directory on `ui`.
    static func live(ui: CallUiState, directory: ProfileDirectory = .shared) -> GroupCallPeerNames {
        let names = GroupCallPeerNames(
            name: { personKeyB64 in
                Data(base64Encoded: personKeyB64).flatMap {
                    CallTileName.storedName(signatureKey: $0, directory: directory)
                }
            },
            changes: directory.$revision.eraseToAnyPublisher()
        )
        names.follow(ui: ui)
        return names
    }

    /// The roster as the Welcome reported it: every entry starts unnamed.
    func setRoster(_ peerIds: [String]) {
        persons = [:]
        callNames = [:]
        for peerId in peerIds {
            persons[SfuCallUi.participantId(forPeer: peerId)] = .some(nil)
        }
    }

    /// A peer joined: its tile is unnamed until it announces itself.
    func peerJoined(_ peerId: String) {
        persons[SfuCallUi.participantId(forPeer: peerId)] = .some(nil)
    }

    /// A peer left: a return under the same id starts unnamed again.
    func peerLeft(_ peerId: String) {
        let id = SfuCallUi.participantId(forPeer: peerId)
        persons.removeValue(forKey: id)
        callNames.removeValue(forKey: id)
    }

    /// `peerId` typed `name` for this call (`nil`: back to its default).
    /// Returns the tile's new name, or `nil` for a peer not on the roster.
    func setCallName(_ peerId: String, _ name: String?) -> String? {
        let id = SfuCallUi.participantId(forPeer: peerId)
        guard persons[id] != nil else { return nil }
        if let name, !name.isEmpty {
            callNames[id] = name
        } else {
            callNames.removeValue(forKey: id)
        }
        return self.name(participantId: id)
    }

    /// `peerId`'s announcement verified as `personKeyB64`. Returns the tile's
    /// new name, or `nil` when nothing changed (an unknown peer, or the same
    /// person re-announcing).
    func identify(_ peerId: String, personKeyB64: String) -> String? {
        let id = SfuCallUi.participantId(forPeer: peerId)
        guard let current = persons[id] else { return nil }
        if current == personKeyB64 { return nil }
        persons[id] = .some(personKeyB64)
        return self.name(participantId: id)
    }

    /// The current name of a participant (empty: unnamed).
    func name(participantId: String) -> String {
        let person = persons[participantId] ?? nil
        return CallTileName.name(
            callName: callNames[participantId],
            resolved: person.flatMap { resolve($0) },
            announcedName: nil,
            claimedUsername: nil) ?? ""
    }

    /// Apply the current names to every remote tile on `ui`.
    func relabel(_ ui: CallUiState) {
        for id in persons.keys {
            ui.setParticipantName(id, name(participantId: id))
        }
    }

    private func follow(ui: CallUiState) {
        self.ui = ui
        subscription = changes?
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in
                Task { @MainActor in
                    guard let self, let ui = self.ui else { return }
                    self.relabel(ui)
                }
            }
    }
}
