// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import WebRTC
@testable import Lakko

/// The call ICE-server rule (`IceServers.swift`), with the cases of the web
/// twin `apps/web/src/calls/ice-servers.test.ts`.
final class IceServersTests: XCTestCase {
    func testTakesTheHostAndPortOfEachTurnUriWithoutItsTransport() {
        XCTAssertEqual(
            IceServers.stunUrls(fromTurn: [
                "turn:edge2.lakko.me:3478?transport=tcp",
                "turn:edge2.lakko.me:3478?transport=udp",
            ]),
            ["stun:edge2.lakko.me:3478"]
        )
    }

    func testUses3478WithoutAPortAndKeepsAnIpv6HostBracketed() {
        XCTAssertEqual(IceServers.stunUrls(fromTurn: ["turn:turn.example"]), ["stun:turn.example:3478"])
        XCTAssertEqual(
            IceServers.stunUrls(fromTurn: ["turn:[2001:db8::1]:3479"]),
            ["stun:[2001:db8::1]:3479"]
        )
    }

    func testSchemeIsCaseInsensitive() {
        XCTAssertEqual(IceServers.stunUrls(fromTurn: ["TURN:edge2.lakko.me:3478"]), ["stun:edge2.lakko.me:3478"])
    }

    func testGivesNothingForTurnsAndForWhatDoesNotParse() {
        XCTAssertEqual(
            IceServers.stunUrls(fromTurn: [
                "turns:edge2.lakko.me:5349?transport=tcp",
                "turn:",
                "turn:host:99999",
                "turn:[::1",
            ]),
            []
        )
    }

    func testIsEmptyWithoutATurnEntry() {
        XCTAssertEqual(IceServers.entries(for: nil), [])
    }

    func testIsStunOnTheTurnHostThenTheTurnEntryAndNothingOnGoogle() {
        let turn = IceServers.Entry(
            urls: ["turn:edge2.lakko.me:3478?transport=tcp"],
            username: "u",
            credential: "c"
        )
        let servers = IceServers.entries(for: turn)
        XCTAssertEqual(servers, [IceServers.Entry(urls: ["stun:edge2.lakko.me:3478"]), turn])
        XCTAssertFalse(servers.flatMap(\.urls).contains { $0.lowercased().contains("google") })
    }

    func testATurnEntryWithOnlyTurnsUrisStandsAlone() {
        let turn = IceServers.Entry(urls: ["turns:edge2.lakko.me:5349"], username: "u", credential: "c")
        XCTAssertEqual(IceServers.entries(for: turn), [turn])
    }

    func testLogsTheUrlsAndNeverTheCredential() {
        let line = IceServers.line(IceServers.entries(for: IceServers.Entry(
            urls: ["turn:edge2.lakko.me:3478"],
            username: "user-x",
            credential: "secret-y"
        )))
        XCTAssertEqual(line, "stun:edge2.lakko.me:3478, turn:edge2.lakko.me:3478")
        XCTAssertFalse(line.contains("user-x"))
        XCTAssertFalse(line.contains("secret-y"))
        XCTAssertEqual(IceServers.line([]), "none (host candidates only)")
    }

    @MainActor
    func testResolveWithoutAClientIsEmpty() async {
        let servers = await IceServers.resolve(nil)
        XCTAssertTrue(servers.isEmpty)
    }

    @MainActor
    func testResolveCarriesTheMintedCredentialOnTheTurnRowOnly() async throws {
        let credential = try XCTUnwrap(TurnCredential(wire: TurnCredentialWire(
            urls: ["turn:edge2.lakko.me:3478?transport=tcp"],
            username: "user-x",
            password: "secret-y"
        )))
        let servers = await IceServers.resolve(TurnCredentialClient(preMinted: credential))
        XCTAssertEqual(servers.count, 2)
        XCTAssertEqual(servers[0].urlStrings, ["stun:edge2.lakko.me:3478"])
        XCTAssertNil(servers[0].username)
        XCTAssertNil(servers[0].credential)
        XCTAssertEqual(servers[1].urlStrings, ["turn:edge2.lakko.me:3478?transport=tcp"])
        XCTAssertEqual(servers[1].username, "user-x")
        XCTAssertEqual(servers[1].credential, "secret-y")
    }

    @MainActor
    func testResolveWithAClientThatMintsNothingIsEmpty() async {
        let servers = await IceServers.resolve(TurnCredentialClient(preMinted: nil))
        XCTAssertTrue(servers.isEmpty)
    }
}
