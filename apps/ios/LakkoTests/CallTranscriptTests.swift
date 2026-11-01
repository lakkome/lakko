// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

/// `docs/ux/calls.md` section 6 — what a finished call leaves behind.
///
/// The defect these pin: a call ended, the screen closed, and the conversation
/// looked exactly as it had before anyone picked up.
final class CallTranscriptTests: XCTestCase {

    private func row(
        _ direction: CallTranscriptRow.Direction,
        _ outcome: CallTranscriptRow.Outcome
    ) -> CallTranscriptRow {
        CallTranscriptRow(direction: direction, outcome: outcome)
    }

    // MARK: - The four sentences

    func testAnAnsweredOutgoingCallReadsAsItsDuration() {
        XCTAssertEqual(
            l10nPlain(row(.outgoing, .answered(durationMs: 724_000)).text), "Outgoing call, 12:04")
    }

    func testAnAnsweredIncomingCallReadsAsItsDuration() {
        XCTAssertEqual(
            l10nPlain(row(.incoming, .answered(durationMs: 724_000)).text), "Incoming call, 12:04")
    }

    func testAMissedCallSaysSoInBothDirections() {
        XCTAssertEqual(row(.incoming, .missed).text, "Missed call")
        XCTAssertEqual(row(.outgoing, .missed).text, "Missed call")
    }

    func testADeclinedCallSaysSoInBothDirections() {
        XCTAssertEqual(row(.incoming, .declined).text, "Declined")
        XCTAssertEqual(row(.outgoing, .declined).text, "Declined")
    }

    // MARK: - Durations

    func testDurationsAreWrittenAsMinutesAndSeconds() {
        XCTAssertEqual(CallTranscriptRow.durationLabel(ms: 0), "0:00")
        XCTAssertEqual(CallTranscriptRow.durationLabel(ms: 999), "0:00")
        XCTAssertEqual(CallTranscriptRow.durationLabel(ms: 1_000), "0:01")
        XCTAssertEqual(CallTranscriptRow.durationLabel(ms: 61_000), "1:01")
        XCTAssertEqual(CallTranscriptRow.durationLabel(ms: 3_600_000), "1:00:00")
        XCTAssertEqual(CallTranscriptRow.durationLabel(ms: 3_723_000), "1:02:03")
    }

    /// A clock that ran backwards cannot produce a negative call length.
    func testANegativeDurationReadsAsZero() {
        XCTAssertEqual(CallTranscriptRow.durationLabel(ms: -5_000), "0:00")
    }

    /// Section 8 — a duration is announced as a duration, never digit by digit.
    func testTheSpokenDurationIsWordsNotDigits() {
        XCTAssertEqual(l10nPlain(CallTranscriptRow.spokenDuration(ms: 724_000)), "12 minutes 4 seconds")
        XCTAssertEqual(l10nPlain(CallTranscriptRow.spokenDuration(ms: 61_000)), "1 minute 1 second")
        XCTAssertEqual(CallTranscriptRow.spokenDuration(ms: 0), "0 seconds")
        XCTAssertEqual(CallTranscriptRow.spokenDuration(ms: 120_000), "2 minutes")
        XCTAssertEqual(
            l10nPlain(CallTranscriptRow.spokenDuration(ms: 3_723_000)), "1 hour 2 minutes 3 seconds")
        XCTAssertEqual(
            l10nPlain(row(.incoming, .missed).accessibilityLabel), "Missed call. Call back")
    }

    // MARK: - Outcome

    func testACallThatCarriedMediaIsAlwaysAnswered() {
        for cause in [
            CallTranscriptRow.Cause.hungUp, .declined, .timedOut, .failed, .busy,
        ] {
            XCTAssertEqual(
                CallTranscriptRow.outcome(
                    connectedAtMs: 1_000, endedAtMs: 61_000, cause: cause),
                .answered(durationMs: 60_000),
                "a connected call ending as \(cause.rawValue) is still a call that happened")
        }
    }

    func testARingThatRanOutIsMissed() {
        XCTAssertEqual(
            CallTranscriptRow.outcome(connectedAtMs: nil, endedAtMs: 1, cause: .timedOut),
            .missed)
    }

    func testACallCancelledBeforeItConnectedIsMissed() {
        XCTAssertEqual(
            CallTranscriptRow.outcome(connectedAtMs: nil, endedAtMs: 1, cause: .hungUp),
            .missed)
    }

