// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko

/// The KeyPackage republish ordering and its single-flight coalescing.
///
/// The defect: the only KeyPackage upload minted on a throwaway `Identity`,
/// so its private keys were never where a Welcome could reach them, and
/// discovery keeps one KeyPackage per account — the user could not be added
/// to any group. The fix publishes from the long-lived identity, and these
/// pin the two properties that make that safe:
///
///   * the provider snapshot is durably written BEFORE the upload, and a
///     failed write means NO upload (never publish a KeyPackage whose private
///     keys might not survive a relaunch);
///   * a burst of requests costs one upload, but a request that arrives after
///     a pass minted still gets a KeyPackage minted after it.
@MainActor
final class KeyPackageRepublishTests: XCTestCase {

    private enum Probe: Error { case persistFailed, uploadFailed, mintFailed }

    /// Records every port call in order and lets a test hold the upload open.
    private final class Recorder {
        var events: [String] = []
        var mintCount = 0
        var mintReturnsNil = false
        var mintThrows = false
        var persistThrows = false
        var uploadThrows = false
        var holdUpload = false
        private var waiters: [CheckedContinuation<Void, Never>] = []

        func releaseUploads() {
            holdUpload = false
            let pending = waiters
            waiters.removeAll()
            pending.forEach { $0.resume() }
        }

        @MainActor
        func ports() -> KeyPackageRepublisher.Ports {
            KeyPackageRepublisher.Ports(
                mint: { [unowned self] in
                    if self.mintThrows { self.events.append("mint-throw"); throw Probe.mintFailed }
                    if self.mintReturnsNil { self.events.append("mint-nil"); return nil }
                    self.mintCount += 1
                    self.events.append("mint#\(self.mintCount)")
                    return KeyPackageUpload(
                        accountId: Data(repeating: 1, count: 16),
                        sessionToken: "tok",
                        keyPackage: Data([UInt8(self.mintCount)])
                    )
                },
                persist: { [unowned self] in
                    if self.persistThrows { self.events.append("persist-throw"); throw Probe.persistFailed }
                    self.events.append("persist")
                },
                upload: { [unowned self] upload in
                    self.events.append("upload#\(upload.keyPackage.first.map { Int($0) } ?? -1)")
                    if self.holdUpload {
                        await withCheckedContinuation { self.waiters.append($0) }
                    }
                    if self.uploadThrows { throw Probe.uploadFailed }
                }
            )
        }
    }

    /// Spin the main actor until `condition` holds (bounded).
    private func waitUntil(_ condition: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        for _ in 0..<1000 where !condition() {
            await Task.yield()
        }
        XCTAssertTrue(condition(), "condition never became true", file: file, line: line)
    }

    // MARK: - Ordering

    func testPersistsBeforeUploading() async {
        let recorder = Recorder()
        let republisher = KeyPackageRepublisher(ports: recorder.ports())
        await republisher.request().value
        XCTAssertEqual(recorder.events, ["mint#1", "persist", "upload#1"])
        XCTAssertEqual(republisher.outcomes, [.uploaded])
    }

    func testPersistFailureMeansNoUpload() async {
        let recorder = Recorder()
        recorder.persistThrows = true
        let republisher = KeyPackageRepublisher(ports: recorder.ports())
        await republisher.request().value
        XCTAssertEqual(recorder.events, ["mint#1", "persist-throw"])
        XCTAssertFalse(recorder.events.contains { $0.hasPrefix("upload") })
        XCTAssertEqual(republisher.outcomes, [.persistFailedNoUpload])
    }

    func testNothingToPublishTouchesNeitherPersistNorUpload() async {
        let recorder = Recorder()
        recorder.mintReturnsNil = true
        let republisher = KeyPackageRepublisher(ports: recorder.ports())
        await republisher.request().value
        XCTAssertEqual(recorder.events, ["mint-nil"])
        XCTAssertEqual(republisher.outcomes, [.nothingToPublish])
    }

    func testMintFailureTouchesNeitherPersistNorUpload() async {
        let recorder = Recorder()
        recorder.mintThrows = true
        let republisher = KeyPackageRepublisher(ports: recorder.ports())
        await republisher.request().value
        XCTAssertEqual(recorder.events, ["mint-throw"])
        XCTAssertEqual(republisher.outcomes, [.mintFailed])
    }

    // MARK: - Single-flight

    func testABurstOfRequestsUploadsOnce() async {
        let recorder = Recorder()
        let republisher = KeyPackageRepublisher(ports: recorder.ports())
        // Five Welcomes joined in one inbox poll, all before the pass runs.
        let tasks = (0..<5).map { _ in republisher.request() }
        for task in tasks { await task.value }
        XCTAssertEqual(recorder.mintCount, 1)
        XCTAssertEqual(recorder.events, ["mint#1", "persist", "upload#1"])
    }

    func testRequestsDuringAnUploadGetExactlyOneTrailingPass() async {
        let recorder = Recorder()
        recorder.holdUpload = true
        let republisher = KeyPackageRepublisher(ports: recorder.ports())
        let first = republisher.request()
        await waitUntil { recorder.events.contains("upload#1") }

        // The running pass has already minted KeyPackage #1, which a Welcome
        // arriving now may have consumed. These must not be answered by it.
        republisher.request()
        republisher.request()
        republisher.request()
        recorder.releaseUploads()
        await first.value

        XCTAssertEqual(recorder.mintCount, 2, "one trailing pass, however many requests")
        XCTAssertEqual(
            recorder.events,
            ["mint#1", "persist", "upload#1", "mint#2", "persist", "upload#2"]
        )
        XCTAssertEqual(republisher.outcomes, [.uploaded, .uploaded])
    }

    func testAFailedUploadDoesNotWedgeLaterRequests() async {
        let recorder = Recorder()
        recorder.uploadThrows = true
        let republisher = KeyPackageRepublisher(ports: recorder.ports())
        await republisher.request().value
        recorder.uploadThrows = false
        await republisher.request().value
        XCTAssertEqual(republisher.outcomes, [.uploadFailed, .uploaded])
        XCTAssertEqual(recorder.mintCount, 2)
    }
}
