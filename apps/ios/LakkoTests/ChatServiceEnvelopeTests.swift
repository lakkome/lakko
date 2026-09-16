// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// ADR-0045 — the FOREGROUND receive loop classifies a committer-liveness
/// beacon as a service envelope, never as a chat bubble.
///
/// ## The defect this pins
///
/// The beacon is a 27-byte, field-less envelope (`domain(26) || version(1)`)
/// every owner and admin emits on a fixed interval. ``ChatClient/toEvent``
/// filtered exactly two service-envelope kinds — call signaling and the
/// ADR-0042 / ADR-0044 push-routing secret — and then fell through to
/// `parseBodyEnvelope`, whose lossy-UTF-8 fallback is CORRECT on its own terms
/// (pre-envelope legacy plaintext must still render) and therefore turns the
/// beacon into a visible bubble reading `lakko/committer-beacon/v1` plus two
/// control bytes. ``beaconBytesWouldRenderAsAChatBubbleWithoutTheArm`` below
/// runs the real parser on the real beacon and shows exactly that, so the
/// arm's necessity is demonstrated rather than asserted.
///
/// ## Why there are no doubles here
///
/// The trap in this area is a stand-in whose parse THROWS on binary input
/// where the real `parse_body` DEGRADES. Such a double makes every case below
/// pass with the filter arm deleted, because the beacon would take the
/// (nonexistent) throw path instead of becoming a bubble. This suite sidesteps
/// the question entirely: it drives the REAL ``ChatClient/toEvent`` with a REAL
/// `ProcessedMessage.application`, over envelopes composed by the REAL FFI
/// (`composeCommitterBeacon` / `composePushRoutingSecret` /
/// `composeEnvelope`). Nothing about envelope semantics is modelled, so
/// there is no double to be unfaithful.
///
/// That also settles the fixture-hygiene rule: the beacon's domain separator
/// ends in a NUL byte, and the fixture here is `composeCommitterBeacon()` —
/// the actual producer — so no NUL is ever written into this file. (The sibling
/// ``NseServiceEnvelopeTests`` must reimplement the gates, because the NSE
/// deliberately links a slim FFI; the foreground has the whole surface and
/// therefore no such excuse.)
///
/// ## Discrimination (verified by running the broken variant)
///
/// With the `looksLikeCommitterBeacon` arm removed from `toEvent`, five of the
/// eleven cases fail:
///   - `testBeaconIsClassifiedAsABeaconNeverAChatEvent` FAILS with
///     `failed - a committer beacon must classify as .committerBeacon, got
///     application(messageId: 1, text: "lakko/committer-beacon/v1...", ...)`;
///   - `testBeaconNeverReachesTheTimelineAsText` FAILS with
///     `XCTAssertFalse failed - the beacon rendered as a chat bubble reading
///     'lakko/committer-beacon/v1 '` -- the visible bubble text itself;
///   - `testBeaconCarriesSenderLeafAndTimestamp` and
///     `testBeaconFromAnUnattributableSenderStillNeverBecomesABubble` FAIL with
///     `expected .committerBeacon, got application(...)`;
///   - `testEachEnvelopeKindClassifiesIndependently` FAILS with
///     `row 1: application(messageId: 1, text: "lakko/committer-beacon/v1...")`.
/// The remaining six pass either way and say so where they sit: they are the
/// regression guards for the two arms that already existed, plus the two
/// property pins (the lossy-fallback demonstration and the FFI-length
/// agreement) that describe the world the arm operates in rather than the arm.
final class ChatServiceEnvelopeTests: XCTestCase {

    // MARK: - Helpers

    /// Run one plaintext through the real classification chain, as a DS row
    /// would arrive after MLS decrypt + role verification.
    private func classify(
        _ plaintext: Data,
        senderLeaf: UInt32? = 3,
        messageId: Int64 = 1
    ) -> IncomingChatEvent {
        ChatClient.toEvent(
            messageId: messageId,
            createdAt: "2026-08-24T00:00:00Z",
            processed: .application(data: plaintext, senderLeaf: senderLeaf, senderAccount: nil),
            // ADR-0023 — the receive loop resolves the sender leaf to a
            // signature key against the epoch's roster. Nothing here is about
            // WHO sent the envelope, only about which arm classifies it, so
            // this stands in for a roster with a single legible entry.
            resolveSender: { "member-\($0)" }
        )
    }

    /// The user-visible text an event would put on the timeline, or `nil` for
    /// an event that renders none.
    private func timelineText(_ event: IncomingChatEvent) -> String? {
        if case .application(_, let text, _, _, _) = event { return text }
        return nil
    }

    /// A real ADR-0023 chat envelope from the real composer.
    private func chatEnvelope(_ text: String) throws -> Data {
        try composeEnvelope(text: text, attachments: [])
    }

    /// First byte of a `CallSignalingPayload` — field 12 (candidate), tag
    /// `(12 << 3) | 2 = 0x62`. The call-signaling arm keys on exactly this
    /// byte set; nothing past it is parsed by `toEvent`.
    private let callSignalingBytes = Data([0x62, 0x02, 0x08, 0x01])

    // MARK: - The beacon arm

    func testBeaconIsClassifiedAsABeaconNeverAChatEvent() {
        let event = classify(composeCommitterBeacon())

        guard case .committerBeacon = event else {
            return XCTFail("a committer beacon must classify as .committerBeacon, got \(event)")
        }
    }

