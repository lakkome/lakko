// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// ADR-0021 — the per-call `peer_tag -> person` map behind a call roster.
///
/// The SFU names a participant by a per-call tag derived over the JWT-verified
/// account id; it never sends a name. The client re-derives the same tag over
/// every account id it knows and matches. Before this the only source of
/// account ids was the accepted-contacts list, so a group member who was not
/// a contact showed as a pseudonym on the call while the members panel — which
/// resolves by signature key — named them. Received profiles (ADR-0048,
/// envelope v2) now carry the sender's account id too, and both sources fold
/// into one map here.
///
/// The map answers WHO, never what they are called: the name is
/// ``CallTileName``'s, asked about the person when the tile is drawn.
///
/// Pure: the tag derivation is injected, so the merge and the collision rule
/// are tested without the FFI or the directory.
enum GroupCallPeerTags {
    /// One person the map can find: the lowercase-hex account id the tag is
    /// derived over and the signature key the name is resolved from.
    struct Person: Equatable {
        let accountIdHex: String
        let signatureKey: Data
    }

    /// The map plus the tags it must NOT answer for.
    struct Map: Equatable {
        /// `peer_tag -> signature key` for every distinct account id seen once.
        var people: [String: Data] = [:]
        /// Tags two DIFFERENT account ids derived to. A 64-bit tag can collide;
        /// naming either person would be a guess, so neither is answered.
        var collided: Set<String> = []

        /// The person behind what the SFU said about a peer, or `nil` when the
        /// tag is unknown or ambiguous. Never the pseudonym the SFU sent: that
        /// is a piece of the account id and names nobody.
        func person(peerTag: String) -> Data? {
            guard !peerTag.isEmpty, !collided.contains(peerTag) else { return nil }
            return people[peerTag]
        }
    }

    /// Fold both sources. Contacts are walked first: the same account reached
    /// through both is ONE person, not a collision, and the first source's key
    /// stands for it (the order only makes the outcome deterministic). Two
    /// DIFFERENT account ids meeting at one tag mark it collided, and a
    /// collided tag stays collided however many more arrive.
    static func build(
        contacts: [Person],
        profiles: [Person],
        tag: (_ accountIdHex: String) -> String
    ) -> Map {
        var map = Map()
        var seenAccounts = Set<String>()
        for person in contacts + profiles {
            let accountIdHex = person.accountIdHex.lowercased()
            guard !accountIdHex.isEmpty, seenAccounts.insert(accountIdHex).inserted else { continue }
            let peerTag = tag(accountIdHex)
            if map.people[peerTag] != nil {
                map.collided.insert(peerTag)
            } else {
                map.people[peerTag] = person.signatureKey
            }
        }
        return map
    }
}

/// The live resolver an SFU group call names its tiles through, and the
/// peer-tag lookup a link call asks who it already knows.
///
/// Not frozen at call start: the map is rebuilt from the current sources on
/// every roster event, and — because a profile can arrive AFTER the peer it
/// names joined (a newcomer asks the group to introduce itself and is answered
/// within about thirty seconds) — the resolver remembers the tag the SFU
/// reported for each participant and re-labels the live roster when the
/// profile directory changes. Owned by the call handle; dropping the handle
/// drops the subscription with it.
@MainActor
final class GroupCallNameResolver {
    private let callIdHex: String
    private let contacts: () -> [GroupCallPeerTags.Person]
    private let profiles: () -> [GroupCallPeerTags.Person]
    private let tag: (_ callIdHex: String, _ accountIdHex: String) -> String
    private let name: (_ signatureKey: Data) -> String?
    /// Roster participant id -> the tag the SFU reported for that peer. Kept
    /// for the re-label; a departed peer's entry is harmless (an unknown id is
    /// a no-op) and a rejoin under the same id overwrites it.
    private var seen: [String: String] = [:]
    private var subscription: AnyCancellable?

    init(
        callIdHex: String,
        contacts: @escaping () -> [GroupCallPeerTags.Person],
        profiles: @escaping () -> [GroupCallPeerTags.Person],
        tag: @escaping (_ callIdHex: String, _ accountIdHex: String) -> String,
        name: @escaping (_ signatureKey: Data) -> String?
    ) {
        self.callIdHex = callIdHex
        self.contacts = contacts
        self.profiles = profiles
        self.tag = tag
        self.name = name
    }

    /// The production resolver: accepted contacts + received profiles with an
    /// account id, the FFI tag derivation (one canonical `peer_tag` shared with
    /// the SFU), and the tile-name rule over the app's one display-name
    /// resolver (``CallTileName/storedName(signatureKey:directory:)``).
    static func live(callIdHex: String, directory: ProfileDirectory = .shared) -> GroupCallNameResolver {
        GroupCallNameResolver(
            callIdHex: callIdHex,
            contacts: {
                AcceptedContactsStore.load().compactMap { c in
                    c.request.fromAccountId.map {
                        GroupCallPeerTags.Person(accountIdHex: hexEncode($0), signatureKey: c.request.fromPubkey)
                    }
                }
            },
            profiles: {
                directory.profilesWithAccountId().compactMap { row in
                    Data(base64Encoded: row.signatureKeyB64).map {
                        GroupCallPeerTags.Person(accountIdHex: hexEncode(row.accountId), signatureKey: $0)
                    }
                }
            },
            tag: { callIdHex, accountIdHex in sfuPeerTag(callId: callIdHex, accountId: accountIdHex) },
            name: { CallTileName.storedName(signatureKey: $0, directory: directory) }
        )
    }

    /// The tile name for a roster event, rebuilding the map from the sources
    /// as they are now. Empty while this device cannot name the peer: the
    /// tile then draws the call screen's unnamed state.
    func resolve(peerId: String, peerTag: String) -> String {
        seen[SfuCallUi.participantId(forPeer: peerId)] = peerTag
        return tileName(currentMap(), peerTag: peerTag)
    }

    /// The person behind `peerTag` as the sources are now, or `nil`.
    func person(peerTag: String) -> Data? {
        currentMap().person(peerTag: peerTag)
    }

    /// Re-label every participant resolved so far on `ui` whenever `changes`
    /// fires (the profile directory's `revision` in production). Weak on both
    /// sides: the subscription must not keep a finished call's screen alive.
    func follow<P: Publisher>(_ changes: P, ui: CallUiState) where P.Output == UInt64, P.Failure == Never {
        subscription = changes
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak self, weak ui] _ in
                guard let self, let ui else { return }
                Task { @MainActor in self.relabel(ui) }
            }
    }

    /// Apply the current map to every participant this resolver has named.
    func relabel(_ ui: CallUiState) {
        let map = currentMap()
        for (id, peerTag) in seen {
            ui.setParticipantName(id, tileName(map, peerTag: peerTag))
        }
    }

    private func tileName(_ map: GroupCallPeerTags.Map, peerTag: String) -> String {
        map.person(peerTag: peerTag).flatMap(name) ?? ""
    }

    private func currentMap() -> GroupCallPeerTags.Map {
        GroupCallPeerTags.build(
            contacts: contacts(),
            profiles: profiles(),
            tag: { [callIdHex, tag] in tag(callIdHex, $0) }
        )
    }
}
