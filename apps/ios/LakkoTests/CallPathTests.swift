// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The call path line (`CallPath.swift`), with the cases of the web twin
/// `apps/web/src/calls/call-path.test.ts`.
final class CallPathTests: XCTestCase {
    private func report(
        pairId: String = "CP1",
        local: [String: NSObject] = [:],
        remote: [String: NSObject] = [:],
        rtt: Double? = 0.012,
        state: String = "succeeded",
        viaTransport: Bool = false
    ) -> [CallMediaStatEntry] {
        var localValues: [String: NSObject] = [
            "candidateType": "host" as NSString, "protocol": "udp" as NSString,
            "address": "192.168.1.20" as NSString, "port": NSNumber(value: 50000),
        ]
        localValues.merge(local) { _, new in new }
        var remoteValues: [String: NSObject] = [
            "candidateType": "host" as NSString, "protocol": "udp" as NSString,
            "address": "192.168.1.21" as NSString, "port": NSNumber(value: 50001),
        ]
        remoteValues.merge(remote) { _, new in new }
        var pairValues: [String: NSObject] = [
            "state": state as NSString, "nominated": NSNumber(value: true),
            "localCandidateId": "L1" as NSString, "remoteCandidateId": "R1" as NSString,
        ]
        if let rtt { pairValues["currentRoundTripTime"] = NSNumber(value: rtt) }
        var entries = [
            CallMediaStatEntry(id: "L1", type: "local-candidate", values: localValues),
            CallMediaStatEntry(id: "R1", type: "remote-candidate", values: remoteValues),
            CallMediaStatEntry(id: pairId, type: "candidate-pair", values: pairValues),
        ]
        if viaTransport {
            entries.append(CallMediaStatEntry(
                id: "T1", type: "transport", values: ["selectedCandidatePairId": pairId as NSString]))
        }
        return entries
    }

    func testPrintsADirectPathWithNoAddressOrPort() throws {
        let line = CallPath.describe(try XCTUnwrap(CallPath.read(report())))
        XCTAssertEqual(line, "call path: local=host/udp remote=host/udp rtt=12ms")
        XCTAssertFalse(line.contains("192.168"))
        XCTAssertFalse(line.contains("5000"))
    }

    func testPrintsTheTurnTransportAndServerNameOfARelayedCandidate() throws {
        let entries = report(
            local: [
                "candidateType": "relay" as NSString, "relayProtocol": "tls" as NSString,
                "url": "turns:edge.lakko.me:443?transport=tcp" as NSString,
                "address": "46.224.156.97" as NSString,
            ],
            remote: ["candidateType": "srflx" as NSString],
            rtt: 0.0414)
        XCTAssertEqual(
            CallPath.describe(try XCTUnwrap(CallPath.read(entries))),
            "call path: local=relay/udp/tls via=edge.lakko.me remote=srflx/udp rtt=41ms")
    }

    func testFollowsTheTransportsSelectedPair() {
        XCTAssertEqual(CallPath.read(report(state: "in-progress", viaTransport: true))?.local, "host/udp")
    }

    func testIsNilBeforeAPairHasSucceeded() {
        XCTAssertNil(CallPath.read(report(state: "in-progress")))
        XCTAssertNil(CallPath.read([]))
    }

    func testPrintsADashWithoutARoundTripTime() throws {
        XCTAssertEqual(
            CallPath.describe(try XCTUnwrap(CallPath.read(report(rtt: nil)))),
            "call path: local=host/udp remote=host/udp rtt=-")
    }

    func testTurnHostNameNamesAHostNeverAnAddress() {
        XCTAssertEqual(CallPath.turnHostName("turn:edge2.lakko.me:3478?transport=udp"), "edge2.lakko.me")
        XCTAssertEqual(CallPath.turnHostName("TURNS:Edge.Lakko.Me"), "edge.lakko.me")
        XCTAssertNil(CallPath.turnHostName("turn:46.224.156.97:3478"))
        XCTAssertNil(CallPath.turnHostName("turn:[2a01:4f8::1]:3478"))
        XCTAssertNil(CallPath.turnHostName("stun:edge2.lakko.me:3478"))
        XCTAssertNil(CallPath.turnHostName(nil))
    }

    func testLogsOncePerPathAndAgainWhenThePathChanges() {
        let log = CallPathLog()
        XCTAssertNil(log.note([]))
        XCTAssertEqual(log.note(report()), "call path: local=host/udp remote=host/udp rtt=12ms")
        XCTAssertNil(log.note(report(rtt: 0.2)))
        let relay: [String: NSObject] = ["candidateType": "relay" as NSString, "relayProtocol": "udp" as NSString]
        XCTAssertEqual(log.note(report(local: relay)), "call path: local=relay/udp/udp remote=host/udp rtt=12ms")
        XCTAssertNotNil(log.note(report(pairId: "CP2", local: relay)))
    }
}
