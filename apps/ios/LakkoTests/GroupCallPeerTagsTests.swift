// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0021 on iOS — the per-call `peer_tag -> person` map behind a call
/// roster, fed by received profiles (ADR-0048 envelope v2) as well as by
/// accepted contacts.
///
/// Two defects these pin. The map was built from the accepted-contacts list
/// alone, so every member who was not a contact went unnamed on the call while
/// the members panel, resolving by signature key, named them. And a peer the
/// map could not answer for was labelled with the first eight characters of
/// the pseudonym the SFU sent, which is a piece of the account id and names
/// nobody: it is an empty name now, the tile's unnamed state
/// (`docs/ux/calls.md`, "Every tile carries the person's name"). The tag
/// derivation and the name lookup are injected; nothing here touches the FFI
/// or the profile directory.
// Fixtures live outside the test class so they are plain, nonisolated
// function values the builder's `(String) -> String` parameter accepts
// without an actor hop.

private let keyA = Data(repeating: 0xA1, count: 32)
private let keyB = Data(repeating: 0xB2, count: 32)
private let keyC = Data(repeating: 0xC3, count: 32)

/// A stand-in for `sfuPeerTag`: one tag per account id, prefixed so a test can
/// read it. The tag is a pure function of the account id, which is the
/// property the real derivation has for one `call_id`.
private func tag(_ accountIdHex: String) -> String { "tag:" + accountIdHex }

/// A stand-in for the tile-name rule over the profile store: `nil` for a
/// person the store cannot name.
private func tileName(_ key: Data) -> String? {
    switch key {
    case keyA: return "Anna"
    case keyB: return "Boris"
    case keyC: return "Clara"
    default: return nil
    }
}

private func person(_ accountIdHex: String, _ key: Data) -> GroupCallPeerTags.Person {
    GroupCallPeerTags.Person(accountIdHex: accountIdHex, signatureKey: key)
}

@MainActor
final class GroupCallPeerTagsTests: XCTestCase {

    // MARK: - The merge

    /// The regression itself: a person known only through a received profile
    /// is found on the call, exactly like an accepted contact.
    func testBothSourcesAreMerged() {
        let map = GroupCallPeerTags.build(
            contacts: [person("aa", keyA)],
            profiles: [person("bb", keyB)],
            tag: tag)
        XCTAssertEqual(map.person(peerTag: "tag:aa"), keyA)
        XCTAssertEqual(map.person(peerTag: "tag:bb"), keyB)
        XCTAssertTrue(map.collided.isEmpty)
    }

    func testAProfileAloneIsEnough() {
        let map = GroupCallPeerTags.build(contacts: [], profiles: [person("bb", keyB)], tag: tag)
        XCTAssertEqual(map.person(peerTag: "tag:bb"), keyB)
    }

    /// The same account reached through both sources is ONE person, not a
    /// collision — the common case, since an accepted contact usually also
    /// sent a profile.
    func testTheSameAccountInBothSourcesIsNotACollision() {
        let map = GroupCallPeerTags.build(
            contacts: [person("aa", keyA)],
            profiles: [person("aa", keyA)],
            tag: tag)
        XCTAssertTrue(map.collided.isEmpty)
        XCTAssertEqual(map.person(peerTag: "tag:aa"), keyA)
    }

    /// Nor is the same account id spelled in a different hex case.
    func testAccountIdHexCaseDoesNotSplitAPerson() {
        let map = GroupCallPeerTags.build(
            contacts: [person("AB", keyA)],
            profiles: [person("ab", keyA)],
            tag: tag)
        XCTAssertTrue(map.collided.isEmpty)
        XCTAssertEqual(map.people.count, 1)
    }

    // MARK: - The collision rule

    /// Two DIFFERENT accounts whose tags coincide: naming either would be a
    /// guess, so the map answers for neither.
    func testACollisionHidesBoth() {
        let colliding: (String) -> String = { _ in "same" }
        let map = GroupCallPeerTags.build(
            contacts: [person("aa", keyA)],
            profiles: [person("bb", keyB)],
            tag: colliding)
        XCTAssertEqual(map.collided, ["same"])
        XCTAssertNil(map.person(peerTag: "same"))
    }

    /// A collision within one source is treated the same as one across them.
    func testACollisionInsideOneSourceHidesBothToo() {
        let colliding: (String) -> String = { _ in "same" }
        let map = GroupCallPeerTags.build(
            contacts: [], profiles: [person("bb", keyB), person("cc", keyC)],
            tag: colliding)
        XCTAssertNil(map.person(peerTag: "same"))
    }

    /// A collided tag stays collided however many more people derive to it.
    func testACollisionIsNotUndoneByAThird() {
        let colliding: (String) -> String = { _ in "same" }
        let map = GroupCallPeerTags.build(
            contacts: [person("aa", keyA), person("bb", keyB), person("cc", keyC)],
            profiles: [],
            tag: colliding)
        XCTAssertEqual(map.collided, ["same"])
    }

