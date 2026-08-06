// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

/// Unit tests for the pure, FFI-free silent-push NSE decrypt core
/// (`decryptPushPreview`, ADR-0041 Phase 3). Drives it with hand-rolled fakes
/// so no slim `LakkoCoreExtension.xcframework` / Keychain / DS is needed — the
/// core file is compiled directly into this test target.
///
/// The iOS counterpart of the web `src/sw/push-decrypt.test.ts` (commit
/// `148753b`) + Android `PushPreviewDecryptTest.kt` (commit `ae9a087`) — happy
/// path, newest-wins, cursor-vs-`p-1` start, every fail-closed branch, and the
/// explicit NEVER-PERSISTS assertions (the throwaway group is always closed +
/// no write hook exists at all: `NsePushDecryptDeps` has no write path, and
/// `FakeThrowawayGroup` exposes `persistAttempted` — always false — because
/// there is no persist entry point on the `NseThrowawayGroup` protocol).
final class NsePushDecryptTests: XCTestCase {

    private let groupHex = String(repeating: "a", count: 64)
    private let groupIdBytes = Data(repeating: 0xAA, count: 32)
    private let accountId = "acct-b64=="

    // MARK: - Fakes

    /// A canned throwaway group: `decode` maps a wire payload's first byte to
    /// a `NseProcessedRow` (or a thrown decrypt failure via `throwOn`). Tracks
    /// close + a would-be-persist flag.
    private final class FakeThrowawayGroup: NseThrowawayGroup {
        private let decode: (Data) -> NseProcessedRow?
        private let throwOn: Set<Int>
        var closed = false
        var processCount = 0

        /// Stays false forever — there is no persist entry point on the
        /// protocol. The test asserts it, mirroring the web/Android
        /// "never-persists" spy.
        let persistAttempted = false

        init(decode: @escaping (Data) -> NseProcessedRow?, throwOn: Set<Int> = []) {
            self.decode = decode
            self.throwOn = throwOn
        }

        func process(_ payload: Data) -> NseProcessedRow? {
            processCount += 1
            let tag = payload.isEmpty ? -1 : Int(payload[payload.startIndex])
            if throwOn.contains(tag) {
                // Mirror a per-row decrypt failure: the real impl returns nil;
                // the fake models the "throws internally, dropped" shape by
                // returning nil for the thrown tag (the core skips nils).
                return nil
            }
            return decode(payload)
        }

        func close() { closed = true }
    }

    /// Fully-configurable fake deps. Records the resolved account + the
    /// `afterId` the core fetched with, so the cursor math is assertable. NO
    /// write method exists (structural read-only).
    private final class FakeDeps: NsePushDecryptDeps {
        let account: NseResolvedAccount?
        let seed: Data?
        let snapshot: Data?
        let group: FakeThrowawayGroup?
        let rows: [NseFetchedRow]
        let cursor: Int64?
        let dsThrows: Bool
        let openThrows: Bool
        let armed: Bool

        var resolveCount = 0
        var openCount = 0
        var dsFetchCount = 0
        var lastAfterId: Int64?

        init(
            account: NseResolvedAccount?,
            seed: Data? = Data(repeating: 1, count: 64),
            snapshot: Data? = Data(repeating: 2, count: 8),
            group: FakeThrowawayGroup? = nil,
            rows: [NseFetchedRow] = [],
            cursor: Int64? = nil,
            dsThrows: Bool = false,
            openThrows: Bool = false,
            armed: Bool = false
        ) {
            self.account = account
            self.seed = seed
            self.snapshot = snapshot
            self.group = group
            self.rows = rows
            self.cursor = cursor
            self.dsThrows = dsThrows
            self.openThrows = openThrows
            self.armed = armed
        }

        func resolveAccount(groupHex: String) -> NseResolvedAccount? {
            resolveCount += 1
            return account
        }

        func loadIdentitySeed(accountIdB64: String) -> Data? { seed }

        func loadSnapshot(accountIdB64: String) -> Data? { snapshot }

        func openThrowawayGroup(
            seed: Data,
            snapshot: Data,
            groupIdBytes: Data
        ) throws -> NseThrowawayGroup? {
            openCount += 1
            if openThrows { throw FakeError.open }
            return group
        }

