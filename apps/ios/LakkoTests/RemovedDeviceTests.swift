// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest

@testable import Lakko

/// "This device was removed" (docs/ux/second-device.md section 3.1): the
/// decisions in ``RemovedDevice`` and the state transition in
/// ``DeviceGroupSyncRepository``.
@MainActor
final class RemovedDeviceTests: XCTestCase {

    private struct ReadFailed: Error {}

    // MARK: - Copy

    func testCopyRendersTheCatalogMessages() {
        let pairs: [(String, String)] = [
            (RemovedDevice.Copy.title, "devices.removed.title"),
            (RemovedDevice.Copy.signOut, "devices.removed.sign_out"),
        ]
        for (text, key) in pairs {
            XCTAssertEqual(text, Localization.text(key))
            XCTAssertNotEqual(text, key, "\(key) is missing from the catalog")
        }
    }

    /// UX 18.6: the two bodies name the account (`{name}`); a guest account
    /// reads "this account", never an empty gap.
    func testTheTwoBodiesNameTheAccount() {
        for key in [RemovedDevice.Copy.bodyRemovingKey, RemovedDevice.Copy.bodyDoneKey] {
            XCTAssertNotEqual(Localization.text(key, texts: ["name": "x"]), key, "\(key) is missing from the catalog")
        }
        let removing = RemovedDevice.Copy.bodyRemoving(account: "@alice")
        let done = RemovedDevice.Copy.bodyDone(account: "@alice")
        XCTAssertTrue(removing.contains("@alice"))
        XCTAssertTrue(done.contains("@alice"))
        XCTAssertNotEqual(removing, done)
        XCTAssertTrue(RemovedDevice.Copy.bodyDone(account: "")
            .contains(Localization.text("devices.signed_out.this_account")))
    }

    // MARK: - Reading isActive

    func testReadActivePassesTheValueThrough() {
        XCTAssertEqual(RemovedDevice.readActive { true }, true)
        XCTAssertEqual(RemovedDevice.readActive { false }, false)
    }

    func testAFailedReadIsNil() {
        XCTAssertNil(RemovedDevice.readActive { throw ReadFailed() })
    }

    // MARK: - Decisions

    func testOnlyAnExplicitFalseIsARemoval() {
        XCTAssertTrue(RemovedDevice.isRemoved(false))
        XCTAssertFalse(RemovedDevice.isRemoved(true))
        XCTAssertFalse(RemovedDevice.isRemoved(nil))
    }

    func testPollStopsOnlyForAnInactiveGroup() {
        XCTAssertEqual(RemovedDevice.pollStepAfterBatch(false), .stopRemoved)
        XCTAssertEqual(RemovedDevice.pollStepAfterBatch(true), .continue)
        XCTAssertEqual(RemovedDevice.pollStepAfterBatch(nil), .continue)
    }

    func testForkRepairRunsOnlyOnAGroupKnownToBeActive() {
        XCTAssertTrue(RemovedDevice.forkRepairAllowed(true))
        XCTAssertFalse(RemovedDevice.forkRepairAllowed(false))
        XCTAssertFalse(RemovedDevice.forkRepairAllowed(nil))
    }

    // MARK: - The repository

    func testAnInactiveGroupSetsDeviceRemovedAndStopsPolling() {
        let repo = DeviceGroupSyncRepository()
        let poll = Task<Void, Never> {
            while !Task.isCancelled { try? await Task.sleep(nanoseconds: 10_000_000) }
        }
        repo.adoptPollTaskForTesting(poll)

        XCTAssertTrue(repo.noteDeviceGroupActivity(false))
        XCTAssertTrue(repo.deviceRemoved)
        XCTAssertFalse(repo.isPolling)
        XCTAssertTrue(poll.isCancelled)
    }

    func testAnActiveGroupKeepsPolling() {
        let repo = DeviceGroupSyncRepository()
        let poll = Task<Void, Never> {
            while !Task.isCancelled { try? await Task.sleep(nanoseconds: 10_000_000) }
        }
        repo.adoptPollTaskForTesting(poll)

        XCTAssertFalse(repo.noteDeviceGroupActivity(true))
        XCTAssertFalse(repo.deviceRemoved)
        XCTAssertTrue(repo.isPolling)
        XCTAssertFalse(poll.isCancelled)
        poll.cancel()
    }

    func testAFailedReadKeepsPolling() {
        let repo = DeviceGroupSyncRepository()
        let poll = Task<Void, Never> {
            while !Task.isCancelled { try? await Task.sleep(nanoseconds: 10_000_000) }
        }
        repo.adoptPollTaskForTesting(poll)

        XCTAssertFalse(repo.noteDeviceGroupActivity(nil))
        XCTAssertFalse(repo.deviceRemoved)
        XCTAssertTrue(repo.isPolling)
        poll.cancel()
    }

    /// Once removed, a later read that fails or even says active does not
    /// bring the device back this session.
    func testRemovalIsForTheSession() {
        let repo = DeviceGroupSyncRepository()
        repo.noteDeviceGroupActivity(false)
        XCTAssertTrue(repo.noteDeviceGroupActivity(nil))
        XCTAssertTrue(repo.noteDeviceGroupActivity(true))
        XCTAssertTrue(repo.deviceRemoved)
    }

    func testSignOutResetsDeviceRemoved() {
        let repo = DeviceGroupSyncRepository()
        repo.noteDeviceGroupActivity(false)
        repo.clear()
        XCTAssertFalse(repo.deviceRemoved)
    }
}
