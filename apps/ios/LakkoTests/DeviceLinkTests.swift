// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// An in-memory `services/auth` device-request table: one open request per
/// account, every value written once, as the server enforces.
private final class FakeDeviceLinkServer: DeviceLinkRelay {
    struct Row {
        var id: String
        var pakeB: Data?
        var pakeA: Data?
        var sealed: Data?
        var confirmA: Data?
        var outcome: String?
        var confirmB: Data?
        var declined = false
    }

    var row: Row?
    var log: [String] = []
    /// Set to answer the next `answer` with a 409 (another device answered).
    var answeredElsewhere = false
    /// Set to make `openRequest` throw, as an offline or cancelled request does.
    var openFails = false
    private var counter: UInt8 = 0

    func createRequest(handle: Data) async throws -> String {
        counter &+= 1
        let id = base64UrlEncodeNoPad(Data(repeating: counter, count: 16))
        row = Row(id: id)
        log.append("create")
        return id
    }

    func status(requestIdB64: String) async throws -> DeviceLinkStatus {
        guard let row, row.id == requestIdB64, !row.declined else { return DeviceLinkStatus(pending: false) }
        return DeviceLinkStatus(pending: true, pakeB: row.pakeB, outcome: row.outcome, confirmB: row.confirmB)
    }

    func postPayload(requestIdB64: String, sealed: DeviceLinkSealed) async throws {
        log.append("payload")
        guard row?.id == requestIdB64, row?.sealed == nil else { throw AuthClientError.argument("used") }
        row?.pakeA = sealed.pakeA
        row?.sealed = sealed.sealedPayload
        row?.confirmA = sealed.confirmA
    }

    func openRequest(sessionToken: String) async throws -> DeviceLinkPendingRequest? {
        if openFails { throw URLError(.cancelled) }
        guard let row, !row.declined else { return nil }
        return DeviceLinkPendingRequest(
            requestIdB64: row.id, pakeB: row.pakeB, pakeA: row.pakeA,
            sealedPayload: row.sealed, confirmA: row.confirmA, outcome: row.outcome)
    }

    func answer(requestIdB64: String, pakeB: Data, sessionToken: String) async throws -> DeviceLinkAnswerResult {
        log.append("pake")
        if answeredElsewhere { return .handledElsewhere }
        guard row?.id == requestIdB64 else { return .gone }
        guard row?.pakeB == nil else { return .handledElsewhere }
        row?.pakeB = pakeB
        return .answered
    }

    func postResult(requestIdB64: String, confirmB: Data?, sessionToken: String) async throws {
        log.append(confirmB == nil ? "result:wrong_code" : "result:accepted")
        row?.outcome = confirmB == nil ? "wrong_code" : "accepted"
        row?.confirmB = confirmB
    }

    func decline(requestIdB64: String, sessionToken: String) async throws {
        log.append("decline")
        row?.declined = true
    }
}

/// ADR-0049 section 2 (amended 2026-09-28): the device link on iOS, both
/// sides, over the real SPAKE2 core — the happy path, a wrong code, a forged
/// confirmation, the answer limit, another device answering, "It is not me",
/// decline and expiry on the new device.
@MainActor
final class DeviceLinkTests: XCTestCase {

    private let handle = Data(repeating: 0x5C, count: 32)

    private func pairingPayload(handle: Data? = nil) throws -> Data {
        let identity = try Identity.fromSeed(seed: Data(repeating: 0x6D, count: 32))
        return try buildPairingQr(
            loginHandle: handle ?? self.handle,
            keyPackage: identity.createKeyPackage(),
            rendezvousPseudonym: Data(repeating: 0x7E, count: 32))
    }

    private func limit() throws -> DeviceLinkAnswerLimit {
        DeviceLinkAnswerLimit(
            defaults: try XCTUnwrap(UserDefaults(suiteName: "dl-limit-\(UUID().uuidString)")))
    }

    private final class Paired {
        var payloads: [Data] = []
        /// Whether the pairing reports the keys as handed over.
        var keysSent = true
    }

    private func responder(
        _ server: FakeDeviceLinkServer, limit: DeviceLinkAnswerLimit, paired: Paired
    ) -> DeviceLinkResponderFlow {
        let handle = self.handle
        return DeviceLinkResponderFlow(ports: .init(
            relay: server,
            sessionToken: { "bearer" },
            ownHandle: { handle },
            checkPayload: { payload, handle in
                _ = try PairingCodec.parsePairingQrPayload(wireBytes: payload, expectedLoginHandle: handle)
            },
            runPairing: { paired.payloads.append($0); return paired.keysSent },
            limit: limit))
    }

