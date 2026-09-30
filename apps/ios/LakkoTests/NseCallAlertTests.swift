// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
// Only for the wording cross-check at the bottom: the extension's own decrypt
// core below is compiled straight into THIS target, so nothing else here comes
// from the app module.
@testable import Lakko
import LakkoCore

/// The notification-service extension's call arm (`docs/ux/calls.md` sections
/// 2.1, 2.2 and 6): a call offer becomes a call-style alert naming the caller
/// and its kind, a call end takes that alert down, and a build whose slim FFI
/// carries no call parse suppresses call rows exactly as the extension always
/// did.
///
/// Drives the pure core (`NsePushDecrypt.swift`, compiled straight into this
/// target) with hand-rolled fakes — no framework, no Keychain, no DS, no
/// notification centre.
///
/// The read-only-throwaway discipline (C-59) is asserted here too: the call arm
/// adds no write path, and the throwaway group is still always closed.
final class NseCallAlertTests: XCTestCase {

    private let groupHex = String(repeating: "a", count: 64)
    private let groupIdBytes = Data(repeating: 0xAA, count: 32)
    private let accountId = "acct-b64=="
    private let callIdHex = "0102030405060708090a0b0c0d0e0f10"
    private let otherCallIdHex = "ff02030405060708090a0b0c0d0e0f10"

    // MARK: - Fakes

    private final class FakeGroup: NseThrowawayGroup {
        private let decode: (Data) -> NseProcessedRow?
        var closed = false

        init(decode: @escaping (Data) -> NseProcessedRow?) {
            self.decode = decode
        }

        func process(_ payload: Data) -> NseProcessedRow? { decode(payload) }
        func dsProofHeader(method: String, path: String, query: String, body: Data) -> String? { nil }
        func close() { closed = true }
    }

    private final class FakeDeps: NsePushDecryptDeps {
        let account: NseResolvedAccount
        let group: FakeGroup
        let rows: [NseFetchedRow]
        let mirroredName: String?
        let muted: Bool

        init(
            account: NseResolvedAccount,
            group: FakeGroup,
            rows: [NseFetchedRow],
            mirroredName: String? = nil,
            muted: Bool = false
        ) {
            self.account = account
            self.group = group
            self.rows = rows
            self.mirroredName = mirroredName
            self.muted = muted
        }

        func lookupRoute(pushGidHex: String) -> (accountIdB64: String, groupIdHex: String)? { nil }
        func resolveAccount(groupHex: String) -> NseResolvedAccount? { account }
        func loadIdentitySeed(accountIdB64: String) -> Data? { Data(repeating: 1, count: 64) }
        func loadSnapshot(accountIdB64: String) -> Data? { Data(repeating: 2, count: 8) }
        func openThrowawayGroup(
            seed: Data,
            snapshot: Data,
            groupIdBytes: Data
        ) throws -> NseThrowawayGroup? { group }
        func dsFetch(
            groupIdBytes: Data, afterId: Int64, group: NseThrowawayGroup
        ) async throws -> [NseFetchedRow] { rows }
        func startCursor(accountIdB64: String, groupHex: String) -> Int64? { nil }
        func isAntiCensorshipArmed() -> Bool { false }
        func callerName(accountIdB64: String, groupIdHex: String) -> String? { mirroredName }
        func isMuted(accountIdB64: String, groupIdHex: String) -> Bool { muted }
    }

    /// A parser that answers from a canned table keyed on the row's first byte.
    private struct FakeCallParser: NseCallSignalingParser {
        let answers: [UInt8: NseCallSignal]

        func parse(_ bytes: Data) -> NseCallSignal? {
            guard let first = bytes.first else { return nil }
            return answers[first]
        }
    }

    /// The clock these tests place offers against. A fixed value rather than
    /// `Date()`, so "fresh" and "past the window" are stated by the fixture
    /// and not by how long the suite took to run.
    private let nowMs: Int64 = 1_800_000_000_000

    /// A row stamped `ageMs` old by the DS, which is the only clock the ring
    /// window is measured against (`docs/ux/calls.md` section 2.1). Fresh
    /// unless a test says otherwise.
    private func row(_ id: Int64, _ tag: UInt8, ageMs: Int64 = 1_000) -> NseFetchedRow {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return NseFetchedRow(
            id: id,
            payload: Data([tag]),
            createdAt: f.string(from: Date(timeIntervalSince1970: TimeInterval(nowMs - ageMs) / 1000))
        )
    }

    private func wake() -> NsePushWake {
        NsePushWake(groupHex: groupHex, pointer: 10)
    }

