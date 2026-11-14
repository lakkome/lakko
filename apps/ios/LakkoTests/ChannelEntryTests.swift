// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// MARK: - Fakes

/// What the head store and the scripted DS saw, in order, across threads.
final class EntryEventLog: @unchecked Sendable {
    static let shared = EntryEventLog()
    private let lock = NSLock()
    private var events: [String] = []

    func add(_ event: String) { lock.lock(); events.append(event); lock.unlock() }
    func all() -> [String] { lock.lock(); defer { lock.unlock() }; return events }
    func reset() { lock.lock(); events = []; lock.unlock() }
}

/// A head store in memory: writes at current + 1 or `409`, an unclaimed
/// vault answers a write `401`.
@MainActor
final class FakeChannelHeadStore: ChannelHeadStore {
    var claimed: Bool
    var version: UInt64 = 0
    var head: VaultConversationHead?
    var failWrites = false
    private(set) var writes: [(version: UInt64, head: VaultConversationHead)] = []
    private(set) var claims = 0

    init(claimed: Bool = true) { self.claimed = claimed }

    func read() async throws -> (version: UInt64, head: VaultConversationHead)? {
        guard let head else { return nil }
        return (version, head)
    }

    func write(version: UInt64, head: VaultConversationHead) async throws {
        EntryEventLog.shared.add("head")
        if failWrites { throw VaultClientError.network }
        guard claimed else { throw VaultClientError.unclaimed }
        guard version == self.version &+ 1 else { throw VaultClientError.conflict }
        self.version = version
        self.head = head
        writes.append((version, head))
    }

    func claim() async throws {
        claims += 1
        claimed = true
    }

    var failDelete = false
    var deleteDelaySeconds: Double = 0
    private(set) var deletes = 0

    func delete() async throws {
        if deleteDelaySeconds > 0 {
            try? await Task.sleep(nanoseconds: UInt64(deleteDelaySeconds * 1_000_000_000))
        }
        if failDelete { throw VaultClientError.http(status: 503, code: nil) }
        deletes += 1
        EntryEventLog.shared.add("delete")
    }
}