        func dsFetch(groupIdBytes: Data, afterId: Int64) async throws -> [NseFetchedRow] {
            dsFetchCount += 1
            lastAfterId = afterId
            if dsThrows { throw FakeError.ds }
            return rows
        }

        func startCursor(accountIdB64: String, groupHex: String) -> Int64? { cursor }

        func isAntiCensorshipArmed() -> Bool { armed }
    }

    private enum FakeError: Error { case open, ds }

    private func okAccount() -> NseResolvedAccount {
        NseResolvedAccount(accountIdB64: accountId, groupIdBytes: groupIdBytes)
    }

    private func row(_ id: Int64, _ tag: UInt8) -> NseFetchedRow {
        NseFetchedRow(id: id, payload: Data([tag]))
    }

    private func chat(_ text: String, attachments: Int = 0) -> NseProcessedRow {
        NseProcessedRow(kind: .chat, text: text, attachmentCount: attachments)
    }

    // MARK: - Happy path

    func testDecryptsNewestChatToRealPreview() async {
        let group = FakeThrowawayGroup(decode: { payload in
            switch payload[payload.startIndex] {
            case 1: return self.chat("hello")
            case 2: return self.chat("world")
            default: return nil
            }
        })
        let deps = FakeDeps(account: okAccount(), group: group, rows: [row(10, 1), row(11, 2)])
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 12), deps)

        XCTAssertEqual(preview?.title, "Lakko")
        XCTAssertEqual(preview?.body, "world") // newest-wins
        XCTAssertEqual(preview?.tag, groupHex)
        XCTAssertTrue(group.closed, "throwaway group must be closed")
        XCTAssertFalse(group.persistAttempted, "no persist entry point may be reached")
    }

    func testAttachmentOnlyChatRendersHint() async {
        let one = FakeThrowawayGroup(decode: { _ in self.chat("", attachments: 1) })
        let a = await decryptPushPreview(
            NsePushWake(groupHex: groupHex, pointer: 5),
            FakeDeps(account: okAccount(), group: one, rows: [row(5, 1)])
        )
        XCTAssertEqual(a?.body, "Sent an attachment")

        let many = FakeThrowawayGroup(decode: { _ in self.chat("", attachments: 3) })
        let b = await decryptPushPreview(
            NsePushWake(groupHex: groupHex, pointer: 5),
            FakeDeps(account: okAccount(), group: many, rows: [row(5, 1)])
        )
        XCTAssertEqual(b?.body, "Sent 3 attachments")
    }

    func testEditSurfacesNewText() async {
        let group = FakeThrowawayGroup(decode: { _ in
            NseProcessedRow(kind: .edit, text: "edited body", attachmentCount: 0)
        })
        let preview = await decryptPushPreview(
            NsePushWake(groupHex: groupHex, pointer: 5),
            FakeDeps(account: okAccount(), group: group, rows: [row(5, 1)])
        )
        XCTAssertEqual(preview?.body, "edited body")
    }

    // MARK: - Newest-wins + kind filtering

    func testLaterNonPreviewOpKeepsOlderChat() async {
        // rows: chat "hi" (id 10), delete (id 11). Delete yields no preview →
        // the older chat stays.
        let group = FakeThrowawayGroup(decode: { payload in
            switch payload[payload.startIndex] {
            case 1: return self.chat("hi")
            case 2: return NseProcessedRow(kind: .delete, text: "", attachmentCount: 0)
            default: return nil
            }
        })
        let preview = await decryptPushPreview(
            NsePushWake(groupHex: groupHex, pointer: 12),
            FakeDeps(account: okAccount(), group: group, rows: [row(10, 1), row(11, 2)])
        )
        XCTAssertEqual(preview?.body, "hi")
    }

    func testOnlyNonPreviewOpsYieldGeneric() async {
        for kind in [NseProcessedKind.delete, .reaction, .typing, .readReceipt] {
            let group = FakeThrowawayGroup(decode: { _ in
                NseProcessedRow(kind: kind, text: "", attachmentCount: 0)
            })
            let preview = await decryptPushPreview(
                NsePushWake(groupHex: groupHex, pointer: 5),
                FakeDeps(account: okAccount(), group: group, rows: [row(5, 1)])
            )
            XCTAssertNil(preview, "kind \(kind) must not produce a preview")
            XCTAssertTrue(group.closed, "group closed even with no preview")
        }
    }

    func testEmptyChatWithNoAttachmentsYieldsGeneric() async {
        let group = FakeThrowawayGroup(decode: { _ in self.chat("", attachments: 0) })
        let deps = FakeDeps(account: okAccount(), group: group, rows: [row(5, 1)])
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 5), deps)
        XCTAssertNil(preview)
    }

    // MARK: - Fail-closed branches → nil (→ generic)

    func testNonGroupWakesYieldNil() async {
        let deps = FakeDeps(account: okAccount())
        let none = await decryptPushPreview(nil, deps)
        XCTAssertNil(none)
        let noGroup = await decryptPushPreview(NsePushWake(groupHex: nil, pointer: 5), deps)
        XCTAssertNil(noGroup)
        let noPointer = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: nil), deps)
        XCTAssertNil(noPointer)
        let wrongLen = await decryptPushPreview(NsePushWake(groupHex: "abc", pointer: 5), deps)
        XCTAssertNil(wrongLen, "wrong length")
        let nonHex = await decryptPushPreview(
            NsePushWake(groupHex: String(repeating: "z", count: 64), pointer: 5), deps
        )
        XCTAssertNil(nonHex, "non-hex")
        XCTAssertEqual(deps.resolveCount, 0, "no account lookup for a malformed wake")
    }

    func testUnknownGroupYieldsNil() async {
        let deps = FakeDeps(account: nil)
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 5), deps)
        XCTAssertNil(preview)
    }

    func testMissingSeedYieldsNil() async {
        let deps = FakeDeps(account: okAccount(), seed: nil)
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 5), deps)
        XCTAssertNil(preview)
        XCTAssertEqual(deps.openCount, 0, "no group open without a seed")
    }

    func testMissingSnapshotYieldsNil() async {
        let deps = FakeDeps(account: okAccount(), snapshot: nil)
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 5), deps)
        XCTAssertNil(preview)
        XCTAssertEqual(deps.openCount, 0, "no group open without a snapshot")
    }

    func testNonMemberGroupYieldsNil() async {
        let deps = FakeDeps(account: okAccount(), group: nil)
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 5), deps)
        XCTAssertNil(preview)
        XCTAssertEqual(deps.openCount, 1)
        XCTAssertEqual(deps.dsFetchCount, 0, "no DS fetch when we are not a member")
    }

    func testOpenThrowingYieldsNil() async {
        let deps = FakeDeps(account: okAccount(), openThrows: true)
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 5), deps)
        XCTAssertNil(preview)
    }

    func testDsFetchFailureYieldsNilAndClosesGroup() async {
        let group = FakeThrowawayGroup(decode: { _ in self.chat("nope") })
        let deps = FakeDeps(account: okAccount(), group: group, dsThrows: true)
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 5), deps)
        XCTAssertNil(preview)
        XCTAssertTrue(group.closed, "group closed on DS failure")
    }

    func testPerRowFailureSkippedLaterRowsProcessed() async {
        // Row id 10 (tag 1) is dropped in process(); row id 11 (tag 2) is a
        // good chat. The dropped row is skipped fail-closed; the good one
        // becomes the preview.
        let group = FakeThrowawayGroup(
            decode: { payload in payload[payload.startIndex] == 2 ? self.chat("survivor") : nil },
            throwOn: [1]
        )
        let preview = await decryptPushPreview(
            NsePushWake(groupHex: groupHex, pointer: 12),
            FakeDeps(account: okAccount(), group: group, rows: [row(10, 1), row(11, 2)])
        )
        XCTAssertEqual(preview?.body, "survivor")
        XCTAssertEqual(group.processCount, 2)
    }

    func testZeroFetchedRowsYieldsNil() async {
        let group = FakeThrowawayGroup(decode: { _ in self.chat("x") })
        let deps = FakeDeps(account: okAccount(), group: group, rows: [])
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 5), deps)
        XCTAssertNil(preview)
        XCTAssertTrue(group.closed)
    }

    // MARK: - Cursor selection (the read-only start-cursor math)

    func testNoPersistedCursorFetchesFromPointerMinusOne() async {
        let group = FakeThrowawayGroup(decode: { _ in nil })
        let deps = FakeDeps(account: okAccount(), group: group, cursor: nil)
        _ = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 20), deps)
        XCTAssertEqual(deps.lastAfterId, 19)
    }

    func testPersistedCursorBelowPointerIsPreferred() async {
        let group = FakeThrowawayGroup(decode: { _ in nil })
        let deps = FakeDeps(account: okAccount(), group: group, cursor: 5)
        _ = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 20), deps)
        XCTAssertEqual(deps.lastAfterId, 5, "intervening commits replay from the app cursor")
    }

    func testPersistedCursorFloorsTheFetchEvenAbovePointer() async {
        // ADR-0041 Finding-2 — the persisted cursor is the fetch FLOOR: even
        // when it is at/above the wake pointer (a stale/ahead wake), we start
        // at the cursor (the app has durably seen everything up to it), never
        // below it. A hostile push-gw pointer can no longer force a fetch
        // below the cursor (the full-journal-replay amplification vector).
        let group = FakeThrowawayGroup(decode: { _ in nil })
        let deps = FakeDeps(account: okAccount(), group: group, cursor: 25)
        _ = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 20), deps)
        XCTAssertEqual(deps.lastAfterId, 25, "cursor floors the fetch, never below it")
    }

    func testHostilePointerCannotForceFullJournalReplayWhenCursorPresent() async {
        // A malicious push-gw sets p=1 to force `after = 0` (whole-journal
        // replay). With a real app cursor present, the cursor floor holds and
        // we fetch only what the app has not seen.
        let group = FakeThrowawayGroup(decode: { _ in nil })
        let deps = FakeDeps(account: okAccount(), group: group, cursor: 500)
        _ = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 1), deps)
        XCTAssertEqual(deps.lastAfterId, 500, "cursor floor caps a hostile p=1 wake")
    }

    func testAfterIsClampedToZero() async {
        let group = FakeThrowawayGroup(decode: { _ in nil })
        let deps = FakeDeps(account: okAccount(), group: group, cursor: nil)
        _ = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 0), deps)
        XCTAssertEqual(deps.lastAfterId, 0)
    }

    // MARK: - Anti-censorship gate (ADR-0041 Finding-1) → nil, NO fetch

    func testArmedAntiCensorshipReturnsNilWithoutFetching() async {
        // When anti-censorship is armed, the NSE must make NO direct DS
        // connection — it short-circuits to the generic banner. Assert the
        // fetch spy is never called (and nothing else, either — the gate is
        // the very first step).
        let group = FakeThrowawayGroup(decode: { _ in self.chat("secret") })
        let deps = FakeDeps(
            account: okAccount(),
            group: group,
            rows: [row(10, 1)],
            armed: true
        )
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 12), deps)

        XCTAssertNil(preview, "armed → no direct connection, generic banner")
        XCTAssertEqual(deps.dsFetchCount, 0, "armed must make NO DS fetch")
        XCTAssertEqual(deps.resolveCount, 0, "armed short-circuits before any store read")
        XCTAssertEqual(deps.openCount, 0, "armed opens no throwaway group")
    }

    func testDisarmedAntiCensorshipStillPreviews() async {
        // The default (disarmed) path is unaffected — the happy preview still
        // fetches + decrypts.
        let group = FakeThrowawayGroup(decode: { _ in self.chat("hello") })
        let deps = FakeDeps(
            account: okAccount(),
            group: group,
            rows: [row(10, 1)],
            armed: false
        )
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 12), deps)

        XCTAssertEqual(preview?.body, "hello")
        XCTAssertEqual(deps.dsFetchCount, 1, "disarmed still fetches")
    }

    // MARK: - NEVER-PERSISTS guard (read-only-throwaway discipline)

    func testFullHappyPathNeverPersistsAndAlwaysDiscards() async {
        let group = FakeThrowawayGroup(decode: { _ in self.chat("final") })
        let deps = FakeDeps(account: okAccount(), group: group, rows: [row(9, 1)])
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 10), deps)

        XCTAssertEqual(preview?.body, "final")
        // The throwaway is discarded...
        XCTAssertTrue(group.closed, "group must be closed (discarded)")
        // ...and no persist / snapshot / cursor-advance was reachable.
        XCTAssertFalse(group.persistAttempted, "no persist may be attempted")
        // The DS was read exactly once (no re-fetch that a cursor advance
        // would imply).
        XCTAssertEqual(deps.dsFetchCount, 1)
    }
}