    // MARK: - The unnamed state

    /// An unknown tag and an empty one are nobody.
    func testAnUnknownTagIsNobody() {
        let map = GroupCallPeerTags.build(contacts: [person("aa", keyA)], profiles: [], tag: tag)
        XCTAssertNil(map.person(peerTag: "tag:zz"))
        XCTAssertNil(map.person(peerTag: ""))
    }

    /// A peer nobody here can name gets an empty name: the tile draws its
    /// unnamed state. Never a number, and never a piece of an identifier.
    func testAPeerNobodyCanNameGetsAnEmptyNameNeverAnIdentifier() {
        let resolver = GroupCallNameResolver(
            callIdHex: "c0ffee",
            contacts: { [person("aa", keyA), person("dd", Data(repeating: 0xD4, count: 32))] },
            profiles: { [] },
            tag: { callIdHex, accountIdHex in callIdHex + ":" + accountIdHex },
            name: tileName)
        // Unknown to the map.
        XCTAssertEqual(resolver.resolve(peerId: "01a0a045-peer", peerTag: "c0ffee:zz"), "")
        // Known to the map, unknown to the profile store.
        XCTAssertEqual(resolver.resolve(peerId: "p2", peerTag: "c0ffee:dd"), "")
        XCTAssertEqual(resolver.resolve(peerId: "p3", peerTag: "c0ffee:aa"), "Anna")
    }

    // MARK: - The live resolver

    /// A profile can arrive AFTER the peer it names joined. The map is not
    /// frozen at call start: the next roster event sees the new source, and
    /// the tile already on screen is re-labelled in place.
    func testAProfileArrivingMidCallRenamesTheTileAlreadyOnScreen() {
        var profiles: [GroupCallPeerTags.Person] = []
        let resolver = GroupCallNameResolver(
            callIdHex: "c0ffee",
            contacts: { [] },
            profiles: { profiles },
            tag: { callIdHex, accountIdHex in callIdHex + ":" + accountIdHex },
            name: tileName)
        let ui = CallUiState()

        // The newcomer joins before anyone knows their name.
        let first = resolver.resolve(peerId: "p1", peerTag: "c0ffee:bb")
        XCTAssertEqual(first, "")
        ui.setParticipants([
            CallUiState.Participant(
                id: SfuCallUi.participantId(forPeer: "p1"), displayName: first, isLocal: false,
                micOn: true, camOn: false, handRaised: false, quality: .good,
                speaking: false, screenSharing: false)
        ])

        // Their profile lands; the roster is re-labelled without a new event.
        profiles = [person("bb", keyB)]
        resolver.relabel(ui)
        XCTAssertEqual(ui.participants.first?.displayName, "Boris")

        // And a later roster event for the same peer answers the name directly.
        XCTAssertEqual(resolver.resolve(peerId: "p1", peerTag: "c0ffee:bb"), "Boris")
    }

    /// The tag is derived over THIS call's id — a profile's account id is
    /// matched against the tag the SFU minted for this call, not a stale one.
    func testTheTagIsDerivedOverTheCallId() {
        let resolver = GroupCallNameResolver(
            callIdHex: "call-2",
            contacts: { [person("aa", keyA)] },
            profiles: { [] },
            tag: { callIdHex, accountIdHex in callIdHex + ":" + accountIdHex },
            name: tileName)
        XCTAssertEqual(resolver.resolve(peerId: "p", peerTag: "call-2:aa"), "Anna")
        XCTAssertEqual(resolver.resolve(peerId: "p", peerTag: "call-1:aa"), "")
        XCTAssertEqual(resolver.person(peerTag: "call-2:aa"), keyA)
        XCTAssertNil(resolver.person(peerTag: "call-1:aa"))
    }

    /// The re-label touches only names that changed and ignores ids no longer
    /// on the roster, so a profile change publishes nothing for a roster that
    /// already agrees.
    func testRelabelIsANoOpForAnAgreeingRoster() {
        let ui = CallUiState()
        ui.setParticipants([
            CallUiState.Participant(
                id: "peer-p1", displayName: "Anna", isLocal: false,
                micOn: true, camOn: false, handRaised: false, quality: .good,
                speaking: false, screenSharing: false)
        ])
        var publishes = 0
        let sub = ui.$participants.dropFirst().sink { _ in publishes += 1 }
        ui.setParticipantName("peer-p1", "Anna")
        ui.setParticipantName("peer-gone", "Boris")
        XCTAssertEqual(publishes, 0)
        ui.setParticipantName("peer-p1", "Anna P.")
        XCTAssertEqual(publishes, 1)
        XCTAssertEqual(ui.participants.first?.displayName, "Anna P.")
        sub.cancel()
    }