/// The DS commit route answering `409` (another owner device's commit owns
/// the epoch), recording that a commit was sent.
final class EntryDsConflictURLProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard let url = request.url else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        if url.path.hasSuffix("/commit") { EntryEventLog.shared.add("commit") }
        let response = HTTPURLResponse(url: url, statusCode: 409, httpVersion: "HTTP/1.1",
                                       headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(#"{"error":"epoch-taken"}"#.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

// MARK: - Tests

/// ADR-0064 section 7, amendment of 2026-11-14: the channel vault head the
/// owner devices keep, and the entry of an owner device that holds no group.
@MainActor
final class ChannelEntryTests: XCTestCase {

    private let channel = Data(repeating: 0x2c, count: 16)
    private var channelHex: String { channel.map { String(format: "%02x", $0) }.joined() }

    private func reentry(_ epoch: UInt64, _ byte: UInt8 = 1) -> VaultReentryEpoch {
        VaultReentryEpoch(epoch: epoch, dsAccessSeed: Data(repeating: byte, count: 32), groupInfo: Data([byte, 9]))
    }

    override func setUp() async throws {
        ChannelVaultHead.reset()
        ChannelEntry.reset()
        EntryEventLog.shared.reset()
    }

    override func tearDown() async throws {
        ChannelVaultHead.storeFor = { LiveChannelHeadStore(channelIdHex: $0) }
        ChannelVaultHead.reset()
        ChannelEntry.reset()
    }

    // MARK: pure rules

    func testMergeKeepsTheTwoNewestEpochsAndTheNewerCopyOfOne() {
        let merged = ChannelVaultHead.mergeReentry(
            [reentry(5), reentry(6, 1)], [reentry(7), reentry(6, 2)], keep: 2)
        XCTAssertEqual(merged.map(\.epoch), [7, 6])
        XCTAssertEqual(merged[1].groupInfo, Data([2, 9]), "the added copy of epoch 6 wins")
        XCTAssertEqual(ChannelVaultHead.mergeReentry([reentry(9)], [reentry(3)], keep: 2).map(\.epoch), [9, 3])
        XCTAssertEqual(ChannelVaultHead.keptEpochs, 2)
    }

    func testOnlyAnOwnerAccountDeviceWithoutTheGroupEnters() {
        XCTAssertTrue(ChannelEntry.shouldEnter(accountRole: ChannelAccountRole.owner, coPublished: false, holdsGroup: false))
        XCTAssertFalse(ChannelEntry.shouldEnter(accountRole: ChannelAccountRole.coPublisher, coPublished: true, holdsGroup: false),
                       "a co-publisher's device is admitted by an owner device, never enters")
        XCTAssertFalse(ChannelEntry.shouldEnter(accountRole: ChannelAccountRole.owner, coPublished: true, holdsGroup: false))
        XCTAssertFalse(ChannelEntry.shouldEnter(accountRole: ChannelAccountRole.owner, coPublished: false, holdsGroup: true),
                       "a device that holds the group never enters over it")
        XCTAssertFalse(ChannelEntry.shouldEnter(accountRole: nil, coPublished: false, holdsGroup: false))
    }

    func testAPollThatMovedTheEpochOwesTheHead() {
        XCTAssertTrue(ChannelPublisherLoops.headOwed(before: 4, after: 5))
        XCTAssertFalse(ChannelPublisherLoops.headOwed(before: 5, after: 5))
        XCTAssertFalse(ChannelPublisherLoops.headOwed(before: nil, after: 5))
    }

    // MARK: entry order

    func testEntryTriesTheNewestSavedEpochFirstAndFallsBackOnALoss() async {
        var tried: [UInt64] = []
        let outcome = await ChannelEntry.enter(saved: [reentry(3), reentry(4)]) { entry in
            tried.append(entry.epoch)
            return entry.epoch == 4 ? .lost : .landed
        }
        XCTAssertEqual(tried, [4, 3])
        XCTAssertEqual(outcome, .entered(epoch: 3))
    }

    func testEntryWaitsWithNoHeadAndWhenEveryEpochIsRefused() async {
        var called = false
        let none = await ChannelEntry.enter(saved: []) { _ in called = true; return .landed }
        XCTAssertEqual(none, .noHead)
        XCTAssertFalse(called)
        let refused = await ChannelEntry.enter(saved: [reentry(1), reentry(2)]) { _ in .lost }
        XCTAssertEqual(refused, .allRefused)
        let unread = await ChannelEntry.enter(saved: nil) { _ in .landed }
        XCTAssertEqual(unread, .unfinished, "an unreadable head is tried again, it is not a fallback")
    }

    func testAnUnansweredEntryStopsWithoutTryingTheOlderEpoch() async {
        var tried: [UInt64] = []
        let outcome = await ChannelEntry.enter(saved: [reentry(8), reentry(7)]) { entry in
            tried.append(entry.epoch)
            return .unfinished
        }
        XCTAssertEqual(tried, [8])
        XCTAssertEqual(outcome, .unfinished)
    }

    // MARK: the writer

    func testAConflictReadsMergesAndWritesAgainKeepingTheTwoNewest() async {
        let store = FakeChannelHeadStore()
        store.version = 3
        var held = ChannelVaultHead.head(channelId: channel, reentry: [reentry(5), reentry(6)], nowMs: 1)
        held.title = "carried by nobody"
        store.head = held
        let ok = await ChannelVaultHead.write(channelIdHex: channelHex, entry: reentry(7), store: store)
        XCTAssertTrue(ok)
        XCTAssertEqual(store.version, 4)
        XCTAssertEqual(store.head?.reentry.map(\.epoch), [7, 6])
        XCTAssertNil(store.head?.registrarSeed)
        XCTAssertEqual(store.head?.title, "")
        // Known now: the next epoch is written at the next version at once.
        let events = EntryEventLog.shared.all().count
        let wroteNext = await ChannelVaultHead.write(channelIdHex: channelHex, entry: reentry(8), store: store)
        XCTAssertTrue(wroteNext)
        XCTAssertEqual(EntryEventLog.shared.all().count, events + 1)
        XCTAssertEqual(store.head?.reentry.map(\.epoch), [8, 7])
    }

    func testTheFirstWriteClaimsTheVault() async {
        let store = FakeChannelHeadStore(claimed: false)
        let wrote = await ChannelVaultHead.write(channelIdHex: channelHex, entry: reentry(0), store: store)
        XCTAssertTrue(wrote)
        XCTAssertEqual(store.claims, 1)
        XCTAssertEqual(store.version, 1)
    }

    func testAFailedWriteReportsFailureAndThrowsNothing() async {
        let store = FakeChannelHeadStore()
        store.failWrites = true
        let ok = await ChannelVaultHead.write(channelIdHex: channelHex, entry: reentry(2), store: store)
        XCTAssertFalse(ok)
        XCTAssertEqual(EntryEventLog.shared.all(), ["head"], "one attempt, no retry loop")
    }

    // MARK: real core

    /// An owner device with nothing but the saved GroupInfo enters, the
    /// existing device applies the entry, both derive one seed, and the
    /// head written after the applied commit carries the new epoch.
    func testAnOwnerDeviceEntersFromTheSavedGroupInfo() async throws {
        let accountSeed = Data(repeating: 0x61, count: 32)
        let a = try Identity.fromSeedBound(
            seed: Data(repeating: 0x62, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let b = try Identity.fromSeedBound(
            seed: Data(repeating: 0x63, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let group = try PublisherGroup.create(identity: a, ownerAccountSeed: accountSeed, channelId: channel)
        let saved = VaultReentryEpoch(
            epoch: try group.epoch(), dsAccessSeed: try group.dsAccessSeed(identity: a),
            groupInfo: try group.groupInfo(identity: a))

        // The head round-trips through the core's codec.
        let head = ChannelVaultHead.head(channelId: channel, reentry: [saved], nowMs: 5)
        XCTAssertEqual(try decodeConversationHead(bytes: try encodeConversationHead(head: head)).reentry, [saved])

        let entered = try publisherGroupEnter(
            identity: b, groupInfo: saved.groupInfo, ownerAccountSeed: accountSeed, channelId: channel,
            dsAccessSeed: saved.dsAccessSeed)
        // G-55: the row the Delivery Service stores names no account.
        let ownerKey = try publisherOwnerAccountKey(ownerAccountSeed: accountSeed)
        XCTAssertNil(entered.commit.range(of: ownerKey), "no account key in the entry row")
        XCTAssertTrue(try entered.group.hasStagedCommit())
        XCTAssertEqual(try entered.group.epoch(), saved.epoch, "the epoch the entry leaves")
        let staged = try XCTUnwrap(try entered.group.stagedChannelSeed(identity: b))
        _ = try group.processIncomingVerifyingRoles(identity: a, bytes: entered.commit)
        XCTAssertTrue(try entered.group.mergeStagedCommit(identity: b))
        XCTAssertEqual(try group.channelSeed(identity: a), try entered.group.channelSeed(identity: b))
        XCTAssertEqual(staged, try entered.group.channelSeed(identity: b))
        XCTAssertTrue(try entered.group.isOwnerDevice(identity: b))

        // Device A applied B's commit: it writes the current epoch.
        let store = FakeChannelHeadStore()
        ChannelVaultHead.storeFor = { _ in store }
        let wroteCurrent = await ChannelVaultHead.writeCurrent(channelIdHex: channelHex, group: group, identity: a)
        XCTAssertTrue(wroteCurrent)
        XCTAssertEqual(store.head?.reentry.first?.epoch, saved.epoch + 1)
        XCTAssertEqual(store.head?.reentry.first?.dsAccessSeed, try group.dsAccessSeed(identity: a))
    }

    /// A co-publisher's device writes no head even when it holds the group.
    func testACoPublisherDeviceWritesNoHead() async throws {
        let ownerSeed = Data(repeating: 0x64, count: 32)
        let copubSeed = Data(repeating: 0x65, count: 32)
        let owner = try Identity.fromSeedBound(
            seed: Data(repeating: 0x66, count: 32), accountSeed: ownerSeed, issuedAtUnixSecs: 1_700_000_000)
        let copub = try Identity.fromSeedBound(
            seed: Data(repeating: 0x67, count: 32), accountSeed: copubSeed, issuedAtUnixSecs: 1_700_000_000)
        let group = try PublisherGroup.create(identity: owner, ownerAccountSeed: ownerSeed, channelId: channel)
        let request = try group.retainJoinRequest(identity: owner, keyPackage: try copub.createKeyPackage())
        let staged = try group.stageApproveExternalJoin(identity: owner, proposalRef: request.joinerSignatureKey)
        XCTAssertTrue(try group.mergeStagedCommit(identity: owner))
        let joined = try publisherGroupJoinFromWelcome(
            identity: copub, welcome: try XCTUnwrap(staged.welcome), groupId: try group.groupId(),
            channelId: channel, ownerAccountKey: try publisherOwnerAccountKey(ownerAccountSeed: ownerSeed))
        let store = FakeChannelHeadStore()
        ChannelVaultHead.storeFor = { _ in store }
        let copubWrote = await ChannelVaultHead.writeCurrent(channelIdHex: channelHex, group: joined, identity: copub)
        XCTAssertFalse(copubWrote)
        XCTAssertTrue(store.writes.isEmpty)
        // A device that refuses to enter from a GroupInfo of a group another
        // account owns: the core refuses the co-publisher's entry.
        XCTAssertThrowsError(try publisherGroupEnter(
            identity: copub, groupInfo: try group.groupInfo(identity: owner),
            ownerAccountSeed: copubSeed, channelId: channel,
            dsAccessSeed: try group.dsAccessSeed(identity: owner)))
    }

    // MARK: the order inside a staged commit

    private func withBoundOwner(
        _ body: (_ account: String, _ seed: Data, _ device: Identity) async throws -> Void
    ) async throws {
        let account = "entry-\(UUID().uuidString)"
        let accountSeed = Data((0..<32).map { _ in UInt8.random(in: 0...255) })
        let device = try Identity.fromSeedBound(
            seed: Data((0..<32).map { _ in UInt8.random(in: 0...255) }), accountSeed: accountSeed,
            issuedAtUnixSecs: 1_700_000_000)
        let previousBound = AcceptedContactsStore.boundAccountIdB64
        let previousProvider = ChannelPublisherIdentity.shared.deviceIdentityProvider
        let previousPersister = ChannelPublisherIdentity.shared.devicePersister
        AcceptedContactsStore.bind(accountIdB64: account)
        ChannelPublisherIdentity.shared.unbind()
        ChannelPublisherIdentity.shared.deviceIdentityProvider = { device }
        ChannelPublisherIdentity.shared.devicePersister = {}
        defer {
            PublisherCommitStore.deleteAll(accountIdB64: account)
            ChannelPublisherIdentity.shared.deviceIdentityProvider = previousProvider
            ChannelPublisherIdentity.shared.devicePersister = previousPersister
            ChannelPublisherIdentity.shared.unbind()
            AcceptedContactsStore.bind(accountIdB64: previousBound)
        }
        try await body(account, accountSeed, device)
    }

    private func rotationContext(account: String, seed: Data) -> ChannelPublisherCommits.Context {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [EntryDsConflictURLProtocol.self]
        return ChannelPublisherCommits.Context(
            channelIdHex: channelHex, currentSeed: seed, accountIdB64: account,
            ds: DsClient(baseURL: "https://ds.entry.test", session: URLSession(configuration: config)))
    }

    private func stageRotation(_ context: ChannelPublisherCommits.Context) async throws -> Bool {
        try await ChannelPublisherCommits.stageAndRun(
            context, state: nil, kind: .rotation, joinerKey: nil, serverTargetEpoch: 2, serverEpoch: 1
        ) { identity, group in
            let staged = try group.stageCommit(identity: identity, admit: [], retire: [], rotate: true)
            return .init(commit: staged.commit, welcome: staged.welcome)
        }.landed
    }

    /// The head carries the epoch a staged commit moves to before the commit
    /// is sent; the commit lost here (`409`) and was abandoned.
    func testTheHeadIsWrittenBeforeTheCommitIsSent() async throws {
        try await withBoundOwner { account, seed, device in
            let group = try PublisherGroup.create(identity: device, ownerAccountSeed: seed, channelId: self.channel)
            let epoch = try group.epoch()
            let store = FakeChannelHeadStore()
            ChannelVaultHead.storeFor = { _ in store }
            let landed = try await self.stageRotation(self.rotationContext(account: account, seed: seed))
            XCTAssertFalse(landed)
            let events = EntryEventLog.shared.all()
            XCTAssertEqual(events.first, "head")
            XCTAssertTrue(events.contains("commit"))
            XCTAssertEqual(store.head?.reentry.first?.epoch, epoch + 1)
            let reloaded = try XCTUnwrap(try publisherGroupLoad(
                identity: device, ownerAccountSeed: seed, channelId: self.channel))
            XCTAssertFalse(try reloaded.hasStagedCommit(), "the lost commit was abandoned")
            XCTAssertEqual(try reloaded.epoch(), epoch)
        }
    }

    /// A head write that fails does not keep the commit from being sent.
    func testAFailedHeadWriteDoesNotBlockTheCommit() async throws {
        try await withBoundOwner { account, seed, device in
            _ = try PublisherGroup.create(identity: device, ownerAccountSeed: seed, channelId: self.channel)
            let store = FakeChannelHeadStore()
            store.failWrites = true
            ChannelVaultHead.storeFor = { _ in store }
            _ = try await self.stageRotation(self.rotationContext(account: account, seed: seed))
            XCTAssertEqual(EntryEventLog.shared.all().filter { $0 == "head" }.count, 1)
            XCTAssertTrue(EntryEventLog.shared.all().contains("commit"))
        }
    }

    /// The live entry from a head with two saved epochs, where the DS takes
    /// neither commit (`409`): each attempt is recorded, sent, abandoned, and
    /// the older epoch tried; nothing is left held and the device falls back.
    func testTheLiveEntryFallsBackToTheOlderEpochAndThenWaits() async throws {
        try await withBoundOwner { account, seed, device in
            let other = try Identity.fromSeedBound(
                seed: Data(repeating: 0x68, count: 32), accountSeed: seed, issuedAtUnixSecs: 1_700_000_000)
            let group = try PublisherGroup.create(identity: other, ownerAccountSeed: seed, channelId: self.channel)
            let first = VaultReentryEpoch(
                epoch: try group.epoch(), dsAccessSeed: try group.dsAccessSeed(identity: other),
                groupInfo: try group.groupInfo(identity: other))
            _ = try group.stageCommit(identity: other, admit: [], retire: [], rotate: true)
            XCTAssertTrue(try group.mergeStagedCommit(identity: other))
            let second = VaultReentryEpoch(
                epoch: try group.epoch(), dsAccessSeed: try group.dsAccessSeed(identity: other),
                groupInfo: try group.groupInfo(identity: other))
            let store = FakeChannelHeadStore()
            store.version = 1
            store.head = ChannelVaultHead.head(channelId: self.channel, reentry: [first, second], nowMs: 1)
            ChannelVaultHead.storeFor = { _ in store }
            let channels = ChannelsClient(
                baseURL: "https://channels.entry.test", roles: .shared,
                transport: { request in
                    let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: "HTTP/1.1",
                                                   headerFields: ["Content-Type": "application/json"])!
                    return (Data(#"{"epoch":3}"#.utf8), response)
                })
            let config = URLSessionConfiguration.ephemeral
            config.protocolClasses = [EntryDsConflictURLProtocol.self]
            let state = ChannelsUiState(accountIdB64: account)
            // The live attempt runs over the context's own DS client; the
            // stub answers the commit route `409`.
            let previous = ChannelEntry.dsClient
            ChannelEntry.dsClient = { DsClient(baseURL: "https://ds.entry.test", session: URLSession(configuration: config)) }
            defer { ChannelEntry.dsClient = previous }
            let result = await ChannelEntry.enterIfPossible(
                channelId: self.channelHex, state: state, client: channels, seed: seed, accountIdB64: account)
            XCTAssertFalse(result.entered)
            XCTAssertTrue(result.fallBack, "every saved epoch refused: the device waits (or decision A)")
            XCTAssertEqual(EntryEventLog.shared.all().filter { $0 == "commit" }.count, 4,
                           "two epochs, each sent and re-sent once on 409")
            XCTAssertNil(try publisherGroupLoad(identity: device, ownerAccountSeed: seed, channelId: self.channel),
                         "an abandoned entry leaves nothing held")
            XCTAssertTrue(PublisherCommitStore.shared.entries(accountIdB64: account).isEmpty)
            XCTAssertNil(state.keysByChannel[self.channelHex])
            // Backed off: the next pass does not read the head again at once.
            let again = await ChannelEntry.enterIfPossible(
                channelId: self.channelHex, state: state, client: channels, seed: seed, accountIdB64: account)
            XCTAssertTrue(again.fallBack)
            XCTAssertEqual(EntryEventLog.shared.all().filter { $0 == "commit" }.count, 4)
        }
    }

    /// An epoch older than both saved ones is not written, and the write
    /// says so: before and after a `409` read.
    func testAnEpochOlderThanBothSavedOnesIsNotWritten() async {
        let store = FakeChannelHeadStore()
        store.version = 2
        store.head = ChannelVaultHead.head(channelId: channel, reentry: [reentry(40), reentry(41)], nowMs: 1)
        // Unknown version: the first try is at 1, the 409 read finds 40 / 41.
        let afterRead = await ChannelVaultHead.write(channelIdHex: channelHex, entry: reentry(5), store: store)
        XCTAssertFalse(afterRead)
        XCTAssertEqual(store.version, 2, "nothing written")
        XCTAssertEqual(store.head?.reentry.map(\.epoch), [40, 41])
        // Known head (from that read): nothing is sent at all.
        EntryEventLog.shared.reset()
        let known = await ChannelVaultHead.write(channelIdHex: channelHex, entry: reentry(6), store: store)
        XCTAssertFalse(known)
        XCTAssertTrue(EntryEventLog.shared.all().isEmpty)
        XCTAssertTrue(store.writes.isEmpty)
    }

    // MARK: security pass

    /// Decision A's re-founded group starts at a low epoch: its head write
    /// replaces the old group's epochs instead of merging with them.
    func testAReFoundWriteReplacesTheDeadGroupsEpochs() async {
        let store = FakeChannelHeadStore()
        store.version = 3
        store.head = ChannelVaultHead.head(channelId: channel, reentry: [reentry(40), reentry(41)], nowMs: 1)
        let ok = await ChannelVaultHead.write(channelIdHex: channelHex, entry: reentry(0, 7), store: store, replace: true)
        XCTAssertTrue(ok)
        XCTAssertEqual(store.version, 4, "at current + 1")
        XCTAssertEqual(store.head?.reentry, [reentry(0, 7)], "only the new group's epoch")
        // A merge (an ordinary write) would have kept 41 above it.
        XCTAssertEqual(ChannelVaultHead.mergeReentry([reentry(40), reentry(41)], [reentry(0)]).map(\.epoch), [41, 40])
    }

    /// Applying another owner device's entry runs the own-device reconcile
    /// in the same pass; a commit that adds no owner leaf, or a device that
    /// is not the owner's, does not.
    func testApplyingAnEntryReconcilesTheOwnDevicesAtOnce() async throws {
        let accountSeed = Data(repeating: 0x81, count: 32)
        let a = try Identity.fromSeedBound(
            seed: Data(repeating: 0x82, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let b = try Identity.fromSeedBound(
            seed: Data(repeating: 0x83, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let group = try PublisherGroup.create(identity: a, ownerAccountSeed: accountSeed, channelId: channel)
        let entered = try publisherGroupEnter(
            identity: b, groupInfo: try group.groupInfo(identity: a), ownerAccountSeed: accountSeed, channelId: channel,
            dsAccessSeed: try group.dsAccessSeed(identity: a))
        let before = ChannelDeviceAdmission.leafAccounts(group)
        _ = try group.processIncomingVerifyingRoles(identity: a, bytes: entered.commit)

        var reconciled: [Data] = []
        let previous = ChannelDeviceAdmission.reconcileNow
        ChannelDeviceAdmission.reconcileNow = { _, _, identity, _ in reconciled.append(identity.signaturePublicKey()) }
        defer { ChannelDeviceAdmission.reconcileNow = previous }
        let context = ChannelPublisherCommits.Context(
            channelIdHex: channelHex, currentSeed: accountSeed, accountIdB64: "entry-reconcile")
        let state = ChannelsUiState(accountIdB64: "entry-reconcile-\(UUID().uuidString)")
        let ran = await ChannelDeviceAdmission.afterApplied(
            before: before, context: context, state: state, identity: a, group: group)
        XCTAssertTrue(ran)
        XCTAssertEqual(reconciled, [a.signaturePublicKey()])

        // Nothing added since: no reconcile.
        let again = await ChannelDeviceAdmission.afterApplied(
            before: ChannelDeviceAdmission.leafAccounts(group), context: context, state: state, identity: a, group: group)
        XCTAssertFalse(again)
        XCTAssertEqual(reconciled.count, 1)
        XCTAssertFalse(ChannelDeviceAdmission.addedOwnerLeaves(
            before: [(leaf: Data([1]), account: Data([9]))],
            after: [(leaf: Data([1]), account: Data([9])), (leaf: Data([2]), account: Data([8]))],
            ownerAccountKey: Data([9])), "a new leaf of another account is not an owner entry")
    }

    // MARK: deleting vaults

    /// The vault delete is `DELETE /vault/<id>` signed under the lineage
    /// identity's vault write key over an empty body; a missing vault is
    /// nothing to delete, a server failure is reported.
    func testTheVaultDeleteIsASignedDeleteOfTheVault() async throws {
        let lineage = try Identity.fromSeed(seed: Data(repeating: 0x71, count: 32))
        var sent: [URLRequest] = []
        var status = 204
        let client = VaultClient(baseURL: "https://storage.entry.test", tokens: AnonTokenPool.shared) { request in
            sent.append(request)
            return (Data(), HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1",
                                            headerFields: nil)!)
        }
        let store = try XCTUnwrap(LiveChannelHeadStore(channelIdHex: channelHex, client: client, lineage: lineage))
        try await store.delete()
        let request = try XCTUnwrap(sent.first)
        let vaultHex = hexEncode(try vaultKeys(accountIdentity: lineage).vaultId(
            vault: ChannelVaultHead.vaultRef(channelId: channel)))
        XCTAssertEqual(request.httpMethod, "DELETE")
        XCTAssertEqual(request.url?.absoluteString, "https://storage.entry.test/vault/\(vaultHex)")
        XCTAssertNil(request.httpBody)
        XCTAssertFalse((request.value(forHTTPHeaderField: storageProofHeaderName()) ?? "").isEmpty)
        status = 404
        try await store.delete()
        status = 500
        do {
            try await store.delete()
            XCTFail("a server failure is reported")
        } catch {}
    }

    /// Channel delete: the vault delete is best effort and never throws.
    func testAFailedVaultDeleteAfterAChannelDeleteIsOnlyReported() async {
        let store = FakeChannelHeadStore()
        ChannelVaultHead.storeFor = { _ in store }
        let ok = await ChannelVaultHead.deleteVault(channelIdHex: channelHex, currentSeed: nil)
        XCTAssertTrue(ok)
        XCTAssertEqual(store.deletes, 1)
        store.failDelete = true
        let failed = await ChannelVaultHead.deleteVault(channelIdHex: channelHex, currentSeed: nil)
        XCTAssertFalse(failed)
    }

    /// Account deletion: the vault of every owned channel, never one the
    /// account left or co-publishes; a store that hangs costs at most the
    /// budget.
    func testAccountDeletionDeletesTheOwnedVaultsWithinItsBudget() async {
        let owned = Data(repeating: 0x31, count: 16)
        let left = Data(repeating: 0x32, count: 16)
        let copub = Data(repeating: 0x33, count: 16)
        let registers: [(channelId: Data, subscribed: Bool, owner: Bool)] = [
            (owned, true, true), (left, false, true), (copub, true, false),
        ]
        XCTAssertEqual(ChannelVaultHead.ownedChannelIds(registers: registers, extras: [:]), [hexEncode(owned)])
        var extras = ChannelExtras()
        extras.accountRole = ChannelAccountRole.owner
        XCTAssertEqual(ChannelVaultHead.ownedChannelIds(registers: [], extras: ["AB": extras]), ["ab"])

        var asked: [String] = []
        let store = FakeChannelHeadStore()
        ChannelVaultHead.storeFor = { id in asked.append(id); return store }
        let account = "vault-delete-\(UUID().uuidString)"
        let deleted = await ChannelVaultHead.deleteOwnedVaults(
            accountIdB64: account, currentSeed: nil, registers: registers)
        XCTAssertEqual(deleted, 1)
        XCTAssertEqual(asked, [hexEncode(owned)])

        let slow = FakeChannelHeadStore()
        slow.deleteDelaySeconds = 5
        ChannelVaultHead.storeFor = { _ in slow }
        let start = Date()
        let none = await ChannelVaultHead.deleteOwnedVaults(
            accountIdB64: account, currentSeed: nil, budgetSeconds: 0.3, registers: registers)
        XCTAssertEqual(none, 0)
        XCTAssertLessThan(Date().timeIntervalSince(start), 3, "a slow storage service cannot hold the deletion")
        let nothing = await ChannelVaultHead.deleteOwnedVaults(
            accountIdB64: account, currentSeed: nil, registers: [])
        XCTAssertEqual(nothing, 0, "an account that owns no channel (a guest) sends nothing")
    }

    // MARK: wire

    func testTheObjectBodyIsTheBigEndianVersionThenTheSealedObject() {
        let body = VaultClient.objectBody(version: 0x0102_0304_0506_0708, sealed: Data([0xaa, 0xbb]))
        XCTAssertEqual([UInt8](body), [1, 2, 3, 4, 5, 6, 7, 8, 0xaa, 0xbb])
        let split = VaultClient.splitObject(body)
        XCTAssertEqual(split?.version, 0x0102_0304_0506_0708)
        XCTAssertEqual(split?.sealed, Data([0xaa, 0xbb]))
        XCTAssertNil(VaultClient.splitObject(Data(count: 8)))
        XCTAssertEqual(VaultClient.headPath("ab"), "/vault/ab/head")
    }
}