    private func joiner(_ server: FakeDeviceLinkServer, payload: Data, now: @escaping () -> Date = { Date() })
        -> DeviceLinkJoin {
        DeviceLinkJoin(
            ports: .init(relay: server, sleep: { _ in await Task.yield() }, now: now),
            handle: handle, pairingPayload: payload)
    }

    /// Drive the old device until the payload is handled (the modal's timer).
    private func runOldDevice(_ flow: DeviceLinkResponderFlow, until: @escaping () -> Bool) async {
        for _ in 0..<200 where !until() {
            await flow.check()
            await Task.yield()
        }
    }

    func testTheCodeTypedOnTheNewDevicePairsAndBothConfirmationsVerify() async throws {
        let server = FakeDeviceLinkServer()
        let paired = Paired()
        let old = responder(server, limit: try limit(), paired: paired)
        let payload = try pairingPayload()
        let new = joiner(server, payload: payload)

        let start = Task { await new.start() }
        while server.row == nil { await Task.yield() }
        XCTAssertEqual(new.phase, .waiting, "the field is disabled until the other device answers")
        await old.check()
        XCTAssertEqual(old.modal, .confirm, "asked first")
        XCTAssertEqual(new.phase, .waiting, "nothing answered before Yes")
        await old.yesItsMe()
        guard case .code(let shown) = old.modal else { return XCTFail("the code is shown after the 200") }
        while new.phase != .enterCode { await Task.yield() }

        let submit = Task { await new.submit(code: shown) }
        await runOldDevice(old) { !paired.payloads.isEmpty }
        let trusted = await submit.value
        await start.value

        XCTAssertTrue(trusted, "accepted AND the old device's confirmation verified")
        XCTAssertEqual(new.phase, .confirmed)
        XCTAssertEqual(paired.payloads, [payload], "the old device pairs the new device's payload")
        XCTAssertNil(old.modal)
        XCTAssertEqual(server.log, ["create", "pake", "payload", "result:accepted"])
    }

    /// The old device added the new one but could not hand over the keys (it
    /// holds no recovery phrase): it says so instead of closing silently.
    func testADeviceThatCannotHandOverTheKeysSaysSo() async throws {
        let server = FakeDeviceLinkServer()
        let paired = Paired()
        paired.keysSent = false
        let old = responder(server, limit: try limit(), paired: paired)
        let new = joiner(server, payload: try pairingPayload())

        let start = Task { await new.start() }
        while server.row == nil { await Task.yield() }
        await old.check()
        await old.yesItsMe()
        guard case .code(let shown) = old.modal else { return XCTFail("the code is shown after the 200") }
        while new.phase != .enterCode { await Task.yield() }

        let submit = Task { await new.submit(code: shown) }
        await runOldDevice(old) { !paired.payloads.isEmpty }
        _ = await submit.value
        await start.value

        XCTAssertEqual(old.modal, .keysNotSent)
        XCTAssertEqual(DeviceLinkModalCopy.keysNotSent, Localization.text("device_link.keys_not_sent"))
        old.dismiss()
        XCTAssertNil(old.modal)
    }

    func testAWrongCodePairsNothingOnEitherSide() async throws {
        let server = FakeDeviceLinkServer()
        let paired = Paired()
        let old = responder(server, limit: try limit(), paired: paired)
        let new = joiner(server, payload: try pairingPayload())

        let start = Task { await new.start() }
        while server.row == nil { await Task.yield() }
        await old.check()
        await old.yesItsMe()
        guard case .code(let shown) = old.modal else { return XCTFail("no code") }
        while new.phase != .enterCode { await Task.yield() }
        let wrong = String(shown.filter(\.isNumber).map { $0 == "9" ? "0" : "9" })

        let submit = Task { await new.submit(code: wrong) }
        await runOldDevice(old) { old.modal == .wrongCode }
        let trusted = await submit.value

        XCTAssertFalse(trusted)
        XCTAssertEqual(new.phase, .wrongCode)
        XCTAssertEqual(old.modal, .wrongCode)
        XCTAssertTrue(paired.payloads.isEmpty, "nothing was added")
        XCTAssertTrue(server.log.contains("result:wrong_code"))
    }

    /// Whoever answered did not hold the code (a relay forging `accepted`):
    /// the new device must not trust the Welcome.
    func testAForgedConfirmationIsNotTrusted() async throws {
        let server = FakeDeviceLinkServer()
        let new = joiner(server, payload: try pairingPayload())
        let real = try DeviceLinkResponder(requestId: Data(repeating: 1, count: 16), loginHandle: handle)
        let start = Task { await new.start() }
        while server.row == nil { await Task.yield() }
        server.row?.pakeB = real.pakeMessage()
        while new.phase != .enterCode { await Task.yield() }

        let submit = Task { await new.submit(code: "123456") }
        while server.row?.sealed == nil { await Task.yield() }
        server.row?.outcome = "accepted"
        server.row?.confirmB = Data(repeating: 0xAB, count: 32)
        let trusted = await submit.value

        XCTAssertFalse(trusted, "an unverified confirmation is never trusted")
        XCTAssertEqual(new.phase, .wrongCode)
    }

