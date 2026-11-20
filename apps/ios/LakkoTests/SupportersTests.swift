// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import XCTest

@testable import Lakko

// Supporter codes and the supporters list (ADR-0097, `docs/ux/supporters.md`).

// MARK: - Fakes

private final class FakeRequest: SupporterTokenRequesting {
    let code: String
    let requestEpoch: UInt64
    var finalizeError: Error?
    private(set) var finalizedWithKey: String?

    init(code: String, epoch: UInt64) {
        self.code = code
        self.requestEpoch = epoch
    }

    func epoch() -> UInt64 { requestEpoch }
    func redeemBodyJson() -> String { #"{"code":"\#(code)","epoch":\#(requestEpoch)}"# }
    func finalize(evaluationB64url: String, proofB64url: String, publicKeyB64url: String) throws -> String {
        if let finalizeError { throw finalizeError }
        finalizedWithKey = publicKeyB64url
        return "header-\(requestEpoch)-\(evaluationB64url)-\(publicKeyB64url)"
    }
}

private final class FakeTransport: SupporterCodeTransport {
    var answers: [(status: Int, body: Data)?] = []
    var keyError: Error?
    private(set) var bodies: [String] = []
    private(set) var keyEpochs: [UInt64] = []

    func redeem(bodyJson: String) async -> (status: Int, body: Data)? {
        bodies.append(bodyJson)
        return answers.isEmpty ? nil : answers.removeFirst()
    }

