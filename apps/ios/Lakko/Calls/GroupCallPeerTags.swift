// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import Foundation
import LakkoCore

/// ADR-0021 — the per-call `peer_tag -> label` map behind a group-call roster.
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
/// Pure: the tag derivation and the label lookup are injected, so the merge
/// and the collision rule are tested without the FFI or the directory.
enum GroupCallPeerTags {
    /// One person the map can name: the lowercase-hex account id the tag is
    /// derived over and the signature key the label is resolved from.
    struct Person: Equatable {
        let accountIdHex: String
        let signatureKey: Data
    }

    /// The map plus the tags it must NOT answer for.
    struct Map: Equatable {
        /// `peer_tag -> label` for every distinct account id seen once.
        var labels: [String: String] = [:]
        /// Tags two DIFFERENT account ids derived to. A 64-bit tag can collide;
        /// naming either person would be a guess, so both fall back.
        var collided: Set<String> = []

        /// The tile label for what the SFU said about a peer: the resolved
        /// name when the tag is known and unambiguous, else the pseudonym the
        /// SFU sent (the pre-ADR-0021 fallback), else the word "Peer".
        func label(peerTag: String, pseudonym: String) -> String {
            if !peerTag.isEmpty, !collided.contains(peerTag), let label = labels[peerTag] {
                return label
            }
            return pseudonym.isEmpty ? "Peer" : String(pseudonym.prefix(8))
        }
    }

    /// Fold both sources. Contacts are walked first: the same account reached
    /// through both is ONE person, not a collision, and the first source names
    /// it (the label resolver is the same either way — rename, profile name,
    /// verified username, fingerprint — so the order only makes the outcome
    /// deterministic). Two DIFFERENT account ids meeting at one tag mark it
    /// collided, and a collided tag stays collided however many more arrive.
    static func build(
        contacts: [Person],
        profiles: [Person],
        tag: (_ accountIdHex: String) -> String,
        label: (_ signatureKey: Data) -> String
    ) -> Map {
        var map = Map()
        var seenAccounts = Set<String>()
        for person in contacts + profiles {
            let accountIdHex = person.accountIdHex.lowercased()
            guard !accountIdHex.isEmpty, seenAccounts.insert(accountIdHex).inserted else { continue }
            let peerTag = tag(accountIdHex)
            if map.labels[peerTag] != nil {
                map.collided.insert(peerTag)
            } else {
                map.labels[peerTag] = label(person.signatureKey)
            }
        }
        return map
    }
}

/// The live resolver an SFU group call names its tiles through.
///
/// Not frozen at call start: the map is rebuilt from the current sources on
/// every roster event, and — because a profile can arrive AFTER the peer it
/// names joined (a newcomer asks the group to introduce itself and is answered
/// within about thirty seconds) — the resolver remembers what the SFU said
/// about each participant and re-labels the live roster when the profile
/// directory changes. Owned by the call handle; dropping the handle drops the
/// subscription with it.
@MainActor
final class GroupCallNameResolver {
    private let callIdHex: String
    private let contacts: () -> [GroupCallPeerTags.Person]
    private let profiles: () -> [GroupCallPeerTags.Person]
    private let tag: (_ callIdHex: String, _ accountIdHex: String) -> String
    private let label: (_ signatureKey: Data) -> String
    /// Roster participant id -> what the SFU said about that peer. Kept for
    /// the re-label; a departed peer's entry is harmless (an unknown id is a
    /// no-op) and a rejoin under the same id overwrites it.
    private var seen: [String: (pseudonym: String, peerTag: String)] = [:]
    private var subscription: AnyCancellable?

    init(
        callIdHex: String,
        contacts: @escaping () -> [GroupCallPeerTags.Person],
        profiles: @escaping () -> [GroupCallPeerTags.Person],
        tag: @escaping (_ callIdHex: String, _ accountIdHex: String) -> String,
        label: @escaping (_ signatureKey: Data) -> String
    ) {
        self.callIdHex = callIdHex
        self.contacts = contacts
        self.profiles = profiles
        self.tag = tag
        self.label = label
    }

    /// The production resolver: accepted contacts + received profiles with an
    /// account id, the FFI tag derivation (one canonical `peer_tag` shared with
    /// the SFU), and the app's one display-name resolver. Call tiles show the
    /// name only.
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
            label: { directory.resolve(signatureKey: $0).name }
        )
    }

    /// The label for a roster event, rebuilding the map from the sources as
    /// they are now.
    func resolve(peerId: String, pseudonym: String, peerTag: String) -> String {
        seen[SfuCallUi.participantId(forPeer: peerId)] = (pseudonym: pseudonym, peerTag: peerTag)
        return currentMap().label(peerTag: peerTag, pseudonym: pseudonym)
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
        for (id, peer) in seen {
            ui.setParticipantName(id, map.label(peerTag: peer.peerTag, pseudonym: peer.pseudonym))
        }
    }

    private func currentMap() -> GroupCallPeerTags.Map {
        GroupCallPeerTags.build(
            contacts: contacts(),
            profiles: profiles(),
            tag: { [callIdHex, tag] in tag(callIdHex, $0) },
            label: label
        )
    }
}