    func testBeaconNeverReachesTheTimelineAsText() {
        let event = classify(composeCommitterBeacon())

        // Whatever the classification, no event this loop emits may carry the
        // domain separator as user-visible text. Written against the rendered
        // text rather than the case so it keeps discriminating if the arm is
        // ever re-routed to a different case.
        let rendered = timelineText(event) ?? ""
        XCTAssertFalse(
            rendered.contains("committer-beacon"),
            "the beacon rendered as a chat bubble reading '\(rendered)'"
        )
    }

    func testBeaconCarriesSenderLeafAndTimestamp() {
        // ADR-0045: the beacon deliberately names neither its sender nor its
        // age, so the fold's only sources are the MLS-authenticated leaf and
        // the DS row. Surfacing both is the reason this is an event and not a
        // silent drop inside the parser.
        let event = classify(composeCommitterBeacon(), senderLeaf: 7, messageId: 42)

        guard case .committerBeacon(let messageId, let senderLeaf, let createdAt) = event else {
            return XCTFail("expected .committerBeacon, got \(event)")
        }
        XCTAssertEqual(messageId, 42)
        XCTAssertEqual(senderLeaf, 7)
        XCTAssertEqual(createdAt, "2026-08-24T00:00:00Z")
    }

    func testBeaconFromAnUnattributableSenderStillNeverBecomesABubble() {
        // A control frame / external sender surfaces no leaf. The fold must
        // treat that as unusable (an unattributable beacon proves no candidate
        // is live), but it is still emphatically not a chat message.
        let event = classify(composeCommitterBeacon(), senderLeaf: nil)

        guard case .committerBeacon(_, let senderLeaf, _) = event else {
            return XCTFail("expected .committerBeacon, got \(event)")
        }
        XCTAssertNil(senderLeaf)
    }

    func testBeaconBytesWouldRenderAsAChatBubbleWithoutTheArm() {
        // The defect, demonstrated on the real parser: `parse_body`'s
        // lossy-UTF-8 fallback DEGRADES binary input to `.chat` — it does NOT
        // throw — which is both correct for legacy plaintext and exactly why
        // the beacon needs its own arm ahead of it. This is also the check
        // that this suite is faithful: the real parser's behaviour here is the
        // thing a naive double would get wrong.
        let parsed = AttachmentBridge.parseBodyEnvelopeBytes(composeCommitterBeacon())

        XCTAssertEqual(parsed.kind, .chat)
        XCTAssertTrue(
            parsed.text.contains("lakko/committer-beacon/v1"),
            "expected the lossy fallback text, got '\(parsed.text)'"
        )
    }

    func testUnknownBeaconVersionIsNotSwallowedByTheArm() {
        // Fail-closed on the version byte: the arm claims only what this
        // build's codec accepts. A future v2 beacon falls through to the chat
        // parser like any other unrecognised plaintext rather than being
        // silently eaten by a length-only match — which is what keeps the
        // cheap peek honest about being backed by the codec.
        var notABeacon = composeCommitterBeacon()
        notABeacon[notABeacon.count - 1] = 0xFF

        let event = classify(notABeacon)

        guard case .application = event else {
            return XCTFail("an unknown beacon version must fall through to chat, got \(event)")
        }
    }

    func testTheLengthTheArmReadsFromTheFfiMatchesTheWire() {
        // The arm sizes its peek with `committerBeaconEnvelopeLen()` rather
        // than a local `27`. With the real core there is nothing to
        // monkeypatch, so what is pinned here is the agreement itself: if the
        // envelope ever grows and the constant does not follow (or vice
        // versa), this fails and the peek stops matching real beacons.
        XCTAssertEqual(Int(committerBeaconEnvelopeLen()), composeCommitterBeacon().count)
    }

    // MARK: - The pre-existing arms still work

    func testARealChatMessageIsStillAChatEvent() throws {
        let event = classify(try chatEnvelope("hello"))

        guard case .application(_, let text, _, _, _) = event else {
            return XCTFail("expected .application, got \(event)")
        }
        XCTAssertEqual(text, "hello")
    }

    func testCallSignalingIsStillLiftedOutOfTheTimeline() {
        let event = classify(callSignalingBytes)

        guard case .callSignaling = event else {
            return XCTFail("expected .callSignaling, got \(event)")
        }
    }

    func testPushRoutingSecretIsStillLiftedOutOfTheTimeline() throws {
        let secret = Data(repeating: 0x5A, count: 32)
        let event = classify(try composePushRoutingSecret(mintEpoch: 11, secret: secret))

        guard case .pushRoutingSecret(_, let decoded, let mintEpoch, _, _) = event else {
            return XCTFail("expected .pushRoutingSecret, got \(event)")
        }
        XCTAssertEqual(decoded, secret)
        XCTAssertEqual(mintEpoch, 11)
    }

    func testEachEnvelopeKindClassifiesIndependently() throws {
        // The arms run per row; a beacon must not shadow the chat message next
        // to it in a poll batch, nor be shadowed by it.
        let kinds = [
            try chatEnvelope("before"),
            composeCommitterBeacon(),
            try chatEnvelope("after"),
        ].map { classify($0) }

        guard case .application = kinds[0] else { return XCTFail("row 0: \(kinds[0])") }
        guard case .committerBeacon = kinds[1] else { return XCTFail("row 1: \(kinds[1])") }
        guard case .application = kinds[2] else { return XCTFail("row 2: \(kinds[2])") }
    }
}
