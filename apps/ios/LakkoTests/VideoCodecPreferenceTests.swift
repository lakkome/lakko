// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import WebRTC
@testable import Lakko

/// The codec preference list every video transceiver is given before a local
/// description (`VideoCodecPreference`): VP8 first, then the formats that wrap
/// it, nothing else that carries media. The pure list is driven with plain
/// MIME types; the last test reads the capabilities the linked WebRTC factory
/// really reports, so a binding that stopped offering VP8 fails here; the
/// SDP tests run a real offer and a real answer through `WebRTCPeerConnection`
/// on peer connections that never connect.
@MainActor
final class VideoCodecPreferenceTests: XCTestCase {

    private func prefs(_ mimeTypes: [String]) -> [String]? {
        VideoCodecPreference.vp8Preferences(mimeTypes, mimeType: { $0 })
    }

    /// The order `RTCDefaultVideoEncoderFactory` and WebKit list: H264 first.
    func testH264FirstListPutsVp8FirstThenWrappers() {
        let caps = [
            "video/H264", "video/rtx", "video/H264", "video/H265", "video/VP8",
            "video/VP9", "video/AV1", "video/red", "video/ulpfec", "video/flexfec-03",
        ]
        XCTAssertEqual(
            prefs(caps),
            ["video/VP8", "video/rtx", "video/red", "video/ulpfec", "video/flexfec-03"])
    }

    func testEveryVp8EntryIsKeptInListedOrder() {
        // Distinguishable entries: a tag after "#" stands for the fmtp line.
        let caps = ["video/VP8#a", "video/H264", "video/vp8#b"]
        let out = VideoCodecPreference.vp8Preferences(caps, mimeType: {
            String($0.split(separator: "#").first ?? "")
        })
        XCTAssertEqual(out, ["video/VP8#a", "video/vp8#b"])
    }

    func testMimeTypeMatchIsCaseInsensitive() {
        XCTAssertEqual(prefs(["VIDEO/RTX", "Video/Vp8"]), ["Video/Vp8", "VIDEO/RTX"])
    }

    func testNoVp8GivesNil() {
        XCTAssertNil(prefs(["video/H264", "video/VP9", "video/AV1", "video/rtx", "video/red"]))
        XCTAssertNil(prefs([]))
    }

    func testOnlyVp8GivesVp8Alone() {
        XCTAssertEqual(prefs(["video/VP8"]), ["video/VP8"])
    }

    func testWarningCarriesTheCountAndNoIdentifier() {
        XCTAssertEqual(
            VideoCodecPreference.unsetWarning(count: 2),
            "2 video transceiver(s) could not be set to VP8; "
                + "this device's video may not reach the other person")
    }

    /// The linked WebRTC: its video receive capabilities contain VP8, and the
    /// list built from them starts with it and holds no H264.
    func testLinkedFactoryOffersVp8() {
        let factory = RTCPeerConnectionFactory(
            encoderFactory: RTCDefaultVideoEncoderFactory(),
            decoderFactory: RTCDefaultVideoDecoderFactory())
        let caps = factory.rtpReceiverCapabilities(forKind: kRTCMediaStreamTrackKindVideo).codecs
        let out = VideoCodecPreference.vp8Preferences(caps, mimeType: { $0.mimeType })
        XCTAssertNotNil(out)
        XCTAssertEqual(out?.first?.mimeType.lowercased(), "video/vp8")
        XCTAssertFalse(out?.contains { $0.mimeType.lowercased() == "video/h264" } ?? true)
    }

    // MARK: - Through the real binding

    /// The codec names of the first `m=video` section, in payload order.
    private func videoCodecs(_ sdp: String) -> [String] {
        let lines = sdp.components(separatedBy: "\r\n")
        guard let start = lines.firstIndex(where: { $0.hasPrefix("m=video") }) else { return [] }
        let end = lines[(start + 1)...].firstIndex(where: { $0.hasPrefix("m=") }) ?? lines.count
        let section = lines[start..<end]
        let payloads = section.first?.split(separator: " ").dropFirst(3).map(String.init) ?? []
        var names: [String: String] = [:]
        for line in section where line.hasPrefix("a=rtpmap:") {
            let parts = line.dropFirst("a=rtpmap:".count).split(separator: " ")
            guard parts.count == 2 else { continue }
            names[String(parts[0])] = String(parts[1].split(separator: "/").first ?? "")
        }
        return payloads.map { names[$0] ?? "?" }
    }

    /// A one-to-one video offer from this device names VP8 first and no
    /// H264, although the default encoder factory lists H264 first.
    func testOfferPutsVp8FirstAndDropsH264() async throws {
        let connection = WebRTCPeerConnection(iceServers: [])
        defer { connection.close() }
        connection.addLocalVideoTrack(WebRTCPeerConnection.newVideoSource())

        let sdp = try await connection.createOffer(audio: false, video: true)

        let codecs = videoCodecs(sdp)
        XCTAssertEqual(codecs.first, "VP8")
        XCTAssertFalse(codecs.contains("H264"))
        XCTAssertFalse(codecs.contains("VP9"))
        XCTAssertFalse(codecs.contains("AV1"))
    }

    /// An offer that lists H264 first (what this binding offers with no
    /// preference set, checked first so the test can fail) is answered with
    /// VP8 and no H264.
    func testAnswerToAnH264FirstOfferChoosesVp8() async throws {
        let factory = RTCPeerConnectionFactory(
            encoderFactory: RTCDefaultVideoEncoderFactory(),
            decoderFactory: RTCDefaultVideoDecoderFactory())
        let config = RTCConfiguration()
        config.sdpSemantics = .unifiedPlan
        let constraints = RTCMediaConstraints(mandatoryConstraints: nil, optionalConstraints: nil)
        let raw = try XCTUnwrap(
            factory.peerConnection(with: config, constraints: constraints, delegate: nil))
        defer { raw.close() }
        let source = factory.videoSource()
        raw.add(factory.videoTrack(with: source, trackId: "raw-video"), streamIds: ["raw"])
        let offer: RTCSessionDescription = try await withCheckedThrowingContinuation { cont in
            DispatchQueue.global().async {
                raw.offer(for: constraints) { sdp, err in
                    if let sdp { cont.resume(returning: sdp) } else {
                        cont.resume(throwing: err ?? WebRTCError.emptySdp)
                    }
                }
            }
        }
        XCTAssertEqual(videoCodecs(offer.sdp).first, "H264", "the offer must start H264-first")

        let answerer = WebRTCPeerConnection(iceServers: [])
        defer { answerer.close() }
        let answer = try await answerer.createAnswer(remoteOfferSdp: offer.sdp)

        let codecs = videoCodecs(answer)
        XCTAssertEqual(codecs.first, "VP8")
        XCTAssertFalse(codecs.contains("H264"))
    }
}