    func testAPayloadForAnotherAccountIsAWrongCode() async throws {
        let server = FakeDeviceLinkServer()
        let paired = Paired()
        let old = responder(server, limit: try limit(), paired: paired)
        let new = joiner(server, payload: try pairingPayload(handle: Data(repeating: 0x01, count: 32)))
        let start = Task { await new.start() }
        while server.row == nil { await Task.yield() }
        await old.check()
        await old.yesItsMe()
        guard case .code(let shown) = old.modal else { return XCTFail("no code") }
        while new.phase != .enterCode { await Task.yield() }
        let submit = Task { await new.submit(code: shown) }
        await runOldDevice(old) { old.modal == .wrongCode }
        _ = await submit.value
        XCTAssertTrue(paired.payloads.isEmpty)
        XCTAssertTrue(server.log.contains("result:wrong_code"))
    }

    func testTheOldDeviceAnswersAtMostFiveRequestsADay() async throws {
        let server = FakeDeviceLinkServer()
        let paired = Paired()
        let limit = try limit()
        let old = responder(server, limit: limit, paired: paired)
        let t0 = Date(timeIntervalSince1970: 1_800_000_000)
        for i in 0..<5 {
            _ = try await server.createRequest(handle: handle)
            await old.check(now: t0.addingTimeInterval(Double(i)))
            await old.yesItsMe(now: t0.addingTimeInterval(Double(i)))
            guard case .code = old.modal else { return XCTFail("request \(i) answered") }
        }
        // Questions left unanswered do not count against the limit.
        _ = try await server.createRequest(handle: handle)
        await old.check(now: t0.addingTimeInterval(9))
        XCTAssertEqual(old.modal, .confirm, "the question is asked whatever the count")
        old.dismiss()
        _ = try await server.createRequest(handle: handle)
        await old.check(now: t0.addingTimeInterval(10))
        await old.yesItsMe(now: t0.addingTimeInterval(10))
        XCTAssertEqual(old.modal, .tooMany)
        XCTAssertEqual(server.log.last, "decline", "over the limit the request is declined")
        XCTAssertEqual(server.log.filter { $0 == "pake" }.count, 5)
        XCTAssertTrue(limit.canAnswer(now: t0.addingTimeInterval(DeviceLinkAnswerLimit.window + 60)),
                      "the window rolls")
        XCTAssertEqual(DeviceLinkAnswerLimit.copy, Localization.text("device_link.request.too_many"))
    }

    func testARequestAnsweredOnAnotherDeviceClosesHere() async throws {
        let server = FakeDeviceLinkServer()
        let old = responder(server, limit: try limit(), paired: Paired())
        _ = try await server.createRequest(handle: handle)
        server.answeredElsewhere = true
        await old.check()
        await old.yesItsMe()
        XCTAssertEqual(old.modal, .handledElsewhere)
    }

    func testItIsNotMeDeclines() async throws {
        let server = FakeDeviceLinkServer()
        let old = responder(server, limit: try limit(), paired: Paired())
        let new = joiner(server, payload: try pairingPayload())
        let start = Task { await new.start() }
        while server.row == nil { await Task.yield() }
        await old.check()
        await old.notMe()
        XCTAssertEqual(old.modal, .declined)
        XCTAssertEqual(server.log.last, "decline")
        XCTAssertFalse(server.log.contains("pake"), "declined at the question: no pake_b")
        await start.value
        XCTAssertEqual(new.phase, .declined, "the new device reads the decline")
        XCTAssertEqual(DeviceLinkJoinCopy.declined, Localization.text("device_link.error.declined"))
    }

    func testTheNewDeviceRequestExpiresAfterTenMinutes() async throws {
        let server = FakeDeviceLinkServer()
        var now = Date(timeIntervalSince1970: 1_800_000_000)
        let new = DeviceLinkJoin(
            ports: .init(relay: server, sleep: { _ in now.addTimeInterval(61) }, now: { now }),
            handle: handle, pairingPayload: try pairingPayload())
        await new.start()
        XCTAssertEqual(new.phase, .expired)
        XCTAssertEqual(DeviceLinkJoinCopy.expired, Localization.text("device_link.code.expired"))
    }

    // MARK: Second security pass, finding 6 — consent before the code