    private func deps(
        rows: [NseFetchedRow],
        decode: @escaping (Data) -> NseProcessedRow?,
        mirroredName: String? = nil,
        muted: Bool = false
    ) -> (FakeDeps, FakeGroup) {
        let group = FakeGroup(decode: decode)
        let deps = FakeDeps(
            account: NseResolvedAccount(accountIdB64: accountId, groupIdBytes: groupIdBytes),
            group: group,
            rows: rows,
            mirroredName: mirroredName,
            muted: muted
        )
        return (deps, group)
    }

    // MARK: - A muted conversation does not ring the lock screen

    private func offerDecode(video: Bool = false) -> (Data) -> NseProcessedRow? {
        { _ in
            NseProcessedRow(
                kind: .callOffer(video: video),
                text: "",
                attachmentCount: 0,
                callIdHex: self.callIdHex
            )
        }
    }

    /// `docs/ux/calls.md` section 2.1. Until this, the extension's only mention
    /// of mute was a comment claiming a muted chat was never woken — which
    /// nothing implemented, so a muted chat rang a closed phone at full volume.
    func testAMutedConversationYieldsASilentAlertRatherThanARingingOne() async {
        let (deps, _) = deps(
            rows: [row(11, 0x01)],
            decode: offerDecode(),
            mirroredName: "Anna",
            muted: true
        )

        let outcome = await decryptPushOutcome(wake(), deps, nowMs: nowMs)

        guard case .callSilent(let alert) = outcome else {
            return XCTFail("expected a silent call alert, got \(outcome)")
        }
        // Still says who called and what kind of call it was: only the
        // interruption is withheld, never the record.
        XCTAssertEqual(alert.title, "Anna")
        XCTAssertEqual(alert.body, "Voice call")
        XCTAssertEqual(alert.callIdHex, callIdHex)
    }

    /// The same conversation unmuted rings, so the silence above is the mute
    /// flag and not something else about the fixture.
    func testTheSameOfferRingsWhenTheConversationIsNotMuted() async {
        let (deps, _) = deps(
            rows: [row(11, 0x01)],
            decode: offerDecode(),
            mirroredName: "Anna",
            muted: false
        )
        guard case .callOffer = await decryptPushOutcome(wake(), deps, nowMs: nowMs) else {
            return XCTFail("an unmuted conversation must still ring")
        }
    }

    /// A wake whose newest call row is an offer the caller has already given
    /// up on. The fetch is a catch-up from the app's cursor, so this is
    /// ordinary rather than exotic — and a phone that rings for it offers an
    /// answer that connects to nobody.
    func testAnOfferPastTheRingWindowIsAMissedCallRatherThanARing() async {
        let past = Int64(LakkoCore.callRingWindowMs()) + 1_000
        let (deps, _) = deps(
            rows: [row(11, 0x01, ageMs: past)],
            decode: offerDecode(),
            mirroredName: "Anna"
        )

        let outcome = await decryptPushOutcome(wake(), deps, nowMs: nowMs)

        guard case .callSilent(let alert) = outcome else {
            return XCTFail("expected a silent missed call, got \(outcome)")
        }
        XCTAssertEqual(alert.title, "Anna")
        XCTAssertEqual(alert.body, "Missed call")
    }

    /// An offer one millisecond inside the window still rings: the boundary
    /// belongs to the caller giving up, not to this side giving up early.
    func testAnOfferJustInsideTheWindowStillRings() async {
        let (deps, _) = deps(
            rows: [row(11, 0x01, ageMs: Int64(LakkoCore.callRingWindowMs()) - 1)],
            decode: offerDecode()
        )
        guard case .callOffer = await decryptPushOutcome(wake(), deps, nowMs: nowMs) else {
            return XCTFail("an offer inside the window must ring")
        }
    }

    /// A muted call that is ALSO over says it was missed, not that it is
    /// happening: the two quiet answers are different sentences.
    func testAMutedCallThatIsAlsoOverSaysItWasMissed() async {
        let past = Int64(LakkoCore.callRingWindowMs()) + 1_000
        let (deps, _) = deps(
            rows: [row(11, 0x01, ageMs: past)],
            decode: offerDecode(),
            muted: true
        )
        guard case .callSilent(let alert) = await decryptPushOutcome(wake(), deps, nowMs: nowMs) else {
            return XCTFail("expected a silent missed call")
        }
        XCTAssertEqual(alert.body, "Missed call")
    }

    // MARK: - An offer rings

