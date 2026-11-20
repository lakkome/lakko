// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest

@testable import Lakko

/// ADR-0099 step 5 — a channel moderators removed (`410 channel-removed`):
/// nothing ends, the posts held stay readable, write controls go, the line
/// says "removed" only after the log verified it (C-98), the service is asked
/// once per standing period, and a restore leaves no trace. Also the
/// `403 hidden-from-search` state of Who can find me.
@MainActor
final class ChannelRemovalTests: XCTestCase {

    private static let channelId = "0123456789abcdef0123456789abcdef"
    private static let removedBody = Data(#"{"code":"channel-removed","detail":"removed","log_index":4}"#.utf8)
    private static let deletedBody = Data(#"{"code":"channel-deleted","detail":"deleted"}"#.utf8)

    // MARK: - Fakes

    private final class Clock: @unchecked Sendable {
        var now: Int64 = 1_000
    }

    /// Answers every request from `reply`, counting what reached it.
    private final class Transport: @unchecked Sendable {
        var requests: [URLRequest] = []
        var reply: (Int, Data) = (200, Data("{}".utf8))

        func perform(_ request: URLRequest) async throws -> (Data, URLResponse) {
            requests.append(request)
            let response = HTTPURLResponse(
                url: request.url!, statusCode: reply.0, httpVersion: nil,
                headerFields: ["Content-Type": "application/json"])!
            return (reply.1, response)
        }
    }

    @MainActor
    private final class Checks {
        var asked: [Data] = []
        var verdict: (SuspensionMarks.Standing, Int64) = (.suspended(untilMs: nil), 50_000)
        var forgotten: [Data] = []
    }

    /// A gate wired to `registry` the way the shared one is wired to the
    /// shared registry.
    private final class Wiring: @unchecked Sendable {
        weak var registry: ChannelRemovals?
    }

    private func make(
        clock: Clock? = nil, checks: Checks? = nil
    ) -> (ChannelRemovalGate, ChannelRemovals) {
        let clock = clock ?? Clock()
        let checks = checks ?? Checks()
        let wiring = Wiring()
        let gate = ChannelRemovalGate(
            onRemoved: { id in Task { @MainActor in wiring.registry?.noteRemoved(id) } },
            onAnswered: { id in Task { @MainActor in wiring.registry?.noteAnswered(id) } },
            nowMs: { clock.now })
        let registry = ChannelRemovals(
            gate: gate,
            check: { id in
                checks.asked.append(id)
                return (standing: checks.verdict.0, recheckAtMs: checks.verdict.1)
            },
            forget: { checks.forgotten.append($0) })
        wiring.registry = registry
        return (gate, registry)
    }

    private func client(_ transport: Transport, gate: ChannelRemovalGate) -> ChannelsClient {
        ChannelsClient(
            baseURL: "https://api.example/channels", roles: ChannelPublisherRoleCache(),
            removalGate: gate, transport: { try await transport.perform($0) })
    }

    private func drain() async {
        for _ in 0..<40 { await Task.yield() }
    }

    // MARK: - The gate

    func testTheGateReadsTheChannelIdOfAChannelsRoute() {
        let id = Self.channelId
        XCTAssertEqual(ChannelRemovalGate.channelId(of: URL(string: "https://a/channels/v1/channels/\(id)/posts")), id)
        XCTAssertEqual(ChannelRemovalGate.channelId(of: URL(string: "https://a/v1/channels/\(id.uppercased())")), id)
        XCTAssertNil(ChannelRemovalGate.channelId(of: URL(string: "https://a/v1/directory/bucket/1")))
        XCTAssertNil(ChannelRemovalGate.channelId(of: URL(string: "https://a/v1/channels/abc/posts")))
    }

    func testA410ChannelRemovedHoldsTheChannelAndIsNotADeletion() async throws {
        let clock = Clock()
        let (gate, registry) = make(clock: clock)
        let transport = Transport()
        transport.reply = (410, Self.removedBody)
        let c = client(transport, gate: gate)

        do {
            _ = try await c.fetchEpoch(channelIdHex: Self.channelId)
            XCTFail("a removed channel answers an error")
        } catch {
            XCTAssertTrue(ChannelsApiError.isChannelRemoved(error))
            XCTAssertFalse(ChannelsApiError.isChannelDeleted(error), "a removal ends nothing")
        }
        await drain()
        XCTAssertEqual(registry.status(Self.channelId), .removed)
        XCTAssertTrue(gate.isHeld(Self.channelId))

        // Held: answered here, the network is not asked again this period.
        do {
            _ = try await c.fetchPosts(channelIdHex: Self.channelId, after: 0, limit: 10)
            XCTFail("held")
        } catch {
            XCTAssertTrue(ChannelsApiError.isChannelRemoved(error))
        }
        XCTAssertEqual(transport.requests.count, 1)
    }

    func testA410ChannelDeletedIsNotARemoval() async {
        let (gate, registry) = make()
        let transport = Transport()
        transport.reply = (410, Self.deletedBody)
        let c = client(transport, gate: gate)
        do {
            _ = try await c.fetchEpoch(channelIdHex: Self.channelId)
        } catch {
            XCTAssertTrue(ChannelsApiError.isChannelDeleted(error))
        }
        await drain()
        XCTAssertNil(registry.status(Self.channelId))
        XCTAssertFalse(gate.isHeld(Self.channelId))
    }

    func testOneCheckPerPeriodThenARestoreLeavesNoTrace() async throws {
        let clock = Clock()
        let checks = Checks()
        checks.verdict = (.suspended(untilMs: nil), 50_000)
        let (gate, registry) = make(clock: clock, checks: checks)
        let transport = Transport()
        transport.reply = (410, Self.removedBody)
        let c = client(transport, gate: gate)

        _ = try? await c.fetchEpoch(channelIdHex: Self.channelId)
        await drain()
        XCTAssertEqual(checks.asked.count, 1)

        clock.now = 49_999
        _ = try? await c.fetchEpoch(channelIdHex: Self.channelId)
        XCTAssertEqual(transport.requests.count, 1, "held until the standing's reuse period ends")

        // The period ended and the channel was restored: it answers again.
        clock.now = 50_000
        transport.reply = (200, Data(#"{"epoch":3}"#.utf8))
        let epoch = try await c.fetchEpoch(channelIdHex: Self.channelId)
        XCTAssertEqual(epoch, 3)
        await drain()
        XCTAssertEqual(transport.requests.count, 2)
        XCTAssertNil(registry.status(Self.channelId))
        XCTAssertFalse(gate.isHeld(Self.channelId))
        XCTAssertEqual(checks.forgotten.count, 1, "the held verdict goes with the restore")
    }

    func testAnUnverifiedRemovalShowsTheNeutralLine() async {
        let checks = Checks()
        checks.verdict = (.unknown, 9_000)
        let (gate, registry) = make(checks: checks)
        registry.noteRemoved(Self.channelId)
        XCTAssertEqual(registry.status(Self.channelId), .checking)
        XCTAssertEqual(registry.status(Self.channelId)?.lineKey, "channels.unavailable",
                       "never a removal before the log says so")
        await drain()
        XCTAssertEqual(registry.status(Self.channelId), .unavailable)
        XCTAssertEqual(registry.status(Self.channelId)?.lineKey, "channels.unavailable")
        XCTAssertTrue(gate.isHeld(Self.channelId))

        checks.verdict = (.notSuspended, 9_000)
        registry.noteAnswered(Self.channelId)
        registry.noteRemoved(Self.channelId)
        await drain()
        XCTAssertEqual(registry.status(Self.channelId), .unavailable, "a log that says not removed proves nothing")
    }

    func testAVerifiedRemovalShowsTheRemovedLine() async {
        let (_, registry) = make()
        registry.noteRemoved(Self.channelId)
        await drain()
        XCTAssertEqual(registry.status(Self.channelId)?.lineKey, "suspended.channel_removed")
    }

    func testARestoreDuringTheCheckLeavesNothing() async {
        let (gate, registry) = make()
        registry.noteRemoved(Self.channelId)
        registry.noteAnswered(Self.channelId)
        await drain()
        XCTAssertNil(registry.status(Self.channelId))
        XCTAssertFalse(gate.isHeld(Self.channelId))
    }

    func testResetDropsEverything() async {
        let (gate, registry) = make()
        registry.noteRemoved(Self.channelId)
        await drain()
        registry.reset()
        XCTAssertNil(registry.status(Self.channelId))
        XCTAssertFalse(gate.isHeld(Self.channelId))
    }

    // MARK: - The conversation

    func testA410KeepsTheChannelAndWithholdsTheWriteControls() async throws {
        let (gate, registry) = make()
        let transport = Transport()
        transport.reply = (410, Self.removedBody)
        let state = ChannelsUiState()
        state.addChannel(ChannelHandle(
            id: Self.channelId, localName: "News", localDescription: "", addedAtMs: 1, handle: "news"))
        state.setSubscriberKeys(channelId: Self.channelId, keys: ChannelsComposer.freshSubscriberKeys())
        let held = PostView(cursor: 7, serverTsMs: 9_000, payloadB64: "AAAA")
        state.setPosts(channelId: Self.channelId, posts: [held])
        let model = ChannelConversationModel(
            channelId: Self.channelId, state: state, client: client(transport, gate: gate),
            accountIdentitySeed: Data(repeating: 1, count: 32), removals: registry)
        XCTAssertTrue(model.writable)

        await model.refresh()
        await drain()

        XCTAssertTrue(state.channels.contains { $0.id == Self.channelId }, "not ended")
        XCTAssertNil(model.removedName, "no ended dialog")
        XCTAssertEqual(state.postsByChannel[Self.channelId], [held], "the held posts stay")
        XCTAssertEqual(model.access, .reading)
        XCTAssertFalse(model.writable)
        XCTAssertEqual(model.removal, .removed)

        let before = transport.requests.count
        model.draft = "hello"
        await model.sendDraft()
        let commented = await model.comment(
            ChannelTimelineItem(
                cursor: 7, serverTsMs: 9_000, epoch: 0, text: "post", attachments: [], forward: nil,
                edited: false, lastOpCursor: nil),
            text: "hi", replyTo: nil)
        XCTAssertFalse(commented)
        XCTAssertEqual(transport.requests.count, before, "nothing is written")
        XCTAssertEqual(model.draft, "hello")
    }

    func testTheHeldPostsAreTheLoadedAndTheLoggedOnes() {
        let loaded = [PostView(cursor: 3, serverTsMs: 30, payloadB64: "c")]
        let logged = [
            PersistedChannelPost(cursor: 1, serverTsMs: 10, text: "a", payloadB64: "a"),
            PersistedChannelPost(cursor: 2, serverTsMs: 20, text: "no wire"),
            PersistedChannelPost(cursor: 3, serverTsMs: 30, text: "c", payloadB64: "old"),
        ]
        let held = ChannelConversationModel.heldPosts(loaded: loaded, logged: logged)
        XCTAssertEqual(held.map(\.cursor), [1, 3])
        XCTAssertEqual(held.last?.payloadB64, "c", "the loaded wire wins")
    }

    // MARK: - The info

    func testARemovedChannelsInfoOffersNoPublisherToolsOrInvitations() {
        let owner: [InfoSection] = [
            .handle, .renameGroup, .invitePeople, .inviteLink, .subscribers, .admins, .reactions, .groupType,
            .membersCanFindMe, .deleteGroup,
        ]
        XCTAssertEqual(ChannelRemovalRules.infoSections(owner, removed: false), owner)
        XCTAssertEqual(
            ChannelRemovalRules.infoSections(owner, removed: true),
            [.handle, .channelDescription, .membersCanFindMe, .unsubscribe],
            "the owner can still take the channel out of the list; Delete stays hidden")
        let subscriber: [InfoSection] = [.handle, .channelDescription, .subscribers, .report, .unsubscribe]
        XCTAssertEqual(
            ChannelRemovalRules.infoSections(subscriber, removed: true),
            [.handle, .channelDescription, .unsubscribe])
    }

    // MARK: - Leaving a removed channel

    private func publisherState() -> ChannelsUiState {
        let state = ChannelsUiState()
        state.addChannel(ChannelHandle(
            id: Self.channelId, localName: "News", localDescription: "", addedAtMs: 1, handle: "news"))
        state.setKeys(channelId: Self.channelId, keys: ChannelPublisherKeys(
            seed: Data(repeating: 0x11, count: 32), signingKeySeed: Data(repeating: 0x22, count: 32)))
        return state
    }

    func testTheOwnerLeavesARemovedChannelWithoutAskingTheService() {
        let state = publisherState()
        ChannelUnsubscribe.leaveRemoved(state: state, channelId: Self.channelId)
        XCTAssertFalse(state.channels.contains { $0.id == Self.channelId })
        XCTAssertNil(state.keysByChannel[Self.channelId])
    }

    func testAnEndFromAnotherDeviceDropsAPublishedChannelOnlyWhenRemoved() async {
        let (_, registry) = make()

        let kept = publisherState()
        ChannelUnsubscribe.endedElsewhere(
            state: kept, channelIdHex: Self.channelId, removals: registry, probeRemoved: { _ in false })
        await drain()
        XCTAssertTrue(kept.channels.contains { $0.id == Self.channelId }, "a live channel stays its publisher's")

        let probed = publisherState()
        ChannelUnsubscribe.endedElsewhere(
            state: probed, channelIdHex: Self.channelId, removals: registry, probeRemoved: { _ in true })
        await drain()
        XCTAssertFalse(probed.channels.contains { $0.id == Self.channelId }, "the service says removed")

        registry.noteRemoved(Self.channelId)
        let known = publisherState()
        ChannelUnsubscribe.endedElsewhere(
            state: known, channelIdHex: Self.channelId, removals: registry,
            probeRemoved: { _ in XCTFail("already known"); return false })
        XCTAssertFalse(known.channels.contains { $0.id == Self.channelId })
    }

    // MARK: - The standing service

    private struct NotProven: Error {}

    private func marks(
        answer: String?, standing: Result<ModerationStanding, Error>, clock: Clock,
        logKey: Data? = Data(repeating: 7, count: 32), proven: Bool = true
    ) -> (SuspensionMarks, () -> Int) {
        var fetches = 0
        let service = SuspensionMarks(ports: SuspensionMarks.Ports(
            logKey: { logKey },
            witnessKeys: { [] },
            witnessThreshold: { 1 },
            prefix: { _ in "abcd" },
            fetch: { _ in
                fetches += 1
                return answer
            },
            verify: { _, _, _, _, _, _ in throw URLError(.badServerResponse) },
            nowMs: { clock.now },
            currentKey: { $0 },
            isOwn: { _ in false },
            sleepMs: { _ in },
            channelPrefix: { _ in "abcd" },
            verifyChannel: { _, channel, _, _, _, _ in
                XCTAssertEqual(channel.count, 16)
                return try standing.get()
            },
            proof: SuspensionMarks.ProofPorts(
                rootKey: { Data(repeating: 9, count: 32) },
                moderatorList: { Data([1]) },
                admitList: { _, _ in true },
                fetchEntry: { index, size in
                    XCTAssertEqual(index, 3)
                    XCTAssertEqual(size, 12)
                    return "{}"
                },
                proveAccount: { _, _, _, _, _, _ in XCTFail("a channel is not an account") },
                proveChannel: { _, _, _, _, channel, _ in
                    XCTAssertEqual(channel.count, 16)
                    if !proven { throw NotProven() }
                })))
        return (service, { fetches })
    }

    private static let answerJSON = #"{"period":1,"root":{"log_size":12}}"#

    func testAnUnprovenChannelRemovalIsUnknown() async {
        let clock = Clock()
        let (service, _) = marks(
            answer: Self.answerJSON,
            standing: .success(ModerationStanding(suspended: true, untilMs: nil, period: 1, reuseUntilMs: 8_000, logIndex: 3)),
            clock: clock, proven: false)
        let result = await service.channelCheck(channelBytes)
        XCTAssertEqual(result.standing, .unknown, "no removed-for-the-rules line without the proof")
        XCTAssertEqual(result.recheckAtMs, min(8_000, clock.now + SuspensionMarks.failureBackoffMs),
                       "asked again at the earlier of the snapshot's reuse and the backoff")
    }

    func testAChannelRemovalWithNoLogIndexIsUnknown() async {
        let clock = Clock()
        let (service, _) = marks(
            answer: Self.answerJSON,
            standing: .success(ModerationStanding(suspended: true, untilMs: nil, period: 1, reuseUntilMs: 8_000, logIndex: nil)),
            clock: clock)
        let result = await service.channelCheck(channelBytes)
        XCTAssertEqual(result.standing, .unknown)
    }

    private var channelBytes: Data { ChannelsClient.hexToData(Self.channelId)! }

    func testTheChannelStandingIsTheVerifiedRemovalUntilItsReusePeriod() async {
        let clock = Clock()
        let (service, fetches) = marks(
            answer: Self.answerJSON,
            standing: .success(ModerationStanding(suspended: true, untilMs: nil, period: 1, reuseUntilMs: 8_000, logIndex: 3)),
            clock: clock)
        let first = await service.channelCheck(channelBytes)
        XCTAssertEqual(first.standing, .suspended(untilMs: nil))
        XCTAssertEqual(first.recheckAtMs, 8_000)
        _ = await service.channelCheck(channelBytes)
        XCTAssertEqual(fetches(), 1, "cached until reuseUntilMs")
    }

    func testAChannelAnswerThatDoesNotVerifyIsUnknown() async {
        let clock = Clock()
        let (service, _) = marks(answer: Self.answerJSON, standing: .failure(URLError(.cannotParseResponse)), clock: clock)
        let result = await service.channelCheck(channelBytes)
        XCTAssertEqual(result.standing, .unknown)
        XCTAssertEqual(result.recheckAtMs, clock.now + SuspensionMarks.failureBackoffMs)
    }

    func testNoPinMeansUnknownAndNothingAsked() async {
        let clock = Clock()
        let (service, fetches) = marks(
            answer: Self.answerJSON,
            standing: .success(ModerationStanding(suspended: true, untilMs: nil, period: 1, reuseUntilMs: 8_000, logIndex: 3)),
            clock: clock, logKey: nil)
        let result = await service.channelCheck(channelBytes)
        XCTAssertEqual(result.standing, .unknown)
        XCTAssertEqual(fetches(), 0)
    }

    func testAFailedFetchIsUnknown() async {
        let clock = Clock()
        let (service, _) = marks(
            answer: nil,
            standing: .success(ModerationStanding(suspended: true, untilMs: nil, period: 1, reuseUntilMs: 8_000, logIndex: 3)),
            clock: clock)
        let result = await service.channelCheck(channelBytes)
        XCTAssertEqual(result.standing, .unknown)
    }

    // MARK: - Hidden from search

    func testThe403HiddenFromSearchIsRecognised() {
        XCTAssertTrue(DiscoveryClientError.isHiddenFromSearch(DiscoveryClientError.http(
            status: 403, endpoint: "/discovery/visibility", body: #"{"error":"hidden-from-search"}"#)))
        XCTAssertFalse(DiscoveryClientError.isHiddenFromSearch(DiscoveryClientError.http(
            status: 403, endpoint: "/discovery/visibility", body: #"{"error":"account-suspended"}"#)))
        XCTAssertFalse(DiscoveryClientError.isHiddenFromSearch(DiscoveryClientError.http(
            status: 401, endpoint: "/discovery/visibility", body: #"{"error":"hidden-from-search"}"#)))
    }

    func testHiddenFromSearchIsHeldUntilAnEveryonePublishSucceeds() throws {
        let c = ReachCenter(persistence: InMemoryReachPersistence(), ds: Offline.ds())
        let hidden = DiscoveryClientError.http(
            status: 403, endpoint: "/discovery/visibility", body: #"{"error":"hidden-from-search"}"#)
        XCTAssertFalse(c.noteEveryoneRefusedAsHidden(hidden, wantPublic: false), "leaving is never refused so")
        XCTAssertFalse(c.hiddenFromSearch)
        XCTAssertTrue(c.noteEveryoneRefusedAsHidden(hidden, wantPublic: true))
        XCTAssertTrue(c.hiddenFromSearch)
        XCTAssertFalse(c.noteEveryoneRefusedAsHidden(URLError(.notConnectedToInternet), wantPublic: true))
        XCTAssertTrue(c.hiddenFromSearch)
        c.noteEveryonePublished(wantPublic: false)
        XCTAssertTrue(c.hiddenFromSearch, "leaving says nothing about search")
        c.noteEveryonePublished(wantPublic: true)
        XCTAssertFalse(c.hiddenFromSearch)
    }

    func testWhileHiddenEveryoneCannotBeChosenAndTheOtherScopesStay() {
        XCTAssertEqual(WhoCanFindMeCopy.modes(hiddenFromSearch: false), [.nobody, .some, .everyone])
        XCTAssertEqual(WhoCanFindMeCopy.modes(hiddenFromSearch: true), [.nobody, .some])

        let some = setGroupsVisible(
            setting: chooseVisibilityMode(setting: defaultVisibilitySetting(), mode: .some), on: true)
        let everyone = chooseVisibilityMode(setting: some, mode: .everyone)
        XCTAssertEqual(WhoCanFindMeCopy.displayed(everyone, hiddenFromSearch: false), everyone)
        let shown = WhoCanFindMeCopy.displayed(everyone, hiddenFromSearch: true)
        XCTAssertEqual(shown.mode, .some)
        XCTAssertEqual(shown, chooseVisibilityMode(setting: everyone, mode: .some),
                       "the shared transition rule decides the parts shown")
        XCTAssertEqual(shown.updatedAtMs, everyone.updatedAtMs, "showing it stores nothing new")
        XCTAssertEqual(WhoCanFindMeCopy.displayed(some, hiddenFromSearch: true), some)
    }
}