    // MARK: - The stored account id

    /// A row written before the profile carried an account id still decodes,
    /// with no id — it keeps naming the person everywhere but the call tile.
    func testARowWithoutAnAccountIdStillDecodes() throws {
        let json = Data(#"{"displayName":"Anna","username":null,"updatedAtMs":5}"#.utf8)
        let claim = try JSONDecoder().decode(ProfileClaim.self, from: json)
        XCTAssertEqual(claim.displayName, "Anna")
        XCTAssertNil(claim.accountId)
    }

    /// Only a whole 16-byte id is an id.
    func testAWrongLengthAccountIdIsNone() {
        XCTAssertNil(ProfileClaim(displayName: nil, username: nil, updatedAtMs: 1,
                                  accountId: Data(repeating: 1, count: 15)).accountId)
        XCTAssertEqual(ProfileClaim(displayName: nil, username: nil, updatedAtMs: 1,
                                    accountId: Data(repeating: 1, count: 16)).accountId?.count, 16)
    }

    /// An account id once learned survives a newer profile that omits it: the
    /// key belongs to one account, and the newer name still replaces the old.
    func testANewerProfileWithoutAnIdKeepsTheKnownId() {
        let store = ReceivedProfileStore(persistence: MemoryPersistence(), isNewer: { $0 > $1 })
        store.activate(accountIdB64: "me")
        let keyB64 = base64Encode(keyA)
        let id = Data(repeating: 7, count: 16)
        XCTAssertEqual(
            store.accept(ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 1, accountId: id),
                         signatureKeyB64: keyB64),
            .stored)
        XCTAssertEqual(
            store.accept(ProfileClaim(displayName: "Anna P.", username: nil, updatedAtMs: 2),
                         signatureKeyB64: keyB64),
            .stored)
        let stored = store.profile(signatureKeyB64: keyB64)
        XCTAssertEqual(stored?.displayName, "Anna P.")
        XCTAssertEqual(stored?.accountId, id)
        let rows = store.profilesWithAccountId()
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows.first?.signatureKeyB64, keyB64)
        XCTAssertEqual(rows.first?.accountId, id)
    }

    /// The same profile sent again (a request answer carries the stamp it was
    /// set at) is not newer, yet it may carry the account id the held row
    /// lacks — a row filed before the envelope had the field. The id is
    /// learned; a different stamp or an already-known id learns nothing.
    func testASameStampResendFillsInAMissingAccountId() {
        let store = ReceivedProfileStore(persistence: MemoryPersistence(), isNewer: { $0 > $1 })
        store.activate(accountIdB64: "me")
        let keyB64 = base64Encode(keyA)
        let id = Data(repeating: 5, count: 16)
        _ = store.accept(ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 3),
                         signatureKeyB64: keyB64)
        XCTAssertEqual(
            store.accept(ProfileClaim(displayName: "Mallory", username: nil, updatedAtMs: 3, accountId: id),
                         signatureKeyB64: keyB64),
            .stored)
        let stored = store.profile(signatureKeyB64: keyB64)
        XCTAssertEqual(stored?.displayName, "Anna", "only the id is taken, never the name")
        XCTAssertEqual(stored?.accountId, id)
        // An older stamp with an id learns nothing; a known id is not replaced.
        XCTAssertEqual(
            store.accept(ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 2,
                                      accountId: Data(repeating: 6, count: 16)),
                         signatureKeyB64: keyB64),
            .notNewer)
        XCTAssertEqual(
            store.accept(ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 3,
                                      accountId: Data(repeating: 6, count: 16)),
                         signatureKeyB64: keyB64),
            .notNewer)
        XCTAssertEqual(store.profile(signatureKeyB64: keyB64)?.accountId, id)
    }

    /// The enumerator lists only rows that name an account id.
    func testTheEnumeratorSkipsRowsWithoutAnId() {
        let store = ReceivedProfileStore(persistence: MemoryPersistence(), isNewer: { $0 > $1 })
        store.activate(accountIdB64: "me")
        _ = store.accept(ProfileClaim(displayName: "Anna", username: nil, updatedAtMs: 1),
                         signatureKeyB64: base64Encode(keyA))
        _ = store.accept(ProfileClaim(displayName: "Boris", username: nil, updatedAtMs: 1,
                                      accountId: Data(repeating: 9, count: 16)),
                         signatureKeyB64: base64Encode(keyB))
        let rows = store.profilesWithAccountId()
        XCTAssertEqual(rows.map(\.signatureKeyB64), [base64Encode(keyB)])
    }

    private final class MemoryPersistence: ProfilePersistence {
        var blobs: [String: Data] = [:]
        func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
        func save(_ data: Data, accountIdB64: String) { blobs[accountIdB64] = data }
        func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
    }
}