    func betaPlaceKey(epoch: UInt64) async throws -> String {
        keyEpochs.append(epoch)
        if let keyError { throw keyError }
        return "key\(epoch)"
    }
}

private struct Boom: Error {}

private func json(_ text: String) -> Data { Data(text.utf8) }

private func evaluated(epoch: UInt64) -> (status: Int, body: Data)? {
    (200, json(#"{"context":"beta-place","epoch":\#(epoch),"evaluation":"ev","proof":"pr"}"#))
}

final class SupporterCodeTests: XCTestCase {

    private func redeemer(
        _ transport: FakeTransport,
        epoch: UInt64 = 7,
        normalize: @escaping (String) throws -> String = { $0 },
        requests: @escaping (FakeRequest) -> Void = { _ in }
    ) -> SupporterCodeRedeemer {
        SupporterCodeRedeemer(
            transport: transport,
            normalize: normalize,
            currentEpoch: { epoch },
            makeRequest: { code, epoch in
                let request = FakeRequest(code: code, epoch: epoch)
                requests(request)
                return request
            },
            contextName: "beta-place")
    }

    // MARK: Entry

    func testContinueNeedsSixteenLettersAndDigits() {
        XCTAssertTrue(SupporterCodeEntry.isComplete("K7QM-2XRA-9DPF-4WNB"))
        XCTAssertTrue(SupporterCodeEntry.isComplete("k7qm 2xra 9dpf 4wnb"))
        XCTAssertTrue(SupporterCodeEntry.isComplete("K7QM2XRA9DPF4WNB"))
        XCTAssertFalse(SupporterCodeEntry.isComplete("K7QM-2XRA-9DPF-4WN"))
        XCTAssertFalse(SupporterCodeEntry.isComplete("K7QM-2XRA-9DPF-4WNBX"))
        XCTAssertFalse(SupporterCodeEntry.isComplete(""))
    }

    func testTheCoreNormalisesAForgivingCode() throws {
        let canonical = try supporterCodeNormalize(text: "K7QM-2XRA-9DPF-4WNB")
        XCTAssertEqual(try supporterCodeNormalize(text: "k7qm 2xra 9dpf 4wnb"), canonical)
        XCTAssertThrowsError(try supporterCodeNormalize(text: "short"))
    }

    // MARK: Lifetime

    func testAPlaceIsLiveThroughTheNextEpoch() {
        let place = HeldSupporterPlace(headerValue: "h", epoch: 10)
        XCTAssertTrue(place.isLive(currentEpoch: 10))
        XCTAssertTrue(place.isLive(currentEpoch: 11))
        XCTAssertFalse(place.isLive(currentEpoch: 12))
    }

    func testAnExpiredPlaceIsDropped() {
        let store = InMemorySupporterPlaceStore(HeldSupporterPlace(headerValue: "h", epoch: 3))
        var epoch: UInt64 = 4
        let holder = SupporterPlaceHolder(store: store, currentEpoch: { epoch })
        XCTAssertEqual(holder.live()?.headerValue, "h")
        epoch = 5
        XCTAssertNil(holder.live())
        XCTAssertNil(store.place)
    }

    // MARK: Answers

    func testTheRedeemAnswersAreRead() {
        XCTAssertEqual(
            SupporterRedeemAnswer.classify(status: 200, body: evaluated(epoch: 3)!.body, context: "beta-place"),
            .evaluated(epoch: 3, evaluation: "ev", proof: "pr"))
        // Another context is no place.
        XCTAssertEqual(
            SupporterRedeemAnswer.classify(
                status: 200, body: json(#"{"context":"call-join","epoch":3,"evaluation":"e","proof":"p"}"#),
                context: "beta-place"),
            .line(.failed))
        for (status, code) in [
            (404, "supporter-code-unknown"), (410, "supporter-code-expired"),
            (409, "supporter-code-mismatch"), (400, "supporter-code-malformed"),
        ] {
            XCTAssertEqual(
                SupporterRedeemAnswer.classify(
                    status: status, body: json(#"{"error":"\#(code)"}"#), context: "beta-place"),
                .line(.unknown), code)
        }
        XCTAssertEqual(
            SupporterRedeemAnswer.classify(
                status: 429, body: json(#"{"error":"supporter-code-limited"}"#), context: "beta-place"),
            .line(.limited))
        XCTAssertEqual(
            SupporterRedeemAnswer.classify(
                status: 400, body: json(#"{"error":"epoch-not-issuable","current_epoch":9}"#),
                context: "beta-place"),
            .epochNotIssuable(currentEpoch: 9))
        XCTAssertEqual(
            SupporterRedeemAnswer.classify(status: 503, body: json("{}"), context: "beta-place"),
            .line(.failed))
        XCTAssertEqual(
            SupporterRedeemAnswer.classify(status: 400, body: json(#"{"error":"bad-blinded-element"}"#),
                                           context: "beta-place"),
            .line(.failed))
    }

    // MARK: Exchange

    func testAWorkingCodeHoldsAPlaceCheckedAgainstTheKeyRoute() async {
        let transport = FakeTransport()
        transport.answers = [evaluated(epoch: 7)]
        let outcome = await redeemer(transport).redeem(code: "K7QM2XRA9DPF4WNB")
        XCTAssertEqual(outcome, .held(HeldSupporterPlace(headerValue: "header-7-ev-key7", epoch: 7)))
        XCTAssertEqual(transport.keyEpochs, [7])
    }

    func testAnUnknownCodeShowsItsLine() async {
        let transport = FakeTransport()
        transport.answers = [(404, json(#"{"error":"supporter-code-unknown"}"#))]
        let outcome = await redeemer(transport).redeem(code: "K7QM2XRA9DPF4WNB")
        XCTAssertEqual(outcome, .line(.unknown))
        XCTAssertTrue(transport.keyEpochs.isEmpty)
    }

    func testACodeTheCoreRefusesNeverReachesTheServer() async {
        let transport = FakeTransport()
        let outcome = await redeemer(transport, normalize: { _ in throw Boom() }).redeem(code: "UUUU")
        XCTAssertEqual(outcome, .line(.unknown))
        XCTAssertTrue(transport.bodies.isEmpty)
    }

    func testNoAnswerIsCouldNotCheck() async {
        let transport = FakeTransport()
        transport.answers = [nil]
        let outcome = await redeemer(transport).redeem(code: "K7QM2XRA9DPF4WNB")
        XCTAssertEqual(outcome, .line(.failed))
    }

    func testTooManyTries() async {
        let transport = FakeTransport()
        transport.answers = [(429, json(#"{"error":"supporter-code-limited"}"#))]
        let outcome = await redeemer(transport).redeem(code: "K7QM2XRA9DPF4WNB")
        XCTAssertEqual(outcome, .line(.limited))
    }

    func testAnotherEpochRebuildsOnce() async {
        let transport = FakeTransport()
        transport.answers = [
            (400, json(#"{"error":"epoch-not-issuable","current_epoch":8}"#)),
            evaluated(epoch: 8),
        ]
        let outcome = await redeemer(transport, epoch: 7).redeem(code: "K7QM2XRA9DPF4WNB")
        XCTAssertEqual(outcome, .held(HeldSupporterPlace(headerValue: "header-8-ev-key8", epoch: 8)))
        XCTAssertEqual(transport.bodies.count, 2)
        XCTAssertTrue(transport.bodies[1].contains(#""epoch":8"#))
    }

    func testASecondEpochRefusalStops() async {
        let transport = FakeTransport()
        transport.answers = [
            (400, json(#"{"error":"epoch-not-issuable","current_epoch":8}"#)),
            (400, json(#"{"error":"epoch-not-issuable","current_epoch":9}"#)),
        ]
        let outcome = await redeemer(transport, epoch: 7).redeem(code: "K7QM2XRA9DPF4WNB")
        XCTAssertEqual(outcome, .line(.failed))
        XCTAssertEqual(transport.bodies.count, 2)
    }

    func testAnEvaluationForAnotherEpochIsRefused() async {
        let transport = FakeTransport()
        transport.answers = [evaluated(epoch: 6)]
        let outcome = await redeemer(transport, epoch: 7).redeem(code: "K7QM2XRA9DPF4WNB")
        XCTAssertEqual(outcome, .line(.failed))
    }

    func testAProofThatFailsHoldsNothing() async {
        let transport = FakeTransport()
        transport.answers = [evaluated(epoch: 7)]
        let outcome = await redeemer(transport, requests: { $0.finalizeError = Boom() })
            .redeem(code: "K7QM2XRA9DPF4WNB")
        XCTAssertEqual(outcome, .line(.failed))
    }

    func testAKeyRouteFailureHoldsNothing() async {
        let transport = FakeTransport()
        transport.answers = [evaluated(epoch: 7)]
        transport.keyError = Boom()
        let outcome = await redeemer(transport).redeem(code: "K7QM2XRA9DPF4WNB")
        XCTAssertEqual(outcome, .line(.failed))
    }

    func testTheRealRequestIsDeterministicPerCodeAndEpoch() throws {
        let code = try supporterCodeNormalize(text: "K7QM-2XRA-9DPF-4WNB")
        let a = try SupporterCodeRequest(code: code, epoch: 5)
        let b = try SupporterCodeRequest(code: code, epoch: 5)
        XCTAssertEqual(a.blindedElementB64url(), b.blindedElementB64url())
        XCTAssertEqual(a.epoch(), 5)
        XCTAssertTrue(a.redeemBodyJson().contains("blinded_element"))
    }

    // MARK: The bind

    func testSupporterCodeUsedIsRecognised() {
        XCTAssertTrue(SignupCap.isSupporterCodeUsed(AuthClientError.http(
            status: 403, endpoint: "/auth/login-handle", body: #"{"error":"supporter-code-used"}"#)))
        XCTAssertTrue(SignupCap.isSupporterCodeUsed(SupporterCodeUsed()))
        XCTAssertFalse(SignupCap.isSupporterCodeUsed(AuthClientError.http(
            status: 403, endpoint: "/auth/login-handle", body: #"{"error":"registration-closed"}"#)))
        XCTAssertFalse(SignupCap.isRegistrationClosed(AuthClientError.http(
            status: 403, endpoint: "/auth/login-handle", body: #"{"error":"supporter-code-used"}"#)))
    }

    func testAHeldPlaceShowsTheFormWhenClosed() {
        XCTAssertEqual(SignupScreenMode.after(open: false, placeHeld: true), .form)
        XCTAssertEqual(SignupScreenMode.after(open: false, placeHeld: false), .closed)
        XCTAssertEqual(SignupScreenMode.after(open: true, placeHeld: false), .form)
    }

    func testTheBindCarriesTheHeldPlace() async throws {
        BindStubURLProtocol.answer(status: 200, body: #"{"bound":true}"#)
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [BindStubURLProtocol.self]
        let client = AuthClient(baseURL: Offline.authBaseURL, session: URLSession(configuration: configuration))
        try await client.bindLoginHandle(
            handle: Data(repeating: 1, count: 32), sessionToken: "s", supporterToken: "place-token")
        XCTAssertEqual(BindStubURLProtocol.seen.last?.method, "PUT")
        XCTAssertEqual(BindStubURLProtocol.seen.last?.token, "place-token")
        XCTAssertEqual(BindStubURLProtocol.seen.last?.bearer, "Bearer s")

        try await client.bindLoginHandle(handle: Data(repeating: 1, count: 32), sessionToken: "s", supporterToken: nil)
        XCTAssertNil(BindStubURLProtocol.seen.last?.token)
    }

    @MainActor
    func testAUsedPlaceComesBackAsSupporterCodeUsedFromTheClaim() async {
        let binder = RefusingBinder(body: #"{"error":"supporter-code-used"}"#)
        do {
            _ = try await claimSignupUsername(
                accountId: Data(count: 16), username: "alice_1", sessionToken: "s",
                directory: UnusedSupporterDirectory(), binder: binder, supporterToken: "tok")
            XCTFail("expected the refusal")
        } catch {
            XCTAssertTrue(error is SupporterCodeUsed)
        }
        XCTAssertEqual(binder.tokens, ["tok"])
    }
}

// MARK: - The sign-up screen

@MainActor
final class SupporterSignupViewModelTests: XCTestCase {

    private final class ScriptedRedeemer: SupporterCodeRedeeming {
        var outcome: SupporterRedeemOutcome
        private(set) var codes: [String] = []
        init(_ outcome: SupporterRedeemOutcome) { self.outcome = outcome }
        func redeem(code: String) async -> SupporterRedeemOutcome {
            codes.append(code)
            return outcome
        }
    }

    private func viewModel(
        open: Bool, store: InMemorySupporterPlaceStore, redeemer: ScriptedRedeemer
    ) -> SignupViewModel {
        SignupOpenStubURLProtocol.answer(status: 200, body: open ? #"{"open":true}"# : #"{"open":false}"#)
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [SignupOpenStubURLProtocol.self]
        let session = URLSession(configuration: configuration)
        let repository = AuthRepository(
            sessionStore: SessionStore(defaults: UserDefaults(suiteName: "supporter-\(UUID().uuidString)")!),
            authClientFactory: { AuthClient(baseURL: Offline.authBaseURL, session: session) },
            persistence: NullSupporterPersistence(),
            nowMs: { 1_000_000 },
            accountDirectory: UnusedSupporterDirectory(),
            restoreDirectory: OfflineRestoreDirectory()
        )
        return SignupViewModel(
            repository: repository,
            supporterPlaces: SupporterPlaceHolder(store: store, currentEpoch: { 7 }),
            supporterRedeemer: redeemer)
    }

    func testAWorkingCodeOpensTheFormWithTheHeldLine() async {
        let store = InMemorySupporterPlaceStore()
        let redeemer = ScriptedRedeemer(.held(HeldSupporterPlace(headerValue: "tok", epoch: 7)))
        let vm = viewModel(open: false, store: store, redeemer: redeemer)
        await vm.onAppear()
        XCTAssertEqual(vm.mode, .closed)

        vm.onOpenSupporterCode()
        XCTAssertEqual(vm.mode, .supporterCode)
        vm.supporterCode = "K7QM-2XRA-9DPF-4WN"
        XCTAssertFalse(vm.canContinueCode)
        vm.supporterCode = "K7QM-2XRA-9DPF-4WNB"
        XCTAssertTrue(vm.canContinueCode)

        await vm.submitSupporterCode()
        XCTAssertEqual(vm.mode, .form)
        XCTAssertTrue(vm.placeHeld)
        XCTAssertEqual(store.place?.headerValue, "tok")
        XCTAssertEqual(redeemer.codes, ["K7QM-2XRA-9DPF-4WNB"])
    }

    func testAFailedCodeKeepsTheFieldAndShowsTheLine() async {
        let store = InMemorySupporterPlaceStore()
        let vm = viewModel(open: false, store: store, redeemer: ScriptedRedeemer(.line(.unknown)))
        await vm.onAppear()
        vm.onOpenSupporterCode()
        vm.supporterCode = "K7QM-2XRA-9DPF-4WNB"
        await vm.submitSupporterCode()
        XCTAssertEqual(vm.mode, .supporterCode)
        XCTAssertEqual(vm.codeLine, .unknown)
        XCTAssertEqual(vm.supporterCode, "K7QM-2XRA-9DPF-4WNB")
        XCTAssertNil(store.place)

        vm.onSupporterCodeBack()
        XCTAssertEqual(vm.mode, .closed)
        XCTAssertNil(vm.codeLine)
    }

    func testAHeldPlaceShowsTheFormOnOpen() async {
        let store = InMemorySupporterPlaceStore(HeldSupporterPlace(headerValue: "tok", epoch: 7))
        let vm = viewModel(open: false, store: store, redeemer: ScriptedRedeemer(.line(.failed)))
        await vm.onAppear()
        XCTAssertEqual(vm.mode, .form)
        XCTAssertTrue(vm.placeHeld)
    }
}

// MARK: - The supporters list

@MainActor
final class SupportersListTests: XCTestCase {

    func testOnlyANewerListReplacesTheHeldOne() {
        let held = HeldSupportersList(listVersion: 5, names: ["A"])
        XCTAssertTrue(SupportersListPolicy.accepts(fetchedVersion: 1, held: nil))
        XCTAssertTrue(SupportersListPolicy.accepts(fetchedVersion: 6, held: held))
        XCTAssertFalse(SupportersListPolicy.accepts(fetchedVersion: 5, held: held))
        XCTAssertFalse(SupportersListPolicy.accepts(fetchedVersion: 4, held: held))
    }

    func testTheRowNeedsANamedList() {
        XCTAssertFalse(SupportersListPolicy.showsRow(nil))
        XCTAssertFalse(SupportersListPolicy.showsRow(HeldSupportersList(listVersion: 1, names: [])))
        XCTAssertTrue(SupportersListPolicy.showsRow(HeldSupportersList(listVersion: 1, names: ["A"])))
    }

    func testTheDailyFetch() {
        let day = SupportersListPolicy.fetchIntervalMillis
        XCTAssertTrue(SupportersListPolicy.isDue(lastFetchMillis: nil, nowMillis: 0))
        XCTAssertFalse(SupportersListPolicy.isDue(lastFetchMillis: 1000, nowMillis: 1000 + day - 1))
        XCTAssertTrue(SupportersListPolicy.isDue(lastFetchMillis: 1000, nowMillis: 1000 + day))
        XCTAssertTrue(SupportersListPolicy.isDue(lastFetchMillis: 5000, nowMillis: 1000), "a clock set back")
    }

    func testTheScreenStates() {
        XCTAssertEqual(SupportersListPolicy.screenState(held: nil, phase: .loading), .loading)
        XCTAssertEqual(SupportersListPolicy.screenState(held: nil, phase: .idle), .loading)
        XCTAssertEqual(SupportersListPolicy.screenState(held: nil, phase: .failed), .failed)
        XCTAssertEqual(
            SupportersListPolicy.screenState(held: HeldSupportersList(listVersion: 1, names: ["A", "B"]), phase: .failed),
            .names(["A", "B"]))
    }

    func testTheListLivesBesideTheReleaseNotice() {
        XCTAssertEqual(
            SupportersListPolicy.listUrl(manifestUrl: URL(string: "https://lakko.me/.well-known/lakko-release")),
            URL(string: "https://lakko.me/.well-known/lakko-supporters"))
        XCTAssertEqual(
            SupportersListPolicy.listUrl(manifestUrl: URL(string: "https://test.lakko.me:8443/x?y=1")),
            URL(string: "https://test.lakko.me:8443/.well-known/lakko-supporters"))
        XCTAssertNil(SupportersListPolicy.listUrl(manifestUrl: nil))
    }

    /// A fake signed file: `version|name,name`; "bad" fails the check.
    private static func verify(_ bytes: Data, _ key: Data) throws -> VerifiedSupportersList {
        let text = String(decoding: bytes, as: UTF8.self)
        guard text != "bad", let bar = text.firstIndex(of: "|"),
              let version = UInt64(text[..<bar]) else { throw Boom() }
        let names = text[text.index(after: bar)...].split(separator: ",").map(String.init)
        return VerifiedSupportersList(listVersion: version, signedAtMs: 0, names: names)
    }

    private final class Script: @unchecked Sendable {
        var answers: [Result<(status: Int, body: Data), Error>] = []
        var count = 0
    }

    private func center(
        key: Data? = Data(count: 32), script: Script, storage: InMemorySupportersListStorage
    ) -> SupportersListCenter {
        SupportersListCenter(
            key: { key },
            url: { URL(string: "https://lakko.me/.well-known/lakko-supporters") },
            fetch: { _ in
                script.count += 1
                return try script.answers.removeFirst().get()
            },
            verify: Self.verify,
            storage: storage,
            defaults: UserDefaults(suiteName: "supporters-\(UUID().uuidString)")!)
    }

    func testAVerifiedListShowsAndIsKept() async {
        let script = Script()
        script.answers = [.success((200, Data("3|Ann,Bo".utf8)))]
        let storage = InMemorySupportersListStorage()
        let c = center(script: script, storage: storage)
        await c.aboutOpened()
        XCTAssertTrue(c.showsRow)
        XCTAssertEqual(c.screenState, .names(["Ann", "Bo"]))
        XCTAssertEqual(storage.bytes, Data("3|Ann,Bo".utf8))
    }

    func testAnOlderOrUnsignedListIsIgnored() async {
        let script = Script()
        script.answers = [.success((200, Data("2|Old".utf8))), .success((200, Data("bad".utf8)))]
        let storage = InMemorySupportersListStorage(Data("3|Ann".utf8))
        let c = center(script: script, storage: storage)
        await c.aboutOpened()
        XCTAssertEqual(c.held, HeldSupportersList(listVersion: 3, names: ["Ann"]))
        await c.aboutOpened()
        XCTAssertEqual(c.held, HeldSupportersList(listVersion: 3, names: ["Ann"]))
        XCTAssertEqual(storage.bytes, Data("3|Ann".utf8))
    }

    func testAFailedFetchWithNothingHeldOffersRetry() async {
        let script = Script()
        script.answers = [.failure(URLError(.notConnectedToInternet)), .success((200, Data("1|Ann".utf8)))]
        let c = center(script: script, storage: InMemorySupportersListStorage())
        await c.aboutOpened()
        XCTAssertFalse(c.showsRow)
        XCTAssertEqual(c.screenState, .failed)
        await c.retry()
        XCTAssertEqual(c.screenState, .names(["Ann"]))
    }

    func testNoPinnedKeyNoListNoFetch() async {
        let script = Script()
        let c = center(key: nil, script: script, storage: InMemorySupportersListStorage(Data("3|Ann".utf8)))
        await c.aboutOpened()
        XCTAssertFalse(c.showsRow)
        XCTAssertEqual(script.count, 0)
    }

    func testTheBackgroundFetchIsDaily() async {
        let script = Script()
        script.answers = [.success((404, Data()))]
        let c = center(script: script, storage: InMemorySupportersListStorage())
        await c.refreshIfDue()
        await c.refreshIfDue()
        XCTAssertEqual(script.count, 1)
        XCTAssertFalse(c.showsRow)
    }
}

// MARK: - Helpers

private struct UnusedSupporterDirectory: AccountDirectory {
    func registerUsername(accountId: Data, username: String, sessionToken: String) async throws -> RegisterResult {
        throw AuthClientError.argument("unused")
    }
    func publishIdentityBundle(
        accountId: Data, signaturePubkey: Data, x25519Pubkey: Data,
        leafAuth: Data?, genesisAttest: Data?, keyResetAttest: Data?, sessionToken: String
    ) async throws {
        throw AuthClientError.argument("unused")
    }
}

private struct NullSupporterPersistence: SessionPersistence {
    func hasDeviceKey(accountIdB64: String) -> Bool { false }
    func accountIdentitySeed(accountIdB64: String) -> Data? { nil }
    func exportKey(accountIdB64: String) -> Data? { nil }
    func sessionToken(accountIdB64: String) -> PersistedSessionToken? { nil }
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {}
    func deleteSessionToken(accountIdB64: String) {}
}

private final class RefusingBinder: LoginHandleBinder {
    let body: String
    private(set) var tokens: [String?] = []
    init(body: String) { self.body = body }

    func bindLoginHandle(handle: Data, sessionToken: String) async throws {
        try await bindLoginHandle(handle: handle, sessionToken: sessionToken, supporterToken: nil)
    }

    func bindLoginHandle(handle: Data, sessionToken: String, supporterToken: String?) async throws {
        tokens.append(supporterToken)
        throw AuthClientError.http(status: 403, endpoint: "/auth/login-handle", body: body)
    }

    func removeLoginHandle(sessionToken: String) async throws -> Bool { true }
}

/// Records method, bearer and anonymous-token header of every request.
final class BindStubURLProtocol: URLProtocol {
    struct Seen { let method: String; let bearer: String?; let token: String? }
    private static let lock = NSLock()
    private static var script: (status: Int, body: String) = (404, "{}")
    private static var _seen: [Seen] = []

    static func answer(status: Int, body: String) {
        lock.withLock {
            script = (status, body)
            _seen = []
        }
    }

    static var seen: [Seen] { lock.withLock { _seen } }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let (status, body) = Self.lock.withLock { () -> (Int, String) in
            Self._seen.append(Seen(
                method: request.httpMethod ?? "GET",
                bearer: request.value(forHTTPHeaderField: "Authorization"),
                token: request.value(forHTTPHeaderField: "x-lakko-anon-token")))
            return Self.script
        }
        guard let url = request.url,
              let response = HTTPURLResponse(
                url: url, statusCode: status, httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"])
        else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}