    func testACallOfferBecomesANamedCallAlert() async {
        let (deps, group) = deps(
            rows: [row(11, 0x01)],
            decode: { _ in
                NseProcessedRow(
                    kind: .callOffer(video: false),
                    text: "",
                    attachmentCount: 0,
                    callIdHex: self.callIdHex
                )
            },
            mirroredName: "Anna"
        )

        let outcome = await decryptPushOutcome(wake(), deps, nowMs: nowMs)

        guard case .callOffer(let alert) = outcome else {
            return XCTFail("expected a call alert, got \(outcome)")
        }
        XCTAssertEqual(alert.title, "Anna")
        XCTAssertEqual(alert.body, "Voice call")
        XCTAssertEqual(alert.callIdHex, callIdHex)
        XCTAssertEqual(alert.tag, groupHex)
        // The throwaway group is discarded on the call path exactly as on the
        // message path (C-59).
        XCTAssertTrue(group.closed)
    }

    func testAVideoOfferSaysVideoCall() async {
        let (deps, _) = deps(
            rows: [row(11, 0x01)],
            decode: { _ in
                NseProcessedRow(
                    kind: .callOffer(video: true),
                    text: "",
                    attachmentCount: 0,
                    callIdHex: self.callIdHex
                )
            },
            mirroredName: "Anna"
        )

        guard case .callOffer(let alert) = await decryptPushOutcome(wake(), deps, nowMs: nowMs) else {
            return XCTFail("expected a call alert")
        }
        XCTAssertEqual(alert.body, "Video call")
    }

    func testAnUnmirroredCallerIsAnnouncedWithoutAName() async {
        let (deps, _) = deps(
            rows: [row(11, 0x01)],
            decode: { _ in
                NseProcessedRow(
                    kind: .callOffer(video: false),
                    text: "",
                    attachmentCount: 0,
                    callIdHex: self.callIdHex
                )
            },
            mirroredName: nil
        )

        guard case .callOffer(let alert) = await decryptPushOutcome(wake(), deps, nowMs: nowMs) else {
            return XCTFail("expected a call alert")
        }
        XCTAssertEqual(alert.title, "Incoming call")
    }

    // MARK: - An end cancels

    func testAnEndedCallCancelsRatherThanRings() async {
        // The caller gave up while the phone was asleep: both rows arrive in
        // one fetch, and what the user must NOT get is a ring for a call that
        // is already over.
        let (deps, _) = deps(
            rows: [row(11, 0x01), row(12, 0x02)],
            decode: { payload in
                payload.first == 0x01
                    ? NseProcessedRow(
                        kind: .callOffer(video: false),
                        text: "",
                        attachmentCount: 0,
                        callIdHex: self.callIdHex)
                    : NseProcessedRow(
                        kind: .callEnd,
                        text: "",
                        attachmentCount: 0,
                        callIdHex: self.callIdHex)
            }
        )

        let outcome = await decryptPushOutcome(wake(), deps, nowMs: nowMs)
        XCTAssertEqual(outcome, .callEnded(callIdHex: callIdHex))
    }

    func testTheEndNamesTheCallItEnded() async {
        let (deps, _) = deps(
            rows: [row(11, 0x02)],
            decode: { _ in
                NseProcessedRow(
                    kind: .callEnd,
                    text: "",
                    attachmentCount: 0,
                    callIdHex: self.otherCallIdHex
                )
            }
        )

        let outcome = await decryptPushOutcome(wake(), deps, nowMs: nowMs)
        XCTAssertEqual(outcome, .callEnded(callIdHex: otherCallIdHex))
    }

    // MARK: - A call outranks a message

    func testACallOfferWinsOverANewerMessage() async {
        let (deps, _) = deps(
            rows: [row(11, 0x01), row(12, 0x03)],
            decode: { payload in
                payload.first == 0x01
                    ? NseProcessedRow(
                        kind: .callOffer(video: false),
                        text: "",
                        attachmentCount: 0,
                        callIdHex: self.callIdHex)
                    : NseProcessedRow(kind: .chat, text: "hi", attachmentCount: 0)
            },
            mirroredName: "Anna"
        )

        guard case .callOffer = await decryptPushOutcome(wake(), deps, nowMs: nowMs) else {
            return XCTFail("a call expires in seconds; the message is still there afterwards")
        }
    }

    func testAMessageStillPreviewsWhenNoCallIsInTheBatch() async {
        let (deps, _) = deps(
            rows: [row(11, 0x03)],
            decode: { _ in NseProcessedRow(kind: .chat, text: "hi", attachmentCount: 0) }
        )

        guard case .preview(let preview) = await decryptPushOutcome(wake(), deps, nowMs: nowMs) else {
            return XCTFail("expected a message preview")
        }
        XCTAssertEqual(preview.body, "hi")
    }

