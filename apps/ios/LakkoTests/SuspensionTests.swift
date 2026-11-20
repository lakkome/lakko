// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI
import XCTest

@testable import Lakko

// ADR-0098 step 3 / UX `reporting-and-moderation.md` section 4 — a suspended
// account on its own device: entering suspended mode on a landing `403` and
// on a bearer `403` through the refresh, leaving it on a normal refresh, the
// 10-minute check, the request gate, the one appeal and the reason text.

private let accountBytes = Data((0..<16).map { UInt8($0 + 1) })
private let accountB64 = accountBytes.base64EncodedString()

/// A full `403 account-suspended` landing body.
private func landingBody(s1: String = "s1.restricted", r2: String = "r2.refresh", grant: Bool = false) -> String {
    var object: [String: Any] = [
        "error": "account-suspended",
        "account_id_b64": base64UrlEncodeNoPad(accountBytes),
        "restricted_token": s1,
        "restricted_expires_at_ms": 2_000_000,
        "refresh_token": r2,
        "refresh_expires_at_ms": 9_000_000,
        "key_generation": 3,
    ]
    if grant {
        object["fresh_grant"] = "k1.grant"
        object["fresh_grant_expires_at_ms"] = 1_600_000
    }
    let data = try! JSONSerialization.data(withJSONObject: object)
    return String(data: data, encoding: .utf8)!
}

private func landing(s1: String = "s1.restricted") -> SuspendedLanding {
    SuspendedLanding.parse(Data(landingBody(s1: s1).utf8))!
}

private func normalPair(_ token: String = "v3.normal") -> SessionPair {
    SessionPair(
        sessionToken: token, sessionExpiresAtMs: 5_000_000,
        refreshToken: "r2.next", refreshExpiresAtMs: 9_000_000, keyGeneration: 3)
}

private func response(_ url: URL, _ status: Int) -> HTTPURLResponse {
    HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: nil)!
}

/// Records every callback of a provider.
private final class ProviderEvents: @unchecked Sendable {
    private let lock = NSLock()
    private var _suspended: [String] = []
    private var _ended: [String] = []
    var suspended: [String] { lock.withLock { _suspended } }
    var ended: [String] { lock.withLock { _ended } }
    func noteSuspended(_ s1: String) { lock.withLock { _suspended.append(s1) } }
    func noteEnded(_ account: String) { lock.withLock { _ended.append(account) } }
}

/// What the refresher answers next.
private final class RefreshScript: @unchecked Sendable {
    private let lock = NSLock()
    private var answers: [Result<SessionPair, Error>]
    private(set) var calls = 0
    init(_ answers: [Result<SessionPair, Error>]) { self.answers = answers }
    func next() throws -> SessionPair {
        let answer: Result<SessionPair, Error> = lock.withLock {
            calls += 1
            return answers.isEmpty ? .failure(URLError(.notConnectedToInternet)) : answers.removeFirst()
        }
        return try answer.get()
    }
}

private func provider(_ script: RefreshScript, events: ProviderEvents) -> SessionTokenProvider {
    let provider = SessionTokenProvider()
    provider.nowMs = { 1_000_000 }
    provider.authBaseURL = { "https://auth.test" }
    provider.suspendedWriteBases = { ["https://ds.test", "https://channels.test", "https://storage.test"] }
    provider.refresher = { _ in try script.next() }
    provider.onSuspended = { landing, _ in events.noteSuspended(landing.pair.sessionToken) }
    provider.onSuspensionEnded = { account in events.noteEnded(account) }
    return provider
}

// MARK: - The wire

final class SuspendedLandingWireTests: XCTestCase {

    func testAFullBodyParsesIntoTheS1PairAndTheGrant() throws {
        let parsed = try XCTUnwrap(SuspendedLanding.parse(Data(landingBody(grant: true).utf8)))
        XCTAssertEqual(parsed.accountId, accountBytes)
        XCTAssertEqual(parsed.accountIdB64, accountB64)
        XCTAssertEqual(parsed.pair.sessionToken, "s1.restricted")
        XCTAssertEqual(parsed.pair.sessionExpiresAtMs, 2_000_000)
        XCTAssertEqual(parsed.pair.refreshToken, "r2.refresh")
        XCTAssertEqual(parsed.pair.refreshExpiresAtMs, 9_000_000)
        XCTAssertEqual(parsed.pair.keyGeneration, 3)
        XCTAssertEqual(parsed.freshGrant, FreshAuthGrant(grant: "k1.grant", expiresAtMs: 1_600_000))
    }