    /// No responder and no `pake_b` before "Yes, it's me".
    func testNothingIsAnsweredBeforeYes() async throws {
        let server = FakeDeviceLinkServer()
        var made = 0
        let handle = self.handle
        let old = DeviceLinkResponderFlow(ports: .init(
            relay: server,
            sessionToken: { "bearer" },
            ownHandle: { handle },
            makeResponder: { id, h in made += 1; return try DeviceLinkResponder(requestId: id, loginHandle: h) },
            checkPayload: { _, _ in },
            runPairing: { _ in true },
            limit: try limit()))
        _ = try await server.createRequest(handle: handle)
        await old.check()
        await old.check()
        XCTAssertEqual(old.modal, .confirm)
        XCTAssertEqual(made, 0, "no responder before consent")
        XCTAssertEqual(server.log, ["create"], "no pake_b before consent")
        XCTAssertNil(server.row?.pakeB)
        await old.yesItsMe()
        XCTAssertEqual(made, 1)
        XCTAssertEqual(server.log, ["create", "pake"])
        guard case .code = old.modal else { return XCTFail("the code after Yes") }
    }

    /// Live walk 2026-09-29: the code step closed a second after "Yes" because
    /// a cancelled poll read as "the request is gone". A failed or cancelled
    /// poll changes nothing; only an answer with no open request closes.
    func testAFailedPollKeepsTheCodeOnScreen() async throws {
        let server = FakeDeviceLinkServer()
        let old = responder(server, limit: try limit(), paired: Paired())
        _ = try await server.createRequest(handle: handle)
        await old.check()
        await old.yesItsMe()
        guard case .code = old.modal else { return XCTFail("the code after Yes") }
        server.openFails = true
        await old.check()
        guard case .code = old.modal else { return XCTFail("a failed poll must not close the code") }
        server.openFails = false
        server.row = nil
        await old.check()
        XCTAssertNil(old.modal, "no open request closes the code")
    }

    /// A failed poll while asking keeps the question too.
    func testAFailedPollKeepsTheQuestion() async throws {
        let server = FakeDeviceLinkServer()
        let old = responder(server, limit: try limit(), paired: Paired())
        _ = try await server.createRequest(handle: handle)
        await old.check()
        XCTAssertEqual(old.modal, .confirm)
        server.openFails = true
        await old.check()
        XCTAssertEqual(old.modal, .confirm)
    }

    /// Dismissing the question without choosing sends nothing, and the same
    /// request is not asked again (it expires).
    func testDismissingTheQuestionSendsNothing() async throws {
        let server = FakeDeviceLinkServer()
        let old = responder(server, limit: try limit(), paired: Paired())
        _ = try await server.createRequest(handle: handle)
        await old.check()
        XCTAssertEqual(old.modal, .confirm)
        old.dismiss()
        XCTAssertNil(old.modal)
        await old.check()
        XCTAssertNil(old.modal, "not asked again")
        XCTAssertEqual(server.log, ["create"], "nothing sent: no pake_b, no decline")
        await old.yesItsMe()
        XCTAssertEqual(server.log, ["create"], "Yes after a dismissal does nothing")
    }

    /// A request answered on another device while this one is asking closes
    /// the question.
    func testAQuestionAnsweredElsewhereCloses() async throws {
        let server = FakeDeviceLinkServer()
        let old = responder(server, limit: try limit(), paired: Paired())
        _ = try await server.createRequest(handle: handle)
        await old.check()
        server.row?.pakeB = Data(repeating: 1, count: 33)
        await old.check()
        XCTAssertEqual(old.modal, .handledElsewhere)
    }

    func testTheQuestionCopy() {
        XCTAssertEqual(
            DeviceLinkModalCopy.askBody,
            Localization.text("device_link.request.someone_signing_in") + "\n"
                + Localization.text("device_link.request.title"))
        XCTAssertEqual(DeviceLinkModalCopy.yes, Localization.text("device_link.request.yes_its_me"))
        XCTAssertEqual(DeviceLinkModalCopy.notMe, Localization.text("device_link.request.decline"))
        XCTAssertEqual(DeviceLinkModalCopy.body, Localization.text("device_link.request.enter_code"))
        for key in ["device_link.request.someone_signing_in", "device_link.request.yes_its_me",
                    "device_link.request.enter_code"] {
            XCTAssertNotEqual(Localization.text(key), key, "\(key) is missing from the catalog")
        }
    }

    func testTheEntryKeepsSixDigitsOnly() {
        XCTAssertEqual(DeviceLinkCodeEntry.digits("47 19-03"), "471903")
        XCTAssertEqual(DeviceLinkCodeEntry.digits("1234567"), "123456")
        XCTAssertEqual(try deviceLinkFormatCode(code: "471903"), "47 19 03")
    }
}