    func testATransportThatNeverCameUpIsMissed() {
        XCTAssertEqual(
            CallTranscriptRow.outcome(connectedAtMs: nil, endedAtMs: 1, cause: .failed),
            .missed)
        XCTAssertEqual(
            CallTranscriptRow.outcome(connectedAtMs: nil, endedAtMs: 1, cause: nil),
            .missed)
    }

    func testARefusalIsDeclined() {
        XCTAssertEqual(
            CallTranscriptRow.outcome(connectedAtMs: nil, endedAtMs: 1, cause: .declined),
            .declined)
    }

    /// A device answering busy refused the call as surely as a person tapping
    /// Decline did.
    func testABusyDeviceReadsAsDeclined() {
        XCTAssertEqual(
            CallTranscriptRow.outcome(connectedAtMs: nil, endedAtMs: 1, cause: .busy),
            .declined)
    }

    /// The end stamp is the clock at teardown; a skewed pair never becomes a
    /// negative call length.
    func testAnEndBeforeTheConnectIsZeroLength() {
        XCTAssertEqual(
            CallTranscriptRow.outcome(connectedAtMs: 10_000, endedAtMs: 9_000, cause: .hungUp),
            .answered(durationMs: 0))
    }

    // MARK: - Storage

    /// The row survives a relaunch by being persisted in the message log, so
    /// what is written has to read back as what it was.
    func testEveryRowRoundTripsThroughItsStoredForm() {
        let rows: [CallTranscriptRow] = [
            row(.outgoing, .answered(durationMs: 0)),
            row(.outgoing, .answered(durationMs: 724_000)),
            row(.incoming, .answered(durationMs: 1)),
            row(.incoming, .missed),
            row(.outgoing, .missed),
            row(.incoming, .declined),
            row(.outgoing, .declined),
        ]
        for original in rows {
            let parsed = CallTranscriptRow.parse(original.marker)
            XCTAssertEqual(parsed, original, "\(original.marker) did not read back")
            XCTAssertEqual(parsed?.text, original.text)
        }
    }

    func testTheStoredFormIsRecognisableAsACallRow() {
        XCTAssertTrue(CallTranscriptRow.isCallRow(row(.incoming, .missed).marker))
        XCTAssertFalse(CallTranscriptRow.isCallRow("Hello"))
        XCTAssertFalse(CallTranscriptRow.isCallRow("[mls commit]"))
    }

    /// An ordinary message is never mistaken for a call row.
    func testOrdinaryTextDoesNotParse() {
        for text in ["", "Missed call", "[mls commit]", "call me back", "[call]"] {
            XCTAssertNil(CallTranscriptRow.parse(text), "\(text) must not parse")
        }
    }

    /// A marker this build cannot read yields nothing, and the timeline then
    /// draws nothing — never the raw marker.
    func testAMarkerFromAnotherBuildYieldsNothing() {
        for text in [
            "[call] sideways answered 1",
            "[call] outgoing transferred 1",
            "[call] outgoing answered",
            "[call] outgoing answered -1",
            "[call] outgoing answered notanumber",
            "[call] outgoing missed 5",
        ] {
            XCTAssertNil(CallTranscriptRow.parse(text), "\(text) must not parse")
        }
    }

    // MARK: - The timeline

    private func systemRow(_ text: String) -> ChatMessage {
        ChatMessage(messageId: -1, direction: .system, text: text, createdAt: "")
    }

    func testTheTimelineDrawsACallRowAndReadsItBack() {
        let message = systemRow(row(.incoming, .missed).marker)
        XCTAssertFalse(ChatTimeline.isHiddenSystemRow(message))
        XCTAssertEqual(ChatTimeline.callRow(message), row(.incoming, .missed))
    }

    func testTheTimelineHidesAnUnreadableCallRow() {
        let message = systemRow("[call] outgoing teleported")
        XCTAssertTrue(ChatTimeline.isHiddenSystemRow(message))
        XCTAssertNil(ChatTimeline.callRow(message))
    }

    func testTheBookkeepingRowsStayHidden() {
        XCTAssertTrue(ChatTimeline.isHiddenSystemRow(systemRow("[mls commit]")))
        XCTAssertTrue(ChatTimeline.isHiddenSystemRow(systemRow("[mls external join]")))
    }