    func testABareBodyIsSuspendedWithoutCredentials() throws {
        let bare = Data(#"{"error":"account-suspended"}"#.utf8)
        XCTAssertNil(SuspendedLanding.parse(bare))
        let error = try XCTUnwrap(AccountSuspendedError.from(status: 403, body: bare))
        XCTAssertNil(error.landing)
    }

    func testOnlyA403WithTheCodeIsASuspension() {
        let body = Data(landingBody().utf8)
        XCTAssertNotNil(AccountSuspendedError.from(status: 403, body: body))
        XCTAssertNil(AccountSuspendedError.from(status: 401, body: body))
        XCTAssertNil(AccountSuspendedError.from(status: 403, body: Data(#"{"error":"fresh-auth-required"}"#.utf8)))
    }

    func testTheDetailsDecode() {
        XCTAssertEqual(SuspensionDetails.decode(Data(#"{"suspended":false}"#.utf8)), .notSuspended)
        let body = #"{"suspended":true,"reason":6,"reason_name":"child-safety","until_ms":null,"appeal":"sent"}"#
        XCTAssertEqual(
            SuspensionDetails.decode(Data(body.utf8)),
            .suspended(SuspensionDetails(reason: 6, reasonName: "child-safety", untilMs: nil, appeal: .sent)))
        let timed = #"{"suspended":true,"reason":1,"reason_name":"spam","until_ms":1700000000000,"appeal":"declined"}"#
        XCTAssertEqual(
            SuspensionDetails.decode(Data(timed.utf8)),
            .suspended(SuspensionDetails(reason: 1, reasonName: "spam", untilMs: 1_700_000_000_000, appeal: .declined)))
        XCTAssertNil(SuspensionDetails.decode(Data("not json".utf8)))
    }

    func testTheAppealAnswers() {
        XCTAssertEqual(SuspensionClient.appealResult(status: 204, body: Data()), .sent)
        XCTAssertEqual(
            SuspensionClient.appealResult(status: 409, body: Data(#"{"error":"appeal-already-sent"}"#.utf8)), .sent)
        XCTAssertEqual(
            SuspensionClient.appealResult(status: 409, body: Data(#"{"error":"not-suspended"}"#.utf8)), .notSuspended)
        XCTAssertEqual(SuspensionClient.appealResult(status: 500, body: Data()), .failed)
    }
}

// MARK: - The reason text

final class SuspensionReasonTextTests: XCTestCase {

    @MainActor
    func testEveryReasonNameMapsToTheReportSheetsKey() {
        // The same key the report sheet shows for each reason the core names.
        for (code, key) in ReportSheetModel.reasons {
            let name = reportReasonName(code: code) ?? ""
            XCTAssertEqual(SuspensionReasonText.catalogKey(reasonName: name), key, "reason \(code)")
        }
        XCTAssertEqual(SuspensionReasonText.catalogKey(reasonName: "child-safety"), "report.reason.child_safety")
        XCTAssertEqual(SuspensionReasonText.catalogKey(reasonName: "spam"), "report.reason.spam")
    }

    func testAnUnknownNameReadsAsSomethingElse() {
        XCTAssertEqual(SuspensionReasonText.catalogKey(reasonName: "unknown"), "report.reason.other")
        XCTAssertEqual(SuspensionReasonText.catalogKey(reasonName: ""), "report.reason.other")
        XCTAssertEqual(SuspensionReasonText.catalogKey(reasonName: "../x"), "report.reason.other")
    }

    @MainActor
    func testTheLinesOfTheScreen() {
        let l10n = Localization.shared
        let open = SuspensionDetails(reason: 1, reasonName: "spam", untilMs: nil, appeal: .none)
        XCTAssertEqual(
            SuspendedScreenCopy.reasonLine(open, l10n: l10n),
            l10n.t("suspended.self.reason", texts: ["reason": l10n.t("report.reason.spam")]))
        XCTAssertEqual(SuspendedScreenCopy.endLine(open, l10n: l10n), l10n.t("suspended.self.no_end"))
        let timed = SuspensionDetails(reason: 1, reasonName: "spam", untilMs: 1_700_000_000_000, appeal: .none)
        XCTAssertNotEqual(SuspendedScreenCopy.endLine(timed, l10n: l10n), l10n.t("suspended.self.no_end"))
    }
}

// MARK: - The request gate

final class SuspendedRequestRuleTests: XCTestCase {
    private let writes = ["https://ds.test", "https://channels.test", "https://storage.test"]

    private func allows(_ method: String, _ url: String, bearer: Bool) -> Bool {
        SuspendedRequestRule.allows(
            method: method, url: URL(string: url), carriesSessionBearer: bearer,
            authBase: "https://auth.test", writeBases: writes)
    }

    func testTheS1RoutesOfAuthGo() {
        XCTAssertTrue(allows("GET", "https://auth.test/account/suspension", bearer: true))
        XCTAssertTrue(allows("POST", "https://auth.test/account/suspension/appeal", bearer: true))
        XCTAssertTrue(allows("DELETE", "https://auth.test/account", bearer: true))
        XCTAssertTrue(allows("POST", "https://auth.test/auth/session/logout", bearer: true))
        XCTAssertTrue(allows("POST", "https://auth.test/auth/action/start", bearer: true))
    }

    func testEveryOtherBearerRequestIsWithheld() {
        XCTAssertFalse(allows("POST", "https://auth.test/auth/token", bearer: true))
        XCTAssertFalse(allows("GET", "https://discovery.test/discovery/identity/x", bearer: true))
        XCTAssertFalse(allows("POST", "https://auth.test/auth/session/bind-device", bearer: true))
        XCTAssertFalse(allows("GET", "https://auth.test.evil/account/suspension", bearer: true))
    }

    func testNoWriteLeavesForTheDeliveryServiceChannelsOrBlobs() {
        XCTAssertFalse(allows("POST", "https://ds.test/ds/group/ab/messages", bearer: false))
        XCTAssertFalse(allows("PUT", "https://ds.test/ds/group/ab/claim", bearer: false))
        XCTAssertFalse(allows("POST", "https://channels.test/v1/channels/x/posts", bearer: false))
        XCTAssertFalse(allows("PUT", "https://storage.test/blob/abc", bearer: false))
    }

    func testReadsAndAnonymousRoutesGo() {
        XCTAssertTrue(allows("GET", "https://ds.test/ds/group/ab/messages", bearer: false))
        XCTAssertTrue(allows("POST", "https://auth.test/auth/session/refresh", bearer: false))
        XCTAssertTrue(allows("GET", "https://moderation.test/moderation/moderators", bearer: false))
    }

    func testTheSendingInfoBlocksAreTakenOut() {
        let all: [InfoSection] = [.message, .call, .addMember, .inviteLink, .addContact, .invitePeople, .report]
        XCTAssertEqual(SuspendedInfo.withoutSending(all, suspended: false), all)
        XCTAssertEqual(SuspendedInfo.withoutSending(all, suspended: true), [.message, .report])
    }
}

// MARK: - The provider

final class SuspendedProviderTests: XCTestCase {

    func testARefreshAnsweredSuspendedInstallsTheS1AndEnters() async {
        let events = ProviderEvents()
        let script = RefreshScript([.failure(AccountSuspendedError(landing: landing(s1: "s1.a")))])
        let provider = provider(script, events: events)
        provider.install(normalPair(), accountIdB64: accountB64)

        let outcome = await provider.refresh()

        XCTAssertEqual(outcome, .suspended(landing(s1: "s1.a")))
        XCTAssertTrue(provider.isSuspended)
        XCTAssertEqual(provider.current?.sessionToken, "s1.a")
        XCTAssertEqual(provider.current?.refreshToken, "r2.refresh")
        XCTAssertEqual(events.suspended, ["s1.a"])
    }

    func testANormalRefreshLeavesSuspendedMode() async {
        let events = ProviderEvents()
        let script = RefreshScript([.success(normalPair("v3.back"))])
        let provider = provider(script, events: events)
        provider.install(landing().pair, accountIdB64: accountB64, suspended: true)

        let outcome = await provider.refresh()

        XCTAssertEqual(outcome, .refreshed(normalPair("v3.back")))
        XCTAssertFalse(provider.isSuspended)
        XCTAssertEqual(events.ended, [accountB64])
    }

    func testARefreshThatStaysSuspendedRenewsTheS1AndDoesNotLeave() async {
        let events = ProviderEvents()
        let script = RefreshScript([.failure(AccountSuspendedError(landing: landing(s1: "s1.renewed")))])
        let provider = provider(script, events: events)
        provider.install(landing().pair, accountIdB64: accountB64, suspended: true)

        _ = await provider.refresh()

        XCTAssertTrue(provider.isSuspended)
        XCTAssertEqual(provider.current?.sessionToken, "s1.renewed")
        XCTAssertEqual(events.ended, [])
    }

    func testABearer403OfAuthRefreshesAndEnters() async throws {
        let events = ProviderEvents()
        let script = RefreshScript([.failure(AccountSuspendedError(landing: landing(s1: "s1.b")))])
        let provider = provider(script, events: events)
        provider.install(normalPair(), accountIdB64: accountB64)
        var request = URLRequest(url: URL(string: "https://auth.test/auth/tokens")!)
        request.httpMethod = "POST"
        request.setValue("Bearer v3.normal", forHTTPHeaderField: "Authorization")

        let (_, answer) = try await provider.perform(request) { sent in
            (Data(#"{"error":"account-suspended"}"#.utf8), response(sent.url!, 403))
        }

        XCTAssertEqual((answer as? HTTPURLResponse)?.statusCode, 403)
        XCTAssertEqual(script.calls, 1)
        XCTAssertTrue(provider.isSuspended)
        XCTAssertEqual(events.suspended, ["s1.b"])
    }

    func testWhileSuspendedAWithheldRequestIsNeverSent() async {
        let provider = provider(RefreshScript([]), events: ProviderEvents())
        provider.install(landing().pair, accountIdB64: accountB64, suspended: true)
        var sent = 0
        var discovery = URLRequest(url: URL(string: "https://discovery.test/discovery/oprf")!)
        discovery.setValue("Bearer s1.restricted", forHTTPHeaderField: "Authorization")
        do {
            _ = try await provider.perform(discovery) { request in
                sent += 1
                return (Data(), response(request.url!, 200))
            }
            XCTFail("a discovery request left a suspended device")
        } catch {
            XCTAssertEqual(error as? SessionSuspendedError, SessionSuspendedError())
        }
        var post = URLRequest(url: URL(string: "https://ds.test/ds/group/ab/messages")!)
        post.httpMethod = "POST"
        do {
            _ = try await provider.perform(post) { request in
                sent += 1
                return (Data(), response(request.url!, 201))
            }
            XCTFail("a Delivery Service write left a suspended device")
        } catch {
            XCTAssertEqual(error as? SessionSuspendedError, SessionSuspendedError())
        }
        XCTAssertEqual(sent, 0)
    }

    func testWhileSuspendedTheSuspensionReadGoesWithTheS1() async throws {
        let provider = provider(RefreshScript([]), events: ProviderEvents())
        provider.install(landing().pair, accountIdB64: accountB64, suspended: true)
        var read = URLRequest(url: URL(string: "https://auth.test/account/suspension")!)
        read.setValue("Bearer s1.restricted", forHTTPHeaderField: "Authorization")
        var bearer: String?
        _ = try await provider.perform(read) { request in
            bearer = request.value(forHTTPHeaderField: "Authorization")
            return (Data(#"{"suspended":false}"#.utf8), response(request.url!, 200))
        }
        XCTAssertEqual(bearer, "Bearer s1.restricted")
    }
}

// MARK: - Suspended mode

private final class CenterRecorder {
    var refreshes = 0
    var statusCalls = 0
    var appealCalls = 0
    var now: Int64 = 1_000_000
    var status: SuspensionClient.StatusResult = .answer(.suspended(
        SuspensionDetails(reason: 1, reasonName: "spam", untilMs: nil, appeal: .none)))
    var appeal: SuspensionClient.AppealResult = .sent
    var list: ModerationClient.ListResult = .list(Data([1, 2, 3]))
    var rootKey: Data? = Data(repeating: 7, count: 32)
    var sealed: [String] = []
}

@MainActor
private func center(_ recorder: CenterRecorder) -> SuspensionCenter {
    let defaults = UserDefaults(suiteName: "suspension-\(UUID().uuidString)")!
    return SuspensionCenter(
        store: SuspensionStore(defaults: defaults),
        ports: SuspensionCenter.Ports(
            restrictedToken: { "s1.restricted" },
            refresh: { recorder.refreshes += 1 },
            status: { _ in recorder.statusCalls += 1; return recorder.status },
            sendAppeal: { _, _ in recorder.appealCalls += 1; return recorder.appeal },
            moderatorList: { recorder.list },
            rootKey: { recorder.rootKey },
            seal: { text, _, _ in recorder.sealed.append(text); return Data(text.utf8) },
            nowMs: { recorder.now }))
}

@MainActor
final class SuspensionCenterTests: XCTestCase {

    func testEnterShowsTheScreenAndLeaveEndsItSilently() async {
        let recorder = CenterRecorder()
        let center = center(recorder)
        center.enter(accountIdB64: accountB64)
        XCTAssertTrue(center.active)
        XCTAssertFalse(center.reading)
        await center.loadDetails()
        XCTAssertEqual(center.details, .loaded(SuspensionDetails(reason: 1, reasonName: "spam", untilMs: nil, appeal: .none)))
        XCTAssertTrue(center.isFlagged(accountIdB64: accountB64))

        center.readChats()
        XCTAssertTrue(center.reading)
        center.showScreen()
        XCTAssertFalse(center.reading)

        center.leave(accountIdB64: accountB64)
        XCTAssertFalse(center.active)
        XCTAssertFalse(center.isFlagged(accountIdB64: accountB64))
    }

    func testTheFlagSurvivesARebindAndALogoutDoesNotForgetIt() {
        let recorder = CenterRecorder()
        let center = center(recorder)
        center.enter(accountIdB64: accountB64)
        center.bind(accountIdB64: nil)
        XCTAssertFalse(center.active)
        center.bind(accountIdB64: accountB64)
        XCTAssertTrue(center.active, "the next sign-in of the account starts suspended")
        center.forget(accountIdB64: accountB64)
        XCTAssertFalse(center.active)
        center.bind(accountIdB64: accountB64)
        XCTAssertFalse(center.active)
    }

    func testTheCheckRunsEveryTenMinutes() async {
        let recorder = CenterRecorder()
        let center = center(recorder)
        center.enter(accountIdB64: accountB64)
        await center.tick()
        XCTAssertEqual(recorder.refreshes, 0, "not due right after entering")
        recorder.now += SuspensionRefreshRule.intervalMs - 1
        await center.tick()
        XCTAssertEqual(recorder.refreshes, 0)
        recorder.now += 1
        await center.tick()
        XCTAssertEqual(recorder.refreshes, 1)
        await center.tick()
        XCTAssertEqual(recorder.refreshes, 1, "the interval starts again")
    }

    func testTheForegroundAsksAtOnceAndNothingRunsWhenNotSuspended() async {
        let recorder = CenterRecorder()
        let center = center(recorder)
        await center.noteForeground()
        await center.tick()
        XCTAssertEqual(recorder.refreshes, 0)
        center.enter(accountIdB64: accountB64)
        await center.noteForeground()
        XCTAssertEqual(recorder.refreshes, 1)
    }

    func testTheRuleOfTheInterval() {
        XCTAssertTrue(SuspensionRefreshRule.due(lastMs: nil, nowMs: 5))
        XCTAssertFalse(SuspensionRefreshRule.due(lastMs: 0, nowMs: 599_999))
        XCTAssertTrue(SuspensionRefreshRule.due(lastMs: 0, nowMs: 600_000))
        XCTAssertTrue(SuspensionRefreshRule.due(lastMs: 1_000, nowMs: 10), "a clock set back asks again")
    }

    func testTheAppealIsSentOnce() async {
        let recorder = CenterRecorder()
        let center = center(recorder)
        center.enter(accountIdB64: accountB64)
        await center.loadDetails()
        XCTAssertTrue(center.canAppeal)

        let first = await center.sendAppeal("  It was not spam.  ")
        XCTAssertTrue(first)
        XCTAssertEqual(recorder.appealCalls, 1)
        XCTAssertEqual(recorder.sealed, ["  It was not spam.  "])
        XCTAssertFalse(center.canAppeal)
        guard case .loaded(let details) = center.details else { return XCTFail("details gone") }
        XCTAssertEqual(details.appeal, .sent)

        let second = await center.sendAppeal("Again")
        XCTAssertFalse(second)
        XCTAssertEqual(recorder.appealCalls, 1, "a second appeal never leaves the device")
    }

    func testAnEmptyOrTooLongAppealIsNotSent() async {
        let recorder = CenterRecorder()
        let center = center(recorder)
        center.enter(accountIdB64: accountB64)
        await center.loadDetails()
        let blank = await center.sendAppeal("   \n ")
        XCTAssertFalse(blank)
        let long = await center.sendAppeal(String(repeating: "a", count: SuspensionAppeal.maxCharacters + 1))
        XCTAssertFalse(long)
        XCTAssertEqual(recorder.appealCalls, 0)
        XCTAssertEqual(SuspensionAppeal.clamp(String(repeating: "a", count: 1_005)).count, 1_000)
        XCTAssertEqual(SuspensionAppeal.length("e\u{301}"), 2, "the counter counts what the seal counts")
    }

    func testNoListOrNoRootKeyIsUnavailable() async {
        let recorder = CenterRecorder()
        recorder.list = .unavailable
        let center = center(recorder)
        center.enter(accountIdB64: accountB64)
        await center.loadDetails()
        let sent = await center.sendAppeal("Please")
        XCTAssertFalse(sent)
        XCTAssertEqual(center.appealPhase, .unavailable)
        recorder.list = .list(Data([1]))
        recorder.rootKey = nil
        _ = await center.sendAppeal("Please")
        XCTAssertEqual(center.appealPhase, .unavailable)
        XCTAssertEqual(recorder.appealCalls, 0)
    }

    func testAFailedSendKeepsTheSheetAndCanBeRetried() async {
        let recorder = CenterRecorder()
        recorder.appeal = .failed
        let center = center(recorder)
        center.enter(accountIdB64: accountB64)
        await center.loadDetails()
        let first = await center.sendAppeal("Please")
        XCTAssertFalse(first)
        XCTAssertEqual(center.appealPhase, .failed)
        recorder.appeal = .sent
        let second = await center.sendAppeal("Please")
        XCTAssertTrue(second)
    }

    func testAFailedLoadShowsTheRetry() async {
        let recorder = CenterRecorder()
        recorder.status = .failed
        let center = center(recorder)
        center.enter(accountIdB64: accountB64)
        await center.loadDetails()
        XCTAssertEqual(center.details, .failed)
        XCTAssertFalse(center.canAppeal)
    }
}

// MARK: - A landing

private struct EmptyPersistence: SessionPersistence {
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
    func deleteSessionToken(accountIdB64: String) {}
}

private final class NoDirectory: AccountDirectory {
    func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
        throw AuthClientError.argument("discovery must not be reached")
    }

    func publishIdentityBundle(
        accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
        leafAuth: Data?, genesisAttest: Data?, keyResetAttest: Data?, sessionToken: String
    ) async throws {
        throw AuthClientError.argument("discovery must not be reached")
    }
}

@MainActor
final class SuspendedLandingFlowTests: XCTestCase {

    func testALanding403OnADeviceWithoutKeysEntersSuspendedModeWithTheScreenAlone() async throws {
        let host = NameRouteStub.scope { _, path, _ in
            path == "/totp/recover" ? (403, landingBody()) : (503, "")
        }
        let tokens = SessionTokenProvider()
        let repo = AuthRepository(
            sessionStore: SessionStore(defaults: UserDefaults(suiteName: "suspended-\(UUID().uuidString)")!),
            authClientFactory: { NameRouteStub.client(host) },
            persistence: EmptyPersistence(),
            nowMs: { 1_000_000 },
            accountDirectory: NoDirectory(),
            restoreDirectory: OfflineRestoreDirectory(),
            tokenProvider: tokens
        )
        let recorder = CenterRecorder()
        let center = center(recorder)
        repo.suspension = center

        try await repo.recoverWithTotp(username: "alice_1", code: "123456")

        XCTAssertEqual(repo.state, .anon(cachedUsername: "alice_1"))
        XCTAssertNil(repo.identitySecrets)
        XCTAssertTrue(tokens.isSuspended)
        XCTAssertEqual(tokens.current?.sessionToken, "s1.restricted")
        XCTAssertEqual(tokens.current?.refreshToken, "r2.refresh")
        XCTAssertEqual(center.account, accountB64)
        XCTAssertTrue(center.isFlagged(accountIdB64: accountB64))
        // Nothing but the refused route was asked.
        XCTAssertEqual(NameRouteStub.requests(host).map(\.path), ["/totp/recover"])

        // Sign out: suspended mode off, the flag kept for the next sign-in.
        repo.logout()
        XCTAssertFalse(center.active)
        XCTAssertTrue(center.isFlagged(accountIdB64: accountB64))
    }
}

// MARK: - Coordinator follow-up: Delete account without keys, file reads

final class SuspendedScreenActionTests: XCTestCase {

    func testDeleteAccountShowsExactlyWhenTheKeysAreAbsent() {
        XCTAssertEqual(SuspendedScreenAction.middle(keysHere: true), .readChats)
        XCTAssertEqual(SuspendedScreenAction.middle(keysHere: false), .deleteAccount)
    }

    func testTheFileReadPassAndTokenIssuanceGoWhileSuspended() {
        let writes = ["https://ds.test", "https://channels.test", "https://storage.test"]
        func allows(_ method: String, _ url: String, bearer: Bool) -> Bool {
            SuspendedRequestRule.allows(
                method: method, url: URL(string: url), carriesSessionBearer: bearer,
                authBase: "https://auth.test", writeBases: writes)
        }
        XCTAssertTrue(allows("POST", "https://auth.test/auth/tokens", bearer: true))
        XCTAssertTrue(allows("POST", "https://storage.test/blob/pass/read", bearer: false))
        XCTAssertTrue(allows("GET", "https://storage.test/blob/abcd", bearer: false))
        XCTAssertFalse(allows("POST", "https://storage.test/blob/pass/upload", bearer: false))
        XCTAssertFalse(allows("POST", "https://storage.test/blob/keep", bearer: false))
        XCTAssertFalse(allows("POST", "https://ds.test/blob/pass/read/x", bearer: false))
    }

    func testTheDeletionCleanupsOutsideAuthTakeTheS1() {
        let routes = [
            SuspendedRequestRule.BearerRoute(base: "https://discovery.test", method: "DELETE", path: "/discovery/account"),
            SuspendedRequestRule.BearerRoute(base: "https://storage.test", method: "DELETE", path: "/backup"),
        ]
        func allows(_ method: String, _ url: String) -> Bool {
            SuspendedRequestRule.allows(
                method: method, url: URL(string: url), carriesSessionBearer: true,
                authBase: "https://auth.test", writeBases: ["https://storage.test"], bearerRoutes: routes)
        }
        XCTAssertTrue(allows("DELETE", "https://discovery.test/discovery/account"))
        XCTAssertTrue(allows("DELETE", "https://storage.test/backup"))
        XCTAssertFalse(allows("GET", "https://storage.test/backup"), "only the deletion")
        XCTAssertFalse(allows("PUT", "https://storage.test/backup"))
        XCTAssertFalse(allows("DELETE", "https://discovery.test/discovery/keypackage/device/x"))
        XCTAssertFalse(allows("POST", "https://discovery.test/discovery/oprf"))
    }

    func testTheMessageMenuOffersNothingThatSendsWhileSuspended() {
        let all: [MessageMenuAction] = [.reply, .forward, .copy, .translate, .select, .pin, .save, .edit, .delete, .report]
        XCTAssertEqual(MessageMenuAction.whileSuspended(all, suspended: false), all)
        XCTAssertEqual(
            MessageMenuAction.whileSuspended(all, suspended: true),
            [.copy, .translate, .select, .pin, .save, .report])
        XCTAssertEqual(MessageMenuAction.whileSuspended([.ban, .copy], suspended: true), [.copy])
    }

    @MainActor
    func testTheMenuRequestLosesItsReactionBarWhileSuspended() {
        let request = MessageMenuRequest(
            id: 1, bubbleFrame: .zero, isOutgoing: false,
            actions: [.reply, .forward, .copy, .edit, .delete],
            canReact: true, bubble: AnyView(EmptyView()),
            onReact: { _ in }, onMoreReactions: {}, perform: { _ in })
        XCTAssertTrue(request.narrowed(suspended: false).canReact)
        XCTAssertEqual(request.narrowed(suspended: false).actions, [.reply, .forward, .copy, .edit, .delete])
        let narrowed = request.narrowed(suspended: true)
        XCTAssertFalse(narrowed.canReact)
        XCTAssertEqual(narrowed.actions, [.copy])
        XCTAssertEqual(narrowed.id, 1)
    }

    func testOnlyThePersonalPinIsOfferedWhileSuspended() {
        XCTAssertTrue(PinGate.maySetShared(oneToOne: true, ownRole: nil, membershipEnded: false))
        XCTAssertFalse(PinGate.maySetShared(oneToOne: true, ownRole: nil, membershipEnded: false, suspended: true))
        XCTAssertFalse(PinGate.maySetShared(oneToOne: false, ownRole: .owner, membershipEnded: false, suspended: true))
        let maySet = PinGate.maySetShared(oneToOne: false, ownRole: .owner, membershipEnded: false, suspended: true)
        let pin = PinSheet.options(sharedPinned: false, personalPinned: false, maySetShared: maySet, oneToOne: false)
        XCTAssertEqual(pin.map(\.choice), [.pinForMe])
        // A message pinned for everyone: no shared Unpin, only Pin for me.
        let shared = PinSheet.options(sharedPinned: true, personalPinned: false, maySetShared: maySet, oneToOne: false)
        XCTAssertEqual(shared.map(\.choice), [.pinForMe])
        let both = PinSheet.options(sharedPinned: true, personalPinned: true, maySetShared: maySet, oneToOne: false)
        XCTAssertEqual(both.map(\.choice), [.unpinForMe])
    }

    func testTheCallLinkLandingOffersNoJoinWhileSuspended() {
        XCTAssertTrue(JoinCallSuspension.offersJoin(suspended: false))
        XCTAssertFalse(JoinCallSuspension.offersJoin(suspended: true))
    }

    func testOnlyTheReadPassContextIsDrawnWhileSuspended() {
        for context in AnonTokenContext.allCases {
            XCTAssertTrue(AnonTokenPool.issuesWhileSuspended(context, suspended: false))
            XCTAssertEqual(AnonTokenPool.issuesWhileSuspended(context, suspended: true), context == .blobRead)
        }
    }
}

final class SuspendedTokenPoolTests: XCTestCase {

    func testASuspendedPoolDrawsBlobReadOnlyAndTheRestWhenItEnds() async {
        let issuer = FakeIssuer()
        let pool = AnonTokenPool(issuer: issuer, random: { 0 })
        let flag = SuspendedFlag(true)
        pool.suspended = { flag.value }
        pool.configure(accountKey: "acct", sessionToken: "s1.restricted")
        await pool.runScheduledRefills()
        XCTAssertFalse(issuer.calls(for: .blobRead).isEmpty)
        XCTAssertEqual(Set(issuer.calls.map(\.context)), [.blobRead], "nothing else is asked while suspended")

        flag.value = false
        await pool.runScheduledRefills()
        XCTAssertFalse(issuer.calls(for: .discoveryLookup).isEmpty, "the suspension over, the rest is topped up")
        pool.clear()
    }

    func testIssuanceAnsweredSuspendedIsNotNowNotAFailure() async throws {
        let host = NameRouteStub.scope { _, path, _ in
            path == "/auth/tokens" ? (403, #"{"error":"account-suspended"}"#) : (503, "")
        }
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [NameRouteStub.self]
        let issuer = AuthAnonTokenIssuer(baseURL: "http://\(host):9", session: URLSession(configuration: config))
        do {
            _ = try await issuer.issue(context: .discoveryLookup, epoch: 1, count: 1, sessionToken: "s1.restricted")
            XCTFail("issued while suspended")
        } catch let refused as AnonTokenUnavailable {
            XCTAssertEqual(refused, .refused(code: "account-suspended"))
        }
    }
}

private final class SuspendedFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var _value: Bool
    init(_ value: Bool) { _value = value }
    var value: Bool {
        get { lock.withLock { _value } }
        set { lock.withLock { _value = newValue } }
    }
}

/// Answers every request 204 and records the account deletion.
final class SuspendedDeleteStub: URLProtocol {
    nonisolated(unsafe) static var deletes: [(bearer: String?, grant: String?)] = []
    /// Every DELETE: path and bearer.
    nonisolated(unsafe) static var allDeletes: [(path: String, bearer: String?)] = []

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        if request.httpMethod == "DELETE" {
            Self.allDeletes.append((request.url?.path ?? "", request.value(forHTTPHeaderField: "Authorization")))
        }
        if request.httpMethod == "DELETE", request.url?.path == "/account" {
            Self.deletes.append((
                request.value(forHTTPHeaderField: "Authorization"),
                request.value(forHTTPHeaderField: WebauthnClient.freshGrantHeader)))
        }
        let response = HTTPURLResponse(
            url: request.url ?? URL(string: "http://stub.test")!, statusCode: 204,
            httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data())
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

@MainActor
final class SuspendedDeleteWithoutKeysTests: XCTestCase {

    override func setUp() {
        super.setUp()
        SuspendedDeleteStub.deletes = []
        SuspendedDeleteStub.allDeletes = []
    }

    override func tearDown() {
        URLProtocol.unregisterClass(SuspendedDeleteStub.self)
        super.tearDown()
    }

    func testADeviceWithoutKeysDeletesTheSuspendedAccountWithTheS1AndTheGrant() async throws {
        let host = NameRouteStub.scope { _, path, _ in
            path == "/totp/recover" ? (403, landingBody(grant: true)) : (503, "")
        }
        let tokens = SessionTokenProvider()
        let repo = AuthRepository(
            sessionStore: SessionStore(defaults: UserDefaults(suiteName: "suspended-del-\(UUID().uuidString)")!),
            authClientFactory: { NameRouteStub.client(host) },
            persistence: EmptyPersistence(),
            nowMs: { 1_000_000 },
            accountDirectory: NoDirectory(),
            restoreDirectory: OfflineRestoreDirectory(),
            removeDeliveredNotifications: {},
            tokenProvider: tokens,
            passkeyPresence: { _ in false },
            grantNowMs: { 1_000_000 }
        )
        repo.suspension = center(CenterRecorder())
        try await repo.recoverWithTotp(username: "alice_1", code: "123456")
        XCTAssertNil(repo.identitySecrets)
        let target = try XCTUnwrap(repo.accountForDeletion())
        XCTAssertEqual(target.accountId, accountBytes)
        XCTAssertEqual(target.sessionToken, "s1.restricted")
        XCTAssertTrue(repo.holdsLiveFreshAuth(), "the landing's grant is the proof")

        URLProtocol.registerClass(SuspendedDeleteStub.self)
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [SuspendedDeleteStub.self]
        var wiped: [String] = []
        let model = PasskeysViewModel(
            authRepository: repo,
            wipeAfterDeletion: { wiped.append($0) },
            client: WebauthnClient(baseURL: "http://auth.test", session: URLSession(configuration: configuration)),
            assertDeleteAction: { _ in XCTFail("no passkey gesture is expected") })

        let deleted = await model.deleteAccount(proof: model.makeProof(.deleteAccount), hasPasskey: false)

        XCTAssertTrue(deleted)
        XCTAssertEqual(wiped, [accountB64], "the usual local cleanup runs")
        XCTAssertEqual(SuspendedDeleteStub.deletes.count, 1)
        XCTAssertEqual(SuspendedDeleteStub.deletes.first?.bearer, "Bearer s1.restricted")
        XCTAssertEqual(SuspendedDeleteStub.deletes.first?.grant, "k1.grant")
        // The directory entry and the backup go too, with the s1.
        let cleanups = SuspendedDeleteStub.allDeletes.filter { $0.path != "/account" }
        XCTAssertEqual(cleanups.count, 2)
        XCTAssertEqual(cleanups.filter { $0.path.hasSuffix("/discovery/account") }.count, 1)
        XCTAssertEqual(cleanups.filter { $0.path.hasSuffix("/backup") }.count, 1)
        XCTAssertTrue(cleanups.allSatisfy { $0.bearer == "Bearer s1.restricted" })
    }
}

// MARK: - The appeal's moderator list

/// The appeal is sealed only to a list the report sheet's gate admits:
/// verified under the root key, not older than this device's floor, the
/// floor raised on a newer one.
@MainActor
final class SuspensionAppealListTests: XCTestCase {

    private var defaults: UserDefaults!
    private let root = Data(repeating: 7, count: 32)

    override func setUp() {
        super.setUp()
        defaults = UserDefaults(suiteName: "appeal-list-\(UUID().uuidString)")
    }

    /// A list's version is its first byte; a list starting 0xFF does not verify.
    private func admitting(_ list: Data, _ rootKey: Data) -> UInt64? {
        ModeratorListGate.admit(list, rootKey: rootKey, defaults: defaults) { list, _ in
            guard let first = list.first, first != 0xFF else { throw URLError(.cannotParseResponse) }
            return UInt64(first)
        }
    }

    private var floor: UInt64? {
        (defaults.object(forKey: ModeratorListGate.highestKey) as? NSNumber)?.uint64Value
    }

    private func appealCenter(served: Data, sealedTo: @escaping (Data) -> Void) -> SuspensionCenter {
        let store = UserDefaults(suiteName: "suspension-\(UUID().uuidString)")!
        let root = self.root
        return SuspensionCenter(
            store: SuspensionStore(defaults: store),
            ports: SuspensionCenter.Ports(
                restrictedToken: { "s1.restricted" },
                refresh: {},
                status: { _ in .answer(.suspended(
                    SuspensionDetails(reason: 1, reasonName: "spam", untilMs: nil, appeal: .none))) },
                sendAppeal: { _, _ in .sent },
                moderatorList: {
                    await ModeratorListGate.admittedList(
                        rootKey: root, fetch: { .list(served) }, admitting: self.admitting)
                },
                rootKey: { root },
                seal: { _, list, _ in sealedTo(list); return Data([1]) },
                nowMs: { 1_000_000 }))
    }

    func testAnOlderListIsNotSealedTo() async {
        defaults.set(NSNumber(value: UInt64(5)), forKey: ModeratorListGate.highestKey)
        var sealedTo: [Data] = []
        let center = appealCenter(served: Data([4, 0xAA]), sealedTo: { sealedTo.append($0) })
        center.enter(accountIdB64: accountB64)
        await center.loadDetails()
        let sent = await center.sendAppeal("It was not spam.")
        XCTAssertFalse(sent)
        XCTAssertTrue(sealedTo.isEmpty, "nothing is sealed to a list older than the floor")
        XCTAssertEqual(center.appealPhase, .unavailable)
        XCTAssertEqual(floor, 5, "the floor is not lowered")
    }

    func testANewerListIsSealedToAndRaisesTheFloor() async {
        defaults.set(NSNumber(value: UInt64(5)), forKey: ModeratorListGate.highestKey)
        var sealedTo: [Data] = []
        let center = appealCenter(served: Data([6, 0xAA]), sealedTo: { sealedTo.append($0) })
        center.enter(accountIdB64: accountB64)
        await center.loadDetails()
        let sent = await center.sendAppeal("It was not spam.")
        XCTAssertTrue(sent)
        XCTAssertEqual(sealedTo, [Data([6, 0xAA])])
        XCTAssertEqual(floor, 6)
    }

    func testTheSharedGateRefusesAnUnverifiedListOrNoRootKey() async {
        var fetches = 0
        let unverified = await ModeratorListGate.admittedList(
            rootKey: root, fetch: { fetches += 1; return .list(Data([0xFF])) }, admitting: admitting)
        XCTAssertEqual(unverified, .unavailable)
        XCTAssertNil(floor, "nothing is recorded for a refused list")
        let noRoot = await ModeratorListGate.admittedList(
            rootKey: nil, fetch: { fetches += 1; return .list(Data([9])) }, admitting: admitting)
        XCTAssertEqual(noRoot, .unavailable)
        XCTAssertEqual(fetches, 1, "no root key asks for nothing")
        let failed = await ModeratorListGate.admittedList(
            rootKey: root, fetch: { .failed }, admitting: admitting)
        XCTAssertEqual(failed, .failed)
        let equal = await ModeratorListGate.admittedList(
            rootKey: root, fetch: { .list(Data([3])) }, admitting: admitting)
        XCTAssertEqual(equal, .list(Data([3])))
        let same = await ModeratorListGate.admittedList(
            rootKey: root, fetch: { .list(Data([3, 1])) }, admitting: admitting)
        XCTAssertEqual(same, .list(Data([3, 1])), "a list at the floor is admitted")
    }
}
