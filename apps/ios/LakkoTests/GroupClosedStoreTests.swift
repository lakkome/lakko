// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// ADR-0073 R.6 - the claim state's transitions, codec and store. The cases
/// are those of `apps/web/src/chat/group-closed-store.test.ts`; the store
/// cases are restated for `UserDefaults` (which cannot throw on a read, so
/// the web "unreadable backend" case becomes "an unreadable blob is none").
final class GroupClosedStoreTests: XCTestCase {

    private var suiteName = ""
    private var defaults: UserDefaults = .standard

    override func setUp() {
        super.setUp()
        suiteName = "GroupClosedStoreTests." + UUID().uuidString
        defaults = UserDefaults(suiteName: suiteName) ?? .standard
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func claim(_ messageId: Int64, _ successor: String = "S") -> SuccessorClaim {
        SuccessorClaim(
            messageId: messageId, successorGroupIdB64: successor, senderKeyB64: "sender",
            senderWasAdmin: true, adminKeysAtReceiptB64: ["sender", "other"])
    }

    // MARK: - The claim state

    func testKeepsPendingClaimsInJournalOrderEachOnceBounded() {
        var state = StoredGroupClosed.noClaims
        state = GroupClosedRules.withPendingClaim(state, claim(9))
        state = GroupClosedRules.withPendingClaim(state, claim(3))
        state = GroupClosedRules.withPendingClaim(state, claim(9))
        XCTAssertEqual(state.pending.map(\.messageId), [3, 9])
        let extra = Int64(GroupClosedRules.maxPendingClaims + 5)
        for i in Int64(100)..<(100 + extra) {
            state = GroupClosedRules.withPendingClaim(state, claim(i))
        }
        XCTAssertEqual(state.pending.count, GroupClosedRules.maxPendingClaims)
        XCTAssertEqual(state.pending.first?.messageId, 3)
    }

    func testAnAcceptedClaimSpendsThePendingListAndKeepsTheOwnClaimRecord() {
        let state = StoredGroupClosed(
            accepted: nil, pending: [claim(3)],
            own: OwnClaim(successorGroupIdB64: "mine", messageId: 12))
        XCTAssertEqual(
            GroupClosedRules.withAcceptedClaim(state, claim(3)),
            StoredGroupClosed(
                accepted: claim(3), pending: [],
                own: OwnClaim(successorGroupIdB64: "mine", messageId: 12)))
    }

    func testTheTransitionsKeepThePreparedSuccessor() {
        let prepared = PreparedSuccessor(
            successorGroupIdB64: "mine", welcomeB64: "d2VsY29tZQ==", targetKeysB64: ["a"], delivered: false)
        let state = StoredGroupClosed(accepted: nil, pending: [], own: nil, prepared: prepared)
        XCTAssertEqual(GroupClosedRules.withPendingClaim(state, claim(4)).prepared, prepared)
        XCTAssertEqual(GroupClosedRules.withAcceptedClaim(state, claim(4)).prepared, prepared)
    }

    // MARK: - The row codec

    func testRoundTrips() {
        let rows: [String: StoredGroupClosed] = [
            "G1": StoredGroupClosed(accepted: claim(5, "S5"), pending: [], own: nil),
            "G2": StoredGroupClosed(
                accepted: nil, pending: [claim(2), claim(8)],
                own: OwnClaim(successorGroupIdB64: "mine", messageId: nil)),
            "G3": StoredGroupClosed(
                accepted: claim(4, "mine"), pending: [], own: nil,
                prepared: PreparedSuccessor(
                    successorGroupIdB64: "mine", welcomeB64: "d2VsY29tZQ==", targetKeysB64: ["a", "b"],
                    delivered: false)),
            "G4": StoredGroupClosed(
                accepted: nil, pending: [], own: nil,
                prepared: PreparedSuccessor(
                    successorGroupIdB64: "mine", welcomeB64: nil, targetKeysB64: [], delivered: true)),
        ]
        XCTAssertEqual(GroupClosedRules.decodeRow(GroupClosedRules.encodeRow(rows)), rows)
    }

    func testDoesNotReadARowOfTheRemovedVisibleFlowOrJunk() {
        XCTAssertNil(GroupClosedRules.decodeRow(#"{"v":2,"g":{"G":{"t":"1","s":null,"b":null}}}"#))
        XCTAssertNil(GroupClosedRules.decodeRow("not json"))
        XCTAssertNil(GroupClosedRules.decodeRow("null"))
    }

    func testDropsAMalformedEntryRatherThanTrustingIt() {
        let json = """
            {"v":3,"g":{
              "ok":{"a":null,"p":[],"o":null},
              "bad":{"a":{"m":"x","s":"S","k":"k","a":true,"t":[]},"p":[],"o":null},
              "junkPending":{"a":null,"p":[{"m":-1},{"m":4,"s":"S","k":"k","a":false,"t":["z"]}],"o":{"s":5,"m":1}}
            }}
            """
        let rows = GroupClosedRules.decodeRow(json)
        XCTAssertNotNil(rows)
        XCTAssertNil(rows?["bad"])
        XCTAssertEqual(rows?["ok"], StoredGroupClosed.noClaims)
        XCTAssertEqual(rows?["junkPending"]?.pending.map(\.messageId), [4])
        XCTAssertNil(rows?["junkPending"]?.own)
    }

    func testReadsTheRowTheWebTwinWrites() {
        // The literal a web client's `encodeGroupClosedRow` produces.
        let json = #"{"v":3,"g":{"G":{"a":{"m":7,"s":"S","k":"sender","a":true,"t":["sender","other"]},"p":[],"o":{"s":"mine","m":null}}}}"#
        XCTAssertEqual(
            GroupClosedRules.decodeRow(json),
            ["G": StoredGroupClosed(
                accepted: claim(7), pending: [], own: OwnClaim(successorGroupIdB64: "mine", messageId: nil))])
    }

    func testABooleanIsNotAMessageIdAndANumberIsNotAFlag() {
        let json = #"{"v":3,"g":{"G":{"a":null,"p":[{"m":true,"s":"S","k":"k","a":true,"t":[]},{"m":4,"s":"S","k":"k","a":1,"t":[]},{"m":4.5,"s":"S","k":"k","a":true,"t":[]}],"o":null}}}"#
        XCTAssertEqual(GroupClosedRules.decodeRow(json)?["G"], StoredGroupClosed.noClaims)
    }

    // MARK: - GroupClosedStore

    /// A reversible stand-in for the Keychain wrap: every byte flipped, and
    /// an `open` that refuses what `seal` did not produce.
    private static let marker = Data("sealed:".utf8)
    private let sealer = GroupClosedStore.Sealer(
        seal: { GroupClosedStoreTests.marker + Data($0.map { $0 ^ 0xFF }) },
        open: { sealed in
            guard sealed.starts(with: GroupClosedStoreTests.marker) else {
                throw NSError(domain: "test.sealer", code: 1)
            }
            return Data(sealed.dropFirst(GroupClosedStoreTests.marker.count).map { $0 ^ 0xFF })
        })

    private func makeStore(_ account: String) -> GroupClosedStore {
        GroupClosedStore(defaults: defaults, ownerAccountIdB64: account, sealer: sealer)
    }

    func testWritesThroughAndReloadsPerAccount() {
        let store = makeStore("acct")
        XCTAssertNil(store.read(groupIdB64: "G"))
        let state = GroupClosedRules.withAcceptedClaim(.noClaims, claim(7))
        XCTAssertTrue(store.write(groupIdB64: "G", state))

        let again = makeStore("acct")
        XCTAssertEqual(again.read(groupIdB64: "G")?.accepted?.messageId, 7)
        XCTAssertEqual(again.entries(), ["G": state])
        // Another account sees nothing of it.
        let other = makeStore("acct2")
        XCTAssertNil(other.read(groupIdB64: "G"))
        XCTAssertTrue(other.entries().isEmpty)

        again.drop(groupIdB64: "G")
        XCTAssertNil(store.read(groupIdB64: "G"))
        XCTAssertTrue(store.entries().isEmpty)
    }

    func testOneBlobPerGroup() {
        let store = makeStore("acct")
        store.write(groupIdB64: "G1", GroupClosedRules.withAcceptedClaim(.noClaims, claim(1)))
        store.write(groupIdB64: "G2", GroupClosedRules.withPendingClaim(.noClaims, claim(2)))
        XCTAssertNotNil(defaults.data(forKey: "lakko.group-closed.v3.acct.G1"))
        XCTAssertNotNil(defaults.data(forKey: "lakko.group-closed.v3.acct.G2"))
        store.drop(groupIdB64: "G1")
        XCTAssertNil(store.read(groupIdB64: "G1"))
        XCTAssertEqual(store.read(groupIdB64: "G2")?.pending.map(\.messageId), [2])
    }

    // ADR-0073 R.13b - the claim store is sealed under the device key: it
    // can hold a prepared Welcome.

    func testThePreparedWelcomeIsNeverStoredAsItIs() throws {
        let store = makeStore("acct")
        let welcomeB64 = Data("a prepared welcome".utf8).base64EncodedString()
        let state = StoredGroupClosed(
            accepted: nil, pending: [], own: OwnClaim(successorGroupIdB64: "successor", messageId: 4),
            prepared: PreparedSuccessor(
                successorGroupIdB64: "successor", welcomeB64: welcomeB64,
                targetKeysB64: ["target-key"], delivered: false))
        XCTAssertTrue(store.write(groupIdB64: "G", state))
        let stored = try XCTUnwrap(defaults.data(forKey: "lakko.group-closed.v3.acct.G"))
        for plain in [welcomeB64, "successor", "target-key"] {
            XCTAssertNil(stored.range(of: Data(plain.utf8)), "\(plain) is readable in the stored row")
        }
        // No value of the account's rows is a string any more.
        XCTAssertNil(defaults.string(forKey: "lakko.group-closed.v3.acct.G"))
        XCTAssertEqual(store.read(groupIdB64: "G"), state)
    }

    func testARowThatCannotBeSealedIsNotWritten() {
        let failing = GroupClosedStore(
            defaults: defaults, ownerAccountIdB64: "acct",
            sealer: GroupClosedStore.Sealer(
                seal: { _ in throw NSError(domain: "test.sealer", code: 2) }, open: { $0 }))
        XCTAssertFalse(failing.write(groupIdB64: "G", .noClaims))
        XCTAssertNil(defaults.object(forKey: "lakko.group-closed.v3.acct.G"))
    }

    func testABlobThatDoesNotOpenReadsAsNone() {
        // Sealed under another key (here: not this sealer's output at all).
        defaults.set(Data("not sealed by this device".utf8), forKey: "lakko.group-closed.v3.acct.G")
        let store = makeStore("acct")
        XCTAssertNil(store.read(groupIdB64: "G"))
        XCTAssertTrue(store.entries().isEmpty)
    }

    func testAnUnreadableBlobReadsAsNone() throws {
        defaults.set(try sealer.seal(Data("not json".utf8)), forKey: "lakko.group-closed.v3.acct.G")
        let store = makeStore("acct")
        XCTAssertNil(store.read(groupIdB64: "G"))
        XCTAssertTrue(store.entries().isEmpty)
    }

    func testAPlaintextRowOfTheEarlierStoreIsNotReadAndIsClearedWithTheAccount() {
        let json = GroupClosedRules.encodeRow(["G": GroupClosedRules.withAcceptedClaim(.noClaims, claim(7))])
        defaults.set(json, forKey: "lakko.group-closed.v2.acct.G")
        defaults.set(json, forKey: "lakko.group-closed.v3.acct.G")
        let store = makeStore("acct")
        XCTAssertNil(store.read(groupIdB64: "G"), "a plaintext row is never trusted")
        GroupClosedStore.clear(ownerAccountIdB64: "acct", defaults: defaults)
        XCTAssertNil(defaults.object(forKey: "lakko.group-closed.v2.acct.G"))
        XCTAssertNil(defaults.object(forKey: "lakko.group-closed.v3.acct.G"))
    }

    func testClearForgetsOneAccountOnly() {
        let a = makeStore("acct")
        let b = makeStore("acct2")
        a.write(groupIdB64: "G1", .noClaims)
        a.write(groupIdB64: "G2", .noClaims)
        b.write(groupIdB64: "G1", .noClaims)
        GroupClosedStore.clear(ownerAccountIdB64: "acct", defaults: defaults)
        XCTAssertTrue(a.entries().isEmpty)
        XCTAssertEqual(b.entries(), ["G1": StoredGroupClosed.noClaims])
    }
}
