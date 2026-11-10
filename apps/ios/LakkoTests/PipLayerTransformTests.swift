// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import QuartzCore
import XCTest

@testable import Lakko

/// The picture-in-picture window's frame transform
/// (`SampleBufferDisplayView.layerTransform`): turn upright first, then
/// mirror left to right on the screen.
final class PipLayerTransformTests: XCTestCase {

    private func map(_ p: CGPoint, _ t: CATransform3D) -> CGPoint {
        p.applying(CATransform3DGetAffineTransform(t))
    }

    func testAMirrorFlipsLeftAndRightOnTheScreenWhateverTheTurn() {
        for angle in [CGFloat(0), .pi / 2, .pi, -.pi / 2] {
            for point in [CGPoint(x: 3, y: 1), CGPoint(x: -2, y: 5)] {
                let turned = map(point, SampleBufferDisplayView.layerTransform(angle: angle, mirrored: false))
                let mirrored = map(point, SampleBufferDisplayView.layerTransform(angle: angle, mirrored: true))
                XCTAssertEqual(mirrored.x, -turned.x, accuracy: 1e-9, "angle \(angle)")
                XCTAssertEqual(mirrored.y, turned.y, accuracy: 1e-9, "angle \(angle)")
            }
        }
    }

    func testWithoutAMirrorItIsTheTurnAlone() {
        let t = SampleBufferDisplayView.layerTransform(angle: .pi / 2, mirrored: false)
        let p = map(CGPoint(x: 1, y: 0), t)
        XCTAssertEqual(p.x, 0, accuracy: 1e-9)
        XCTAssertEqual(p.y, 1, accuracy: 1e-9)
    }
}
