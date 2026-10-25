// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// `docs/ux/who-can-find-me.md` section 6.1 — the channel handle the search
/// field asks beside the people search.
///
/// The gate in front of it (`looks_like_a_name`) is `crates/lakko-ui-rules`
/// and is tested there. What is tested here is the part iOS owns: asking the
/// channel namespace only for a possible name, without its `@`, and telling
/// "no such channel" from "no answer" and from a used-up allowance.
final class NameLookupTests: XCTestCase {

    private struct Offline: Error {}

    /// A name the blinder would refuse never costs a lookup.
    func testANameThatCannotBeCanonicalIsNeverAsked() async {
        var asked = false
        for typed in ["ab", "anna smith", "anna!", "", "@", String(repeating: "a", count: 33)] {
            let leg = await PeopleSearch.channelLeg(query: typed, lookup: { _ in asked = true; return nil })
            XCTAssertNil(leg.channel, "asked about \(typed)")
            XCTAssertFalse(leg.failed)
            XCTAssertFalse(leg.allowanceUsed)
        }
        XCTAssertFalse(asked, "a name the blinder refuses must not reach the network")
    }

    func testTheAtSignPeopleTypeIsStrippedBeforeAsking() async {
        var seen: [String] = []
        _ = await PeopleSearch.channelLeg(query: "  @Anna  ", lookup: { seen.append($0); return nil })
        XCTAssertEqual(seen, ["Anna"])
    }

    func testAChannelHandleResolvesToAChannel() async {
        let leg = await PeopleSearch.channelLeg(query: "@lakko_news", lookup: { _ in "ab12" })
        XCTAssertEqual(leg.channel, FoundChannel(idHex: "ab12", handle: "lakko_news"))
    }

    func testNoSuchChannelIsAnAnswer() async {
        let leg = await PeopleSearch.channelLeg(query: "anna", lookup: { _ in nil })
        XCTAssertNil(leg.channel)
        XCTAssertFalse(leg.failed)
        XCTAssertFalse(leg.allowanceUsed)
    }

    /// Telling someone a channel does not exist because the connection
    /// dropped is the worst thing this can do.
    func testAnUnreachableLookupIsAFailureRatherThanNothing() async {
        let leg = await PeopleSearch.channelLeg(query: "anna", lookup: { _ in throw Offline() })
        XCTAssertNil(leg.channel)
        XCTAssertTrue(leg.failed)
        XCTAssertFalse(leg.allowanceUsed)
    }

    func testAUsedUpAllowanceIsItsOwnOutcome() async {
        let leg = await PeopleSearch.channelLeg(
            query: "anna", lookup: { _ in throw AnonTokenUnavailable.quotaExhausted(retryAfterSecs: nil) })
        XCTAssertTrue(leg.allowanceUsed)
        XCTAssertFalse(leg.failed)
    }
}
