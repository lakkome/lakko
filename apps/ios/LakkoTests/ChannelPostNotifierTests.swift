// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// `docs/ux/channel-notifications.md` — the iOS wiring of the shared rule:
/// the read kind, the candidates handed to the rule, the stored cursor, the
/// notification shown, and the background entry. The rule itself is the
/// real one (`channelPostsToNotify` through `LakkoCore`); it is tested in
/// Rust.
@MainActor
final class ChannelPostNotifierTests: XCTestCase {

    private let channel = String(repeating: "ab", count: 16)
    private let other = String(repeating: "cd", count: 16)
    private let now: UInt64 = 1_800_000_000_000
    private var account: String = ""
    private var defaults: UserDefaults!
    private var suiteName: String = ""

    override func setUp() {
        super.setUp()
        account = "acct-\(UUID().uuidString)"
        suiteName = "channel-notify-tests-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
        AccountStoreGate.open(account)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func post(_ cursor: UInt64, _ text: String = "hello", ageMs: UInt64 = 1_000) -> PersistedChannelPost {
        PersistedChannelPost(cursor: cursor, serverTsMs: Int64(now - ageMs), text: text)
    }

    private func facts(
        muted: Bool = false, open: Bool = false, own: Bool = false, lastNotified: UInt64 = 0
    ) -> ChannelNotify.Facts {
        ChannelNotify.Facts(muted: muted, openAndVisible: open, own: own, lastNotified: lastNotified, nowMs: now)
    }

    // MARK: - The read kind

    func testReadKindFollowsTheSlotAndTheMarker() {
        let slot = ChannelPostSlot(cursor: 3, posts: [])
        XCTAssertEqual(ChannelNotify.readKind(slotBefore: slot, readMarker: nil), .ongoing)
        XCTAssertEqual(ChannelNotify.readKind(slotBefore: slot, readMarker: 3), .ongoing)
        XCTAssertEqual(ChannelNotify.readKind(slotBefore: nil, readMarker: nil), .firstAfterSubscribe)
        XCTAssertEqual(ChannelNotify.readKind(slotBefore: nil, readMarker: 5), .history)
    }

    // MARK: - The candidates

    func testCandidatesAreTheNewPostsPlusOneNonPostAtTheNewCursor() {
        let read = ChannelRead(
            channelIdHex: channel,
            slotBefore: ChannelPostSlot(cursor: 10, posts: [post(9)]),
            slotAfter: ChannelPostSlot(cursor: 14, posts: [post(9), post(11), post(12)]),
            readMarker: 10)
        let (fresh, posts) = ChannelNotify.candidates(read: read, own: false)
        XCTAssertEqual(fresh.map(\.cursor), [11, 12])
        XCTAssertEqual(posts.map(\.cursor), [11, 12, 14])
        XCTAssertEqual(posts.map(\.isPost), [true, true, false])
    }

    func testNoNonPostRecordWhenTheNewestRecordIsAPost() {
        let read = ChannelRead(
            channelIdHex: channel,
            slotBefore: ChannelPostSlot(cursor: 10, posts: []),
            slotAfter: ChannelPostSlot(cursor: 12, posts: [post(11), post(12)]),
            readMarker: 10)
        XCTAssertEqual(ChannelNotify.candidates(read: read, own: false).posts.map(\.isPost), [true, true])
    }

    // MARK: - The decision, through the real rule

    private func ongoing(_ cursors: [UInt64], before: UInt64 = 10, marker: UInt64 = 10) -> ChannelRead {
        ChannelRead(
            channelIdHex: channel,
            slotBefore: ChannelPostSlot(cursor: before, posts: []),
            slotAfter: ChannelPostSlot(cursor: cursors.max() ?? before, posts: cursors.map { post($0, "p\($0)") }),
            readMarker: marker)
    }

    func testANewPostNotifiesTheNewestAndStoresTheCursor() {
        let d = ChannelNotify.decide(read: ongoing([11, 12]), facts: facts(lastNotified: 10))
        XCTAssertEqual(d.post?.cursor, 12)
        XCTAssertEqual(d.lastNotified, 12)
    }

    func testMutedOwnOrOpenNotifiesNothingButStillMovesTheCursor() {
        for f in [facts(muted: true), facts(open: true), facts(own: true)] {
            let d = ChannelNotify.decide(read: ongoing([11, 12]), facts: f)
            XCTAssertNil(d.post)
            XCTAssertEqual(d.lastNotified, 12)
        }
    }

    func testAPostAlreadyNotifiedIsNotNotifiedAgain() {
        let d = ChannelNotify.decide(read: ongoing([11, 12]), facts: facts(lastNotified: 12))
        XCTAssertNil(d.post)
        XCTAssertEqual(d.lastNotified, 12)
    }

    func testTheFirstReadAfterSubscribingNotifiesOnlyAFreshNewestPost() {
        let fresh = ChannelRead(
            channelIdHex: channel, slotBefore: nil,
            slotAfter: ChannelPostSlot(cursor: 3, posts: [post(1, ageMs: 10_000_000), post(3, ageMs: 60_000)]),
            readMarker: nil)
        XCTAssertEqual(ChannelNotify.decide(read: fresh, facts: facts()).post?.cursor, 3)

        let stale = ChannelRead(
            channelIdHex: channel, slotBefore: nil,
            slotAfter: ChannelPostSlot(cursor: 3, posts: [post(3, ageMs: 2 * 3_600_000)]),
            readMarker: nil)
        let d = ChannelNotify.decide(read: stale, facts: facts())
        XCTAssertNil(d.post)
        XCTAssertEqual(d.lastNotified, 3)
    }

    func testARefetchOfHistoryNotifiesNothing() {
        let read = ChannelRead(
            channelIdHex: channel, slotBefore: nil,
            slotAfter: ChannelPostSlot(cursor: 20, posts: [post(19), post(20)]),
            readMarker: 5)
        let d = ChannelNotify.decide(read: read, facts: facts())
        XCTAssertNil(d.post)
        XCTAssertEqual(d.lastNotified, 20)
    }

    func testTheBodyIsTheRowLineElseTheGenericLine() {
        XCTAssertEqual(ChannelNotify.body(post: post(1, " two\nlines "), generic: "G"), "two lines")
        XCTAssertEqual(ChannelNotify.body(post: post(1, "  "), generic: "G"), "G")
        XCTAssertEqual(ChannelNotify.tag(channelIdHex: channel.uppercased()), "channel:\(channel)")
    }

    // MARK: - The stored cursor

    func testTheStoreNeverMovesBackAndDropsChannelsNoLongerHeld() {
        let store = ChannelNotifiedStore(defaults: defaults)
        store.save(accountIdB64: account, channelIdHex: other, cursor: 4, keep: [other, channel])
        store.save(accountIdB64: account, channelIdHex: channel, cursor: 9, keep: [other, channel])
        store.save(accountIdB64: account, channelIdHex: channel, cursor: 5, keep: [other, channel])
        XCTAssertEqual(store.load(accountIdB64: account), [channel: 9, other: 4])
        store.save(accountIdB64: account, channelIdHex: channel, cursor: 10, keep: [channel])
        XCTAssertEqual(store.load(accountIdB64: account), [channel: 10])
        store.clear(accountIdB64: account)
        XCTAssertEqual(store.load(accountIdB64: account), [:])
    }

    func testTheStoreWritesNothingOnceTheAccountIsSealed() {
        let store = ChannelNotifiedStore(defaults: defaults)
        AccountStoreGate.seal(account)
        store.save(accountIdB64: account, channelIdHex: channel, cursor: 9, keep: [channel])
        XCTAssertEqual(store.load(accountIdB64: account), [:])
    }

    // MARK: - The notifier

    private final class Poster: ChannelPostNotificationPoster {
        var shown: [(String, String, String)] = []
        func post(channelIdHex: String, title: String, body: String) {
            shown.append((channelIdHex, title, body))
        }
    }

    private func notifier(muted: Set<String> = [], poster: Poster) -> ChannelPostNotifier {
        ChannelPostNotifier(
            deps: .init(
                muted: { muted.contains($0) },
                openAndVisible: { _ in false },
                own: { _ in false },
                title: { "Title \($0.prefix(2))" },
                held: { [self.channel, self.other] },
                nowMs: { self.now }),
            store: ChannelNotifiedStore(defaults: defaults),
            poster: poster)
    }

    func testTheNotifierShowsOnePerChannelAndStoresEveryCursor() {
        let poster = Poster()
        let n = notifier(muted: [other], poster: poster)
        let mutedRead = ChannelRead(
            channelIdHex: other, slotBefore: ChannelPostSlot(cursor: 1, posts: []),
            slotAfter: ChannelPostSlot(cursor: 3, posts: [post(2), post(3)]), readMarker: 1)
        n.handle(accountIdB64: account, reads: [ongoing([11, 12]), mutedRead])

        XCTAssertEqual(poster.shown.count, 1)
        XCTAssertEqual(poster.shown.first?.0, channel)
        XCTAssertEqual(poster.shown.first?.1, "Title ab")
        XCTAssertEqual(poster.shown.first?.2, "p12")
        XCTAssertEqual(
            ChannelNotifiedStore(defaults: defaults).load(accountIdB64: account), [channel: 12, other: 3])

        // The same read again (a second sync over the same records) shows
        // nothing: the stored cursor already covers it.
        n.handle(accountIdB64: account, reads: [ongoing([11, 12])])
        XCTAssertEqual(poster.shown.count, 1)
    }

    // MARK: - The sync hands its reads over

    /// A sync reports each channel it moved, with the slot before and after
    /// and the read marker as it stood BEFORE the sync (the slot-synced hook
    /// seeds markers during it), and only after the log is saved.
    func testTheSyncReportsEachReadAfterSavingTheLog() async throws {
        let channels = ChannelsUiState()
        channels.addChannel(ChannelHandle(id: channel, localName: "c", localDescription: "", addedAtMs: 1))
        channels.setSubscribedSeed(channelId: channel, epoch: 1, seed: Data(repeating: 7, count: 32))
        let body = Data(#"{"posts":[{"cursor":5,"server_ts_ms":1,"payload_b64":"AAAA"}],"next_after":5}"#.utf8)
        let client = ChannelsClient(
            baseURL: "https://channels.invalid", roles: ChannelPublisherRoleCache(),
            transport: { request in
                (body, HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: nil)!)
            })
        let store = ChannelPostLogStore()
        defer { store.delete(accountIdB64: account) }
        let sync = ChannelPostSync(accountIdB64: account, channels: channels, store: store, client: client)
        var markers: [String: UInt64] = [:]
        sync.channelReadMarkers = { markers }
        sync.onSlotSynced = { id, slot in markers[id] = slot.cursor }
        var reported: [ChannelRead] = []
        var savedWhenReported: ChannelPostSlot?
        sync.onChannelReads = { acct, reads in
            XCTAssertEqual(acct, self.account)
            reported.append(contentsOf: reads)
            savedWhenReported = store.get(accountIdB64: acct)?.channels[self.channel]
        }

        _ = await sync.syncAndWait()

        XCTAssertEqual(reported.count, 1)
        XCTAssertEqual(reported.first?.channelIdHex, channel)
        XCTAssertNil(reported.first?.slotBefore)
        XCTAssertEqual(reported.first?.slotAfter.cursor, 5)
        XCTAssertNil(reported.first?.readMarker, "the marker the slot hook seeded is not the one before")
        XCTAssertEqual(savedWhenReported?.cursor, 5)

        // A second sync over nothing new reports nothing.
        reported = []
        _ = await sync.syncAndWait()
        XCTAssertTrue(reported.isEmpty)
    }

    // MARK: - One writer of the log

    /// Counts the fetches of one client and, while closed, holds them.
    private final class FetchGate: @unchecked Sendable {
        private let lock = NSLock()
        private var closed: Bool
        private var count = 0
        init(closed: Bool) { self.closed = closed }
        var calls: Int { lock.withLock { count } }
        var isClosed: Bool { lock.withLock { closed } }
        func open() { lock.withLock { closed = false } }
        func enter() { lock.withLock { count += 1 } }
    }

    private func gatedClient(_ gate: FetchGate) -> ChannelsClient {
        let body = Data(#"{"posts":[{"cursor":5,"server_ts_ms":1,"payload_b64":"AAAA"}],"next_after":5}"#.utf8)
        return ChannelsClient(
            baseURL: "https://channels.invalid", roles: ChannelPublisherRoleCache(),
            transport: { request in
                gate.enter()
                while gate.isClosed { try await Task.sleep(nanoseconds: 10_000_000) }
                return (body, HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: nil)!)
            })
    }

    private func readableChannels() -> ChannelsUiState {
        let channels = ChannelsUiState()
        channels.addChannel(ChannelHandle(id: channel, localName: "c", localDescription: "", addedAtMs: 1))
        channels.setSubscribedSeed(channelId: channel, epoch: 1, seed: Data(repeating: 7, count: 32))
        return channels
    }

    /// A background run's own sync and the session's sync of the same
    /// account never write the log at once: the second waits until the first
    /// has saved, then reads the log it left, so the post the first one
    /// handed to the notifier is not handed over a second time.
    func testTwoSyncsOfOneProcessNeverRunAtOnce() async throws {
        let store = ChannelPostLogStore()
        defer { store.delete(accountIdB64: account) }
        let gateA = FetchGate(closed: true)
        let gateB = FetchGate(closed: false)
        let background = ChannelPostSync(
            accountIdB64: account, channels: readableChannels(), store: store, client: gatedClient(gateA))
        let session = ChannelPostSync(
            accountIdB64: account, channels: readableChannels(), store: store, client: gatedClient(gateB))
        var readsA: [ChannelRead] = []
        var readsB: [ChannelRead] = []
        background.channelReadMarkers = { [:] }
        session.channelReadMarkers = { [:] }
        background.onChannelReads = { _, reads in readsA += reads }
        session.onChannelReads = { _, reads in readsB += reads }

        let first = Task { await background.syncAndWait() }
        while gateA.calls == 0 { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(ChannelPostLogWriter.isHeld)
        let second = Task { await session.syncAndWait() }
        try await Task.sleep(nanoseconds: 300_000_000)
        XCTAssertEqual(gateB.calls, 0, "the session sync must not read while the background one holds the log")

        gateA.open()
        _ = await first.value
        _ = await second.value

        XCTAssertGreaterThan(gateB.calls, 0)
        XCTAssertEqual(readsA.count, 1)
        XCTAssertTrue(readsB.isEmpty, "the second sync reads the log the first saved")
        XCTAssertEqual(session.log.channels[channel]?.cursor, 5, "the session's published log follows the store")
        XCTAssertFalse(ChannelPostLogWriter.isHeld)
    }

    // MARK: - The background entry

    func testTheBackgroundRunNeedsAnAccount() async {
        var built = 0
        let ran = await ChannelPostBackgroundRefresh.run(.init(
            accountIdB64: { nil },
            activeSync: { nil },
            makeSync: { _ in built += 1; return ChannelPostSync(accountIdB64: "x", channels: ChannelsUiState()) }))
        XCTAssertFalse(ran)
        XCTAssertEqual(built, 0)
    }

    func testTheBackgroundRunBuildsASyncWhenNoSessionSyncServesTheAccount() async {
        var built: [String] = []
        let active = ChannelPostSync(accountIdB64: "someone-else", channels: ChannelsUiState())
        let ran = await ChannelPostBackgroundRefresh.run(.init(
            accountIdB64: { self.account },
            activeSync: { active },
            makeSync: { account in
                built.append(account)
                return ChannelPostSync(accountIdB64: account, channels: ChannelsUiState())
            }), budgetSeconds: 5)
        XCTAssertTrue(ran)
        XCTAssertEqual(built, [account])
    }

    func testTheBackgroundRunUsesTheSessionSyncOfTheSameAccount() async {
        var built = 0
        let active = ChannelPostSync(accountIdB64: account, channels: ChannelsUiState())
        let ran = await ChannelPostBackgroundRefresh.run(.init(
            accountIdB64: { self.account },
            activeSync: { active },
            makeSync: { _ in built += 1; return ChannelPostSync(accountIdB64: "x", channels: ChannelsUiState()) }),
            budgetSeconds: 5)
        XCTAssertTrue(ran)
        XCTAssertEqual(built, 0)
    }
    /// Anti-censorship on but not armed in this process: the run sends
    /// nothing, so no request goes around the tunnel.
    func testTheBackgroundRunSendsNothingAroundAnUnarmedTunnel() async {
        var built = 0
        let ran = await ChannelPostBackgroundRefresh.run(.init(
            accountIdB64: { self.account },
            activeSync: { nil },
            makeSync: { _ in built += 1; return ChannelPostSync(accountIdB64: "x", channels: ChannelsUiState()) },
            mayUseNetwork: { false }), budgetSeconds: 5)
        XCTAssertFalse(ran)
        XCTAssertEqual(built, 0)
    }
}
