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
    /// ADR-0042 — an opaque `push_gid` hex (the post-Sprint-3 wake `g`);
    /// deliberately DIFFERENT from `groupHex` so a test can prove the real
    /// group_id (not the wake's `g`) drives the fetch/open on a route hit.
    private let pushGidHex = String(repeating: "b", count: 64)

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

        /// ADR-0053 point 9 — records what the fetch asked it to sign.
        var signed: [(method: String, path: String, query: String, body: Data)] = []
        func dsProofHeader(method: String, path: String, query: String, body: Data) -> String? {
            signed.append((method, path, query, body))
            return "v1.fake"
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
        /// ADR-0042 Sprint 4 — the canned reverse-index row and the account
        /// it is filed under (`nil` = miss, the pre-ADR-0042 legacy shape
        /// every older test exercises). A lookup for any other account misses.
        let route: (accountIdB64: String, groupIdHex: String)?
        /// ADR-0042 (amended 2026-09-30) — what the binding gate answers:
        /// the bound account, or `nil` for "no binding / wrong `d`".
        let bound: String?
        let dsThrows: Bool
        let openThrows: Bool
        let armed: Bool

        var resolveCount = 0
        var openCount = 0
        var dsFetchCount = 0
        var lastAfterId: Int64?
        var lookupCount = 0
        var lastLookupPushGid: String?
        var lastLookupAccount: String?
        var lastKnowsGroupAccount: String?
        var boundCount = 0
        var lastWakeDevice: String?
        var lastFetchGroupIdBytes: Data?
        var lastCursorGroupHex: String?
        /// ADR-0053 — the group the fetch was handed to sign with.
        var lastFetchGroup: NseThrowawayGroup?

        init(
            account: NseResolvedAccount?,
            seed: Data? = Data(repeating: 1, count: 64),
            snapshot: Data? = Data(repeating: 2, count: 8),
            group: FakeThrowawayGroup? = nil,
            rows: [NseFetchedRow] = [],
            cursor: Int64? = nil,
            route: (accountIdB64: String, groupIdHex: String)? = nil,
            dsThrows: Bool = false,
            openThrows: Bool = false,
            armed: Bool = false,
            bound: String?? = .none
        ) {
            // By default the gate passes for the test account, so the older
            // tests exercise everything behind it unchanged.
            self.bound = bound ?? "acct-b64=="
            self.account = account
            self.seed = seed
            self.snapshot = snapshot
            self.group = group
            self.rows = rows
            self.cursor = cursor
            self.route = route
            self.dsThrows = dsThrows
            self.openThrows = openThrows
            self.armed = armed
        }

        func boundAccount(wakeDevicePseudonymHex: String?) -> String? {
            boundCount += 1
            lastWakeDevice = wakeDevicePseudonymHex
            return bound
        }

        func lookupRoute(accountIdB64: String, pushGidHex: String) -> String? {
            lookupCount += 1
            lastLookupPushGid = pushGidHex
            lastLookupAccount = accountIdB64
            guard let route, route.accountIdB64 == accountIdB64 else { return nil }
            return route.groupIdHex
        }

        func accountKnowsGroup(accountIdB64: String, groupHex: String) -> Bool {
            resolveCount += 1
            lastKnowsGroupAccount = accountIdB64
            return account?.accountIdB64 == accountIdB64
        }

        /// Every account a per-account store was read for.
        var readAccounts: [String] = []

        func loadIdentitySeed(accountIdB64: String) -> Data? {
            readAccounts.append(accountIdB64)
            return seed
        }

        func loadSnapshot(accountIdB64: String) -> Data? {
            readAccounts.append(accountIdB64)
            return snapshot
        }

        func openThrowawayGroup(
            seed: Data,
            snapshot: Data,
            groupIdBytes: Data
        ) throws -> NseThrowawayGroup? {
            openCount += 1
            if openThrows { throw FakeError.open }
            return group
        }

        func dsFetch(
            groupIdBytes: Data,
            afterId: Int64,
            group: NseThrowawayGroup
        ) async throws -> [NseFetchedRow] {
            lastFetchGroup = group
            dsFetchCount += 1
            lastAfterId = afterId
            lastFetchGroupIdBytes = groupIdBytes
            if dsThrows { throw FakeError.ds }
            return rows
        }

        func startCursor(accountIdB64: String, groupHex: String) -> Int64? {
            readAccounts.append(accountIdB64)
            lastCursorGroupHex = groupHex
            return cursor
        }

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

    /// ADR-0053 point 9 — the journal read is signed through the THROWAWAY
    /// group the wake opened (its access key at the restored epoch), never
    /// through anything that persists.
    func testTheJournalReadIsSignedThroughTheThrowawayGroup() async {
        let group = FakeThrowawayGroup(decode: { _ in self.chat("hi") })
        let deps = FakeDeps(account: okAccount(), group: group, rows: [row(10, 1)])
        _ = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 10), deps)
        XCTAssertTrue(deps.lastFetchGroup === group, "the fetch is handed the opened group to sign with")
        XCTAssertFalse(group.persistAttempted)
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

    /// ADR-0047 — a media-only message is labelled from its already-decoded
    /// `kind`; an unknown kind, or more than one attachment, keeps the generic
    /// line (the NSE never fetches media to find out more).
    func testMediaOnlyChatIsLabelledByKind() async {
        func body(kind: String?, attachments: Int = 1) async -> String? {
            let group = FakeThrowawayGroup(decode: { _ in
                NseProcessedRow(kind: .chat, text: "", attachmentCount: attachments, singleAttachmentKind: kind)
            })
            return await decryptPushPreview(
                NsePushWake(groupHex: groupHex, pointer: 5),
                FakeDeps(account: okAccount(), group: group, rows: [row(5, 1)])
            )?.body
        }
        let voice = await body(kind: "voice")
        let video = await body(kind: "video_note")
        let gif = await body(kind: "gif")
        let unknown = await body(kind: "sticker")
        let plain = await body(kind: nil)
        let many = await body(kind: "voice", attachments: 2)
        XCTAssertEqual(voice, "Voice message")
        XCTAssertEqual(video, "Video message")
        XCTAssertEqual(gif, "GIF")
        XCTAssertEqual(unknown, "Sent an attachment")
        XCTAssertEqual(plain, "Sent an attachment")
        XCTAssertEqual(many, "Sent 2 attachments")
    }

    /// ADR-0061 — the attachment line and the server's generic placeholder
    /// come from the words the app stored, English per word when it has not.
    func testNotificationWordsFollowTheStoredLanguage() {
        let saved = nseNotificationWordLookup
        defer { nseNotificationWordLookup = saved }
        nseNotificationWordLookup = { _ in nil }
        XCTAssertEqual(nseAttachmentsLine(count: 1), "Sent an attachment")
        XCTAssertEqual(nseAttachmentsLine(count: 4), "Sent 4 attachments")
        XCTAssertEqual(nseGenericBody("New message"), "New message")
        XCTAssertEqual(nseGenericBody("Incoming call"), "Incoming call")

        let words = [
            "notification.message.attachments#1": "Вложение",
            "notification.message.attachments#4": "4 вложения",
            "notification.message.new": "Новое сообщение",
        ]
        nseNotificationWordLookup = { words[$0] }
        XCTAssertEqual(nseAttachmentsLine(count: 1), "Вложение")
        XCTAssertEqual(nseAttachmentsLine(count: 4), "4 вложения")
        XCTAssertEqual(nseAttachmentsLine(count: 70), "Sent 70 attachments", "a count the app did not render")
        XCTAssertEqual(nseGenericBody("New message"), "Новое сообщение")
        XCTAssertEqual(nseGenericBody("Incoming call"), "Incoming call", "English per word")
        XCTAssertEqual(nseGenericBody("Hello"), "Hello", "only the two placeholders are swapped")
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

    // MARK: - ADR-0042 Sprint 4 — push_gid reverse-map (route-first, legacy fallback)

    func testPushGidWakeResolvesViaReverseIndexAndDecrypts() async {
        // The wake's `g` is an opaque push_gid; the reverse index maps it to
        // the REAL (account, group_id). The legacy resolve must never be
        // consulted on a hit — `account: nil` proves it is not needed.
        let group = FakeThrowawayGroup(decode: { _ in self.chat("routed hello") })
        let deps = FakeDeps(
            account: nil,
            group: group,
            rows: [row(10, 1)],
            route: (accountIdB64: accountId, groupIdHex: groupHex)
        )
        let preview = await decryptPushPreview(NsePushWake(groupHex: pushGidHex, pointer: 11), deps)

        XCTAssertEqual(preview?.body, "routed hello")
        XCTAssertEqual(preview?.tag, groupHex, "tag is the REAL group hex, not the push_gid")
        XCTAssertEqual(deps.lookupCount, 1, "reverse index consulted first")
        XCTAssertEqual(deps.lastLookupPushGid, pushGidHex)
        XCTAssertEqual(deps.resolveCount, 0, "route hit never consults the legacy resolve")
        XCTAssertEqual(deps.lastFetchGroupIdBytes, groupIdBytes, "DS fetch uses the REAL group_id")
        XCTAssertEqual(deps.lastCursorGroupHex, groupHex, "cursor read keyed on the REAL group hex")
        XCTAssertTrue(group.closed, "throwaway group closed on the routed path too")
        XCTAssertFalse(group.persistAttempted, "no persist entry point on the routed path")
    }

    func testRouteMissFallsBackToLegacyRawGroupIdPath() async {
        // A legacy wake: `g` IS the raw group_id and the reverse index has
        // no row for it — the pre-ADR-0042 path runs byte-for-byte.
        let group = FakeThrowawayGroup(decode: { _ in self.chat("legacy") })
        let deps = FakeDeps(account: okAccount(), group: group, rows: [row(10, 1)], route: nil)
        let preview = await decryptPushPreview(NsePushWake(groupHex: groupHex, pointer: 11), deps)

        XCTAssertEqual(preview?.body, "legacy")
        XCTAssertEqual(preview?.tag, groupHex)
        XCTAssertEqual(deps.lookupCount, 1, "reverse index consulted first even for a legacy wake")
        XCTAssertEqual(deps.resolveCount, 1, "miss falls back to the raw-group_id resolve")
        XCTAssertEqual(deps.lastCursorGroupHex, groupHex)
    }

    func testMalformedRouteRowFallsBackToLegacy() async {
        // A corrupt plaintext row (wrong-length / non-hex group id, empty
        // account) must DEGRADE to the legacy path, never crash / mis-route.
        let badRoutes: [(accountIdB64: String, groupIdHex: String)] = [
            (accountIdB64: accountId, groupIdHex: "abc"),
            (accountIdB64: accountId, groupIdHex: String(repeating: "z", count: 64)),
            (accountIdB64: accountId, groupIdHex: ""),
            (accountIdB64: "", groupIdHex: groupHex),
        ]
        for badRoute in badRoutes {
            let group = FakeThrowawayGroup(decode: { _ in self.chat("survivor") })
            let deps = FakeDeps(
                account: okAccount(), group: group, rows: [row(10, 1)], route: badRoute
            )
            let preview = await decryptPushPreview(
                NsePushWake(groupHex: groupHex, pointer: 11), deps
            )
            XCTAssertEqual(
                preview?.body, "survivor",
                "bad route row \(badRoute) must degrade to the legacy path"
            )
            XCTAssertEqual(deps.resolveCount, 1, "legacy resolve ran for \(badRoute)")
        }
    }

    func testRouteMissAndUnknownLegacyGroupYieldGeneric() async {
        // Neither the reverse index nor the legacy known-groups match →
        // nil → the generic banner (fail-closed), with no group opened.
        let deps = FakeDeps(account: nil, route: nil)
        let preview = await decryptPushPreview(NsePushWake(groupHex: pushGidHex, pointer: 5), deps)

        XCTAssertNil(preview)
        XCTAssertEqual(deps.lookupCount, 1)
        XCTAssertEqual(deps.resolveCount, 1)
        XCTAssertEqual(deps.openCount, 0, "no throwaway group on a double miss")
        XCTAssertEqual(deps.dsFetchCount, 0, "no DS fetch on a double miss")
    }

    func testArmedShortCircuitsBeforeReverseIndexLookup() async {
        // The ADR-0041 Finding-1 armed gate stays the very FIRST step — it
        // must precede even the reverse-index read.
        let group = FakeThrowawayGroup(decode: { _ in self.chat("secret") })
        let deps = FakeDeps(
            account: okAccount(),
            group: group,
            rows: [row(10, 1)],
            route: (accountIdB64: accountId, groupIdHex: groupHex),
            armed: true
        )
        let preview = await decryptPushPreview(NsePushWake(groupHex: pushGidHex, pointer: 12), deps)

        XCTAssertNil(preview)
        XCTAssertEqual(deps.lookupCount, 0, "armed gate precedes the reverse-index read")
        XCTAssertEqual(deps.resolveCount, 0)
        XCTAssertEqual(deps.dsFetchCount, 0)
    }

    func testUppercaseRouteGroupHexIsNormalizedAndDecrypts() async {
        // A row whose group hex was stored uppercase still routes — the core
        // lowercases before validating (defensive; the store writes lowercase).
        let group = FakeThrowawayGroup(decode: { _ in self.chat("normalized") })
        let deps = FakeDeps(
            account: nil,
            group: group,
            rows: [row(10, 1)],
            route: (accountIdB64: accountId, groupIdHex: groupHex.uppercased())
        )
        let preview = await decryptPushPreview(NsePushWake(groupHex: pushGidHex, pointer: 11), deps)

        XCTAssertEqual(preview?.body, "normalized")
        XCTAssertEqual(preview?.tag, groupHex, "tag normalized to lowercase")
        XCTAssertEqual(deps.lastFetchGroupIdBytes, groupIdBytes)
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

    // MARK: - ADR-0042 (amended 2026-09-30) — only the signed-in account

    private let otherAccount = "other-b64=="
    private let wakeDevice = String(repeating: "d", count: 64)

    func testNoBindingOrAMismatchedDShowsNothingAndReadsNothing() async {
        // `bound: .some(nil)` is the gate refusing: no binding, no `d`, or a
        // `d` that is not the bound pseudonym. Nothing behind it may run.
        let group = FakeThrowawayGroup(decode: { _ in self.chat("secret") })
        let deps = FakeDeps(
            account: okAccount(),
            group: group,
            rows: [row(10, 1)],
            route: (accountIdB64: accountId, groupIdHex: groupHex),
            bound: .some(nil)
        )
        let outcome = await decryptPushOutcome(
            NsePushWake(groupHex: pushGidHex, pointer: 11, devicePseudonymHex: wakeDevice), deps)

        XCTAssertEqual(outcome, .none, "a refused wake gets the generic line")
        XCTAssertEqual(deps.boundCount, 1)
        XCTAssertEqual(deps.lookupCount, 0, "no reverse-index read")
        XCTAssertEqual(deps.resolveCount, 0, "no known-groups read")
        XCTAssertTrue(deps.readAccounts.isEmpty, "no per-account store read")
        XCTAssertEqual(deps.openCount, 0)
        XCTAssertEqual(deps.dsFetchCount, 0)
    }

    func testTheGateIsHandedTheWakesDevicePseudonym() async {
        let group = FakeThrowawayGroup(decode: { _ in self.chat("hello") })
        let deps = FakeDeps(account: okAccount(), group: group, rows: [row(10, 1)])
        _ = await decryptPushOutcome(
            NsePushWake(groupHex: groupHex, pointer: 11, devicePseudonymHex: wakeDevice), deps)
        XCTAssertEqual(deps.lastWakeDevice, wakeDevice)
    }

    func testEveryReadIsForTheBoundAccount() async {
        let group = FakeThrowawayGroup(decode: { _ in self.chat("routed") })
        let deps = FakeDeps(
            account: nil,
            group: group,
            rows: [row(10, 1)],
            route: (accountIdB64: accountId, groupIdHex: groupHex)
        )
        let preview = await decryptPushPreview(
            NsePushWake(groupHex: pushGidHex, pointer: 11, devicePseudonymHex: wakeDevice), deps)

        XCTAssertEqual(preview?.body, "routed")
        XCTAssertEqual(deps.lastLookupAccount, accountId)
        XCTAssertEqual(Set(deps.readAccounts), [accountId], "seed, snapshot and cursor for the bound account")
    }

    func testARouteRowOfAnotherAccountIsAMiss() async {
        // Two accounts in one group derive the same push_gid. The row here was
        // written by the other account, and the bound account knows no group
        // by the wake's `g`: nothing is shown.
        let group = FakeThrowawayGroup(decode: { _ in self.chat("not yours") })
        let deps = FakeDeps(
            account: nil,
            group: group,
            rows: [row(10, 1)],
            route: (accountIdB64: otherAccount, groupIdHex: groupHex)
        )
        let outcome = await decryptPushOutcome(
            NsePushWake(groupHex: pushGidHex, pointer: 11, devicePseudonymHex: wakeDevice), deps)

        XCTAssertEqual(outcome, .none)
        XCTAssertEqual(deps.lastLookupAccount, accountId, "looked up for the bound account")
        XCTAssertEqual(deps.openCount, 0)
        XCTAssertEqual(deps.dsFetchCount, 0)
        XCTAssertFalse(deps.readAccounts.contains(otherAccount))
    }

    func testTheLegacyFallbackSearchesOnlyTheBoundAccount() async {
        // Another account on this device knows the raw group id; the bound
        // account does not. The legacy scan must not find it.
        let group = FakeThrowawayGroup(decode: { _ in self.chat("not yours") })
        let deps = FakeDeps(
            account: NseResolvedAccount(accountIdB64: otherAccount, groupIdBytes: groupIdBytes),
            group: group,
            rows: [row(10, 1)],
            route: nil
        )
        let outcome = await decryptPushOutcome(
            NsePushWake(groupHex: groupHex, pointer: 11, devicePseudonymHex: wakeDevice), deps)

        XCTAssertEqual(outcome, .none)
        XCTAssertEqual(deps.lastKnowsGroupAccount, accountId, "only the bound account was asked")
        XCTAssertEqual(deps.openCount, 0)
        XCTAssertFalse(deps.readAccounts.contains(otherAccount))
    }

    func testTheLegacyFallbackForTheBoundAccountStillPreviews() async {
        let group = FakeThrowawayGroup(decode: { _ in self.chat("legacy mine") })
        let deps = FakeDeps(
            account: okAccount(),
            group: group,
            rows: [row(10, 1)],
            route: (accountIdB64: otherAccount, groupIdHex: String(repeating: "c", count: 64))
        )
        let preview = await decryptPushPreview(
            NsePushWake(groupHex: groupHex, pointer: 11, devicePseudonymHex: wakeDevice), deps)

        XCTAssertEqual(preview?.body, "legacy mine")
        XCTAssertEqual(deps.lastFetchGroupIdBytes, groupIdBytes)
        XCTAssertEqual(Set(deps.readAccounts), [accountId])
    }
}