    func testTheLegacyPreviewEntryPointIgnoresCalls() async {
        // `decryptPushPreview` stays the name for the message half, so the
        // ADR-0041 callers and their tests are untouched by calls existing.
        let (deps, _) = deps(
            rows: [row(11, 0x01)],
            decode: { _ in
                NseProcessedRow(
                    kind: .callOffer(video: false),
                    text: "",
                    attachmentCount: 0,
                    callIdHex: self.callIdHex
                )
            }
        )

        let preview = await decryptPushPreview(wake(), deps)
        XCTAssertNil(preview)
    }

    // MARK: - Classification

    func testCallSignalingIsClassifiedByTheInjectedParser() {
        let offer = Data([0x52, 0x00])
        let parser = FakeCallParser(answers: [
            0x52: .offer(callIdHex: callIdHex, video: true)
        ])

        let row = nseClassifyCallSignaling(offer, parser: parser)

        XCTAssertEqual(row?.kind, .callOffer(video: true))
        XCTAssertEqual(row?.callIdHex, callIdHex)
    }

    func testAnEndIsClassifiedAsAnEnd() {
        let end = Data([0x6A, 0x00])
        let parser = FakeCallParser(answers: [0x6A: .end(callIdHex: callIdHex)])

        XCTAssertEqual(nseClassifyCallSignaling(end, parser: parser)?.kind, .callEnd)
    }

    func testAnAnswerOrCandidateIsNotAUserVisibleCallEvent() {
        let answer = Data([0x5A, 0x00])
        let parser = FakeCallParser(answers: [0x5A: .other])

        XCTAssertNil(nseClassifyCallSignaling(answer, parser: parser))
    }

    func testAChatMessageIsNeverMistakenForACall() {
        // A JSON envelope opens with `{`; the byte peek refuses it before the
        // parser is ever consulted.
        let chat = Data("{\"v\":1}".utf8)
        let parser = FakeCallParser(answers: [0x7B: .offer(callIdHex: callIdHex, video: false)])

        XCTAssertNil(nseClassifyCallSignaling(chat, parser: parser))
    }

    func testWithNoParserEveryCallRowIsSuppressedAsBefore() {
        // The state of a build whose slim FFI carries no call parse yet: quiet,
        // not wrong. A row nobody can read must never ring a phone.
        XCTAssertNil(nseClassifyCallSignaling(Data([0x52, 0x00]), parser: nil))
    }

    func testAParserThatCannotReadTheBytesSuppresses() {
        XCTAssertNil(
            nseClassifyCallSignaling(Data([0x52, 0x00]), parser: FakeCallParser(answers: [:]))
        )
    }

    // MARK: - What an ended call says (section 6)

    func testAnAlertStillOnScreenMeansAMissedCall() {
        let content = nseEndedCallContent(hadDeliveredAlert: true, callerName: "Anna")
        XCTAssertEqual(content.title, "Anna")
        XCTAssertEqual(content.body, "Missed call")
        XCTAssertFalse(content.passive)
    }

    func testAMissedCallWithNoNameStillSaysSomething() {
        let content = nseEndedCallContent(hadDeliveredAlert: true, callerName: nil)
        XCTAssertEqual(content.title, "Incoming call")
        XCTAssertEqual(content.body, "Missed call")
    }

    func testACallAlreadyDealtWithSaysNothingAndStaysQuiet() {
        // Answered or declined here: the app took the alert down, so there is
        // nothing to tell the user. An extension cannot suppress a notification
        // outright, and `passive` is the quietest it can make one.
        let content = nseEndedCallContent(hadDeliveredAlert: false, callerName: "Anna")
        XCTAssertTrue(content.title.isEmpty)
        XCTAssertTrue(content.body.isEmpty)
        XCTAssertTrue(content.passive)
    }

    // MARK: - Wording

    func testTheExtensionAndTheAppAgreeOnTheCallKindWording() {
        // Two processes, one wording (the extension links neither the app
        // module nor the full FFI, so it carries its own copy). This is the
        // test that keeps the copy honest.
        XCTAssertEqual(nseCallKindLabel(video: false), CallKindLabel.text(video: false))
        XCTAssertEqual(nseCallKindLabel(video: true), CallKindLabel.text(video: true))
        XCTAssertEqual(nseUnknownCallerTitle, IncomingCallReport.unknownCaller)
    }
}
