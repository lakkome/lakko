// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// Which seed the call's shared SFrame keys move to (``CallKeysSeeds``).
final class CallKeysSeedsTests: XCTestCase {
    private func seed(_ byte: UInt8) -> Data { Data(repeating: byte, count: 32) }

    func testTheFirstArmCreatesAndTheSameSeedUsesTheKeys() {
        var seeds = CallKeysSeeds()
        XCTAssertEqual(seeds.decide(armAt: seed(1)), .create)
        seeds.moved(to: seed(1))
        XCTAssertEqual(seeds.decide(armAt: seed(1)), .use)
    }

    func testANewSeedRekeysFirst() {
        var seeds = CallKeysSeeds()
        seeds.moved(to: seed(1))
        XCTAssertEqual(seeds.decide(armAt: seed(2)), .rekey)
        seeds.moved(to: seed(2))
        XCTAssertEqual(seeds.current, seed(2))
    }

    /// An arm that read the seed before a rekey must not roll the call back.
    func testASupersededSeedUsesTheCurrentKeys() {
        var seeds = CallKeysSeeds()
        seeds.moved(to: seed(1))
        seeds.moved(to: seed(2))
        XCTAssertEqual(seeds.decide(armAt: seed(1)), .use)
        XCTAssertEqual(seeds.current, seed(2))
    }

    func testOnlyTheLastSixteenSeedsAreRemembered() {
        var seeds = CallKeysSeeds()
        for byte in UInt8(0)...UInt8(17) { seeds.moved(to: seed(byte)) }
        // Current is 17; 1...16 are remembered, 0 is forgotten.
        XCTAssertEqual(seeds.decide(armAt: seed(1)), .use)
        XCTAssertEqual(seeds.decide(armAt: seed(0)), .rekey)
    }

    func testClearForgetsEverything() {
        var seeds = CallKeysSeeds()
        seeds.moved(to: seed(1))
        seeds.clear()
        XCTAssertNil(seeds.current)
        XCTAssertEqual(seeds.decide(armAt: seed(1)), .create)
    }
}