    /// An incoming message that happens to look like a marker is not a system
    /// row, so it is never read as a call.
    func testAPeersMessageIsNeverReadAsACallRow() {
        let message = ChatMessage(
            messageId: 7, direction: .incoming,
            text: row(.outgoing, .answered(durationMs: 1)).marker, createdAt: "")
        XCTAssertNil(ChatTimeline.callRow(message))
        XCTAssertFalse(ChatTimeline.isHiddenSystemRow(message))
    }

    /// Section 6 — the row has to survive a relaunch the way the rest of the
    /// timeline does, which it gets for free by being an ordinary system row
    /// in the encrypted message log. This is that route, end to end.
    func testACallRowSurvivesTheMessageLogRoundTrip() throws {
        let original = ChatMessage(
            messageId: -1,
            direction: .system,
            text: row(.outgoing, .answered(durationMs: 724_000)).marker,
            createdAt: "2026-09-17T12:04:00.000Z")
        let restored = try XCTUnwrap(PersistedMessage(from: original).toChatMessage())
        XCTAssertEqual(restored.direction, .system)
        XCTAssertEqual(restored.messageId, -1)
        XCTAssertEqual(restored.createdAt, original.createdAt)
        XCTAssertEqual(ChatTimeline.callRow(restored).map { l10nPlain($0.text) }, "Outgoing call, 12:04")
    }

    // MARK: - The chat list

    func testTheChatListShowsTheSentenceNotTheMarker() {
        let preview = ChatListFormatting.preview(
            text: row(.incoming, .missed).marker,
            attachmentLabels: [],
            direction: .system,
            deleted: false,
            isGroup: false,
            senderName: nil)
        XCTAssertEqual(preview, "Missed call")
    }

    func testTheChatListShowsAnAnsweredCallWithItsDuration() {
        let preview = ChatListFormatting.preview(
            text: row(.outgoing, .answered(durationMs: 724_000)).marker,
            attachmentLabels: [],
            direction: .system,
            deleted: false,
            isGroup: false,
            senderName: nil)
        XCTAssertEqual(l10nPlain(preview), "Outgoing call, 12:04")
    }

    func testACallAnsweredThatNeverCarriedMediaIsFailedNotMissed() {
        for cause in [CallTranscriptRow.Cause.failed, .timedOut, .hungUp] {
            XCTAssertEqual(
                CallTranscriptRow.outcome(connectedAtMs: nil, endedAtMs: 1, cause: cause, accepted: true),
                .failed)
        }
        XCTAssertEqual(
            CallTranscriptRow.outcome(connectedAtMs: nil, endedAtMs: 1, cause: nil, accepted: true), .failed)
        // A refusal stays a refusal, and media that flowed stays answered.
        XCTAssertEqual(
            CallTranscriptRow.outcome(connectedAtMs: nil, endedAtMs: 1, cause: .declined, accepted: true),
            .declined)
        XCTAssertEqual(
            CallTranscriptRow.outcome(connectedAtMs: 1, endedAtMs: 3, cause: .failed, accepted: true),
            .answered(durationMs: 2))
    }

    func testAVideoCallKeepsItsModeInTheStoredRow() {
        for outcome: CallTranscriptRow.Outcome in [.answered(durationMs: 12), .missed, .declined, .failed] {
            let row = CallTranscriptRow(direction: .incoming, outcome: outcome, video: true)
            XCTAssertEqual(CallTranscriptRow.parse(row.marker), row)
            XCTAssertEqual(CallTranscriptRow.parse(row.marker)?.video, true)
        }
    }

    func testARowFromBeforeTheVideoFieldReadsAsVoice() {
        XCTAssertEqual(CallTranscriptRow.parse("[call] outgoing missed")?.video, false)
        XCTAssertEqual(CallTranscriptRow.parse("[call] outgoing answered 5")?.video, false)
        XCTAssertNil(CallTranscriptRow.parse("[call] outgoing video"))
    }

    func testAFailedCallDoesNotReadAsMissed() {
        let row = CallTranscriptRow(direction: .incoming, outcome: .failed)
        XCTAssertNotEqual(row.text, CallTranscriptRow(direction: .incoming, outcome: .missed).text)
        XCTAssertEqual(row.text, Localization.text("call.log.failed"))
    }
}
