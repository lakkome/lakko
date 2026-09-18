// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// Reading a missing remote picture off the inbound counters.
///
/// The call this exists for: Android reported encoding and sending video with
/// its SFrame encryptor armed, and iOS reported the track attaching and then no
/// frame for a minute. The screen cannot tell the three reasons apart, and they
/// have opposite fixes — so the counters name the first stage that failed.
final class RemoteVideoDiagnosisTests: XCTestCase {

    private func stats(
        packets: Int64 = 0, received: Int64 = 0, decoded: Int64 = 0,
        dropped: Int64 = 0, bytes: Int64 = 0, pli: Int64 = 0,
        keyFrames: Int64 = 0, codec: String? = nil, decoder: String? = nil
    ) -> InboundVideoStats {
        InboundVideoStats(
            packetsReceived: packets, framesReceived: received,
            framesDecoded: decoded, framesDropped: dropped, bytesReceived: bytes,
            pliCount: pli, keyFramesDecoded: keyFrames, codec: codec,
            decoder: decoder)
    }

    // MARK: - The three ways a picture goes missing

    /// Nothing on the wire: the fault is upstream of anything this client does
    /// with frames.
    func testNoPacketsIsATransportAnswerNotADecryptionOne() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.diagnose(stats(), rendered: false), .noPacketsArriving)
    }

    /// An empty report says the same thing as a stream of zeroes.
    func testAnAbsentInboundStreamReadsAsNoPackets() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.diagnose(.none, rendered: false), .noPacketsArriving)
    }

    /// The shape the live call is expected to show if the decryptor is the
    /// culprit: packets climb, nothing decodes. An SFrame open failure discards
    /// the frame before the decoder is reached.
    func testPacketsWithNothingDecodedPointsAtDecryption() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.diagnose(
                stats(packets: 312, received: 0, decoded: 0, bytes: 51_084),
                rendered: false),
            .packetsNotDecoding)
    }

    /// Frames assembled but still none decoded is the same verdict — the
    /// decoder never saw them, whatever the depacketiser counted.
    func testFramesReceivedButNoneDecodedIsStillTheDecodingAnswer() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.diagnose(
                stats(packets: 500, received: 20, decoded: 0), rendered: false),
            .packetsNotDecoding)
    }

    /// Media is fine and the view layer is not: a separate failure with a
    /// separate fix, which is why the renderer's own signal is an input rather
    /// than something inferred from the counters.
    func testDecodedButNeverDrawnIsAViewProblem() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.diagnose(
                stats(packets: 500, received: 30, decoded: 28), rendered: false),
            .decodedButNotRendered)
    }

    /// A rendered frame is the end of the question, whatever the counters say —
    /// including a report that has not caught up yet.
    func testARenderedFrameIsFlowingRegardlessOfTheCounters() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.diagnose(stats(packets: 500, decoded: 28), rendered: true),
            .flowing)
        XCTAssertEqual(
            RemoteVideoDiagnosis.diagnose(.none, rendered: true), .flowing)
    }

    /// The verdict names the FIRST stage that failed, not the last symptom —
    /// dropped frames alongside no decoding must not read as a decoder problem.
    func testTheVerdictNamesTheFirstFailedStage() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.diagnose(
                stats(packets: 400, received: 10, decoded: 0, dropped: 10),
                rendered: false),
            .packetsNotDecoding)
    }

    // MARK: - The line a human reads

    /// One line, the counters that distinguish the cases, and nothing that
    /// identifies a person or carries payload.
    func testTheLogLineCarriesTheCountersAndNoPayload() {
        let s = stats(packets: 312, received: 0, decoded: 0, dropped: 4, bytes: 51_084)
        let line = RemoteVideoDiagnosis.packetsNotDecoding.logLine(
            s, rendered: false, reading: .early(afterSeconds: 4))
        XCTAssertTrue(line.contains("PACKETS_NOT_DECODING"))
        XCTAssertTrue(line.contains("packetsReceived=312"))
        XCTAssertTrue(line.contains("framesDecoded=0"))
        XCTAssertTrue(line.contains("framesDropped=4"))
        XCTAssertTrue(line.contains("bytesReceived=51084"))
        XCTAssertTrue(line.contains("rendered=false"))
        XCTAssertFalse(line.contains("\n"), "one line, so a transcript stays readable")
    }

    /// The codec earns its place in the line: a rejected frame and an
    /// undecodable one produce the SAME counters, so without the codec name
    /// `PACKETS_NOT_DECODING` cannot be acted on.
    func testTheLineNamesTheCodecThatMakesTheVerdictActionable() {
        let s = stats(
            packets: 312, decoded: 0, codec: "video/H264", decoder: "VideoToolbox")
        let line = RemoteVideoDiagnosis.packetsNotDecoding.logLine(
            s, rendered: false, reading: .early(afterSeconds: 4))
        XCTAssertTrue(line.contains("codec=video/H264"))
        XCTAssertTrue(line.contains("decoder=VideoToolbox"))
    }

    /// A report that names no codec still produces a readable line rather than
    /// an empty field a reader would misread as "no codec was negotiated".
    func testAnAbsentCodecReadsAsUnknownRatherThanBlank() {
        let line = RemoteVideoDiagnosis.noPacketsArriving.logLine(
            stats(), rendered: false, reading: .early(afterSeconds: 4))
        XCTAssertTrue(line.contains("codec=unknown"))
        XCTAssertTrue(line.contains("decoder=none"))
    }

    /// The two counters that narrow `PACKETS_NOT_DECODING` to the case this
    /// call actually hit: a receiver that joined mid-stream with no keyframe to
    /// start from. `pliCount` says whether this side asked for one, which is
    /// the difference between "the sender ignored us" and "we never asked".
    func testTheLineCarriesThePliAndKeyframeCounters() {
        let s = stats(packets: 107, decoded: 0, dropped: 27, pli: 5, keyFrames: 0)
        let line = RemoteVideoDiagnosis.packetsNotDecoding.logLine(
            s, rendered: false, reading: .early(afterSeconds: 4))
        XCTAssertTrue(line.contains("pliCount=5"))
        XCTAssertTrue(line.contains("keyFramesDecoded=0"))
    }

    /// A receiver that never asked reads differently from one that asked and
    /// was ignored, and the line must not blur them.
    func testAskingAndNotAskingAreDistinguishable() {
        let asked = RemoteVideoDiagnosis.packetsNotDecoding.logLine(
            stats(packets: 107, pli: 5), rendered: false,
            reading: .early(afterSeconds: 4))
        let silent = RemoteVideoDiagnosis.packetsNotDecoding.logLine(
            stats(packets: 107, pli: 0), rendered: false,
            reading: .early(afterSeconds: 4))
        XCTAssertNotEqual(asked, silent)
        XCTAssertTrue(silent.contains("pliCount=0"))
    }

    // MARK: - When the counters were read (the reading, not just the verdict)

    /// The defect this closes, in one assertion. The probe fired once, four
    /// seconds after the TRACK attached; on a live call the sender's first
    /// packet came nine seconds later, so it reported NO_PACKETS about a moment
    /// when no packet could exist and never looked again. True, and worse than
    /// useless.
    func testAnEarlyReadingAndALateOneAreDistinguishableInTheLog() {
        let early = RemoteVideoDiagnosis.noPacketsArriving.logLine(
            stats(), rendered: false, reading: .early(afterSeconds: 4))
        let late = RemoteVideoDiagnosis.noPacketsArriving.logLine(
            stats(packets: 107), rendered: false,
            reading: .afterFirstPacket(settleSeconds: 4))
        XCTAssertNotEqual(early, late)
        XCTAssertTrue(early.contains("early"))
        XCTAssertTrue(late.contains("late"))
        XCTAssertTrue(late.contains("after first packet"))
    }

    /// The late reading must name the wire event it is anchored to, or a reader
    /// cannot tell it from another fixed delay.
    func testEveryReadingLabelsItsOwnAnchor() {
        XCTAssertTrue(
            RemoteVideoReading.early(afterSeconds: 4).label.contains("track attach"))
        XCTAssertTrue(
            RemoteVideoReading.afterFirstPacket(settleSeconds: 4).label
                .contains("first packet"))
        XCTAssertTrue(RemoteVideoReading.decoding.label.contains("decoding"))
        XCTAssertTrue(
            RemoteVideoReading.budgetExpired(afterSeconds: 45).label.contains("budget"))
    }

    // MARK: - The ticker's rule

    /// Nothing has arrived and the budget has not run out: keep looking. This
    /// is the state the one-shot probe treated as a final answer.
    func testTheProbeKeepsWaitingWhileNothingHasArrived() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.step(
                stats: stats(), rendered: false, secondsSinceFirstPacket: nil,
                secondsRemaining: 30, settleSeconds: 4),
            .keepWaiting)
    }

    /// Packets have started but have not settled — still too early to conclude.
    func testTheProbeWaitsForThePacketsToSettle() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.step(
                stats: stats(packets: 12), rendered: false,
                secondsSinceFirstPacket: 1, secondsRemaining: 30, settleSeconds: 4),
            .keepWaiting)
    }

    /// The reading the whole change exists for: taken once the sender is known
    /// to be flowing, not on a local clock.
    func testTheLateReadingIsTakenAfterThePacketsSettle() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.step(
                stats: stats(packets: 107), rendered: false,
                secondsSinceFirstPacket: 4, secondsRemaining: 30, settleSeconds: 4),
            .report(.afterFirstPacket(settleSeconds: 4)))
    }

    /// Decoding ends the question, so it is asked before either deadline —
    /// including before the packets have settled.
    func testDecodingEndsTheProbeEarly() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.step(
                stats: stats(packets: 107, decoded: 3), rendered: false,
                secondsSinceFirstPacket: 1, secondsRemaining: 30, settleSeconds: 4),
            .report(.decoding))
        XCTAssertEqual(
            RemoteVideoDiagnosis.step(
                stats: stats(), rendered: true, secondsSinceFirstPacket: nil,
                secondsRemaining: 30, settleSeconds: 4),
            .report(.decoding))
    }

    /// A call where no packet ever arrives must still produce a line: a probe
    /// that goes quiet reads exactly like one that never ran.
    func testTheBudgetAlwaysProducesALine() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.step(
                stats: stats(), rendered: false, secondsSinceFirstPacket: nil,
                secondsRemaining: 0, settleSeconds: 4),
            .report(.budgetExpired(afterSeconds: 0)))
    }

    /// The settle deadline outranks the budget, so a sender that starts in the
    /// last seconds still gets the reading that describes it.
    func testASenderThatStartsLateStillGetsItsOwnReading() {
        XCTAssertEqual(
            RemoteVideoDiagnosis.step(
                stats: stats(packets: 107), rendered: false,
                secondsSinceFirstPacket: 5, secondsRemaining: 0, settleSeconds: 4),
            .report(.afterFirstPacket(settleSeconds: 4)))
    }

    /// Every verdict has a distinct name, or a console reader cannot tell two
    /// different answers apart.
    func testEveryVerdictHasItsOwnName() {
        let names: [String] = [
            RemoteVideoDiagnosis.noPacketsArriving.rawValue,
            RemoteVideoDiagnosis.packetsNotDecoding.rawValue,
            RemoteVideoDiagnosis.decodedButNotRendered.rawValue,
            RemoteVideoDiagnosis.flowing.rawValue,
        ]
        XCTAssertEqual(Set(names).count, names.count)
    }
}
