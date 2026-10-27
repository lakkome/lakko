// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import LakkoCore
import UIKit
import XCTest
@testable import Lakko

/// UX `docs/ux/profile-screen.md` and ADR-0084 — the pure parts of the
/// profile screen and the profile photo.
final class ProfileScreenTests: XCTestCase {

    // MARK: - Fixtures

    private func ref(_ byte: UInt8, size: UInt32 = 20_000, side: UInt16 = 640) -> AvatarBlobRef {
        AvatarBlobRef(
            blobId: Data(repeating: byte, count: 32), key: Data(repeating: byte ^ 0xFF, count: 32),
            nonce: Data(repeating: byte, count: 12), size: size, side: side)
    }

    private func media(_ seed: UInt8 = 1) -> PendingAvatarMedia {
        PendingAvatarMedia(stillJPEG: Data(repeating: seed, count: 10), stillSide: 640)
    }

    @MainActor
    private final class Recorder {
        var uploads = 0
        var renames: [String] = []
        var published: [(String?, ProfileAvatar?)] = []
        var uploadFails = false
        var renameError: Error?
        var publishFails = false
    }

    @MainActor
    private func model(saved: ProfileSaved, recorder: Recorder) -> ProfileEditModel {
        let avatar = ProfileAvatar(still: ref(9))
        let ports = ProfileSavePorts(
            upload: { _ in
                recorder.uploads += 1
                if recorder.uploadFails { throw CancellationError() }
                return avatar
            },
            rename: { name in
                recorder.renames.append(name)
                if let error = recorder.renameError { throw error }
                return name
            },
            publish: { name, avatar in
                if recorder.publishFails { throw CancellationError() }
                recorder.published.append((name, avatar))
            })
        return ProfileEditModel(saved: saved, ports: ports)
    }

    // MARK: - Pending and Save enablement (section 5, section 8)

    func testPendingFollowsTheSavedValues() {
        let saved = ProfileSaved(avatar: nil, name: "Alice", username: "alice")
        var pending = ProfilePending()
        XCTAssertTrue(pending.isEmpty)
        pending.setName("Alicia", saved: saved)
        XCTAssertEqual(pending.name, .changed("Alicia"))
        pending.setName("Alice", saved: saved)
        XCTAssertEqual(pending.name, .unchanged, "back to the saved value is no change")
        pending.setName(nil, saved: saved)
        XCTAssertEqual(pending.name, .changed(nil), "clearing the name is a change")
        pending.setUsername("alice", saved: saved)
        XCTAssertEqual(pending.username, .unchanged)
        pending.removePhoto(saved: saved)
        XCTAssertEqual(pending.photo, .unchanged, "nothing to remove when no photo is saved")
        pending.setPhoto(media())
        XCTAssertTrue(pending.showsPhoto(saved: saved))
        pending.removePhoto(saved: saved)
        XCTAssertEqual(pending.photo, .unchanged, "a pending photo over no saved one is dropped")

        let withPhoto = ProfileSaved(avatar: ProfileAvatar(still: ref(1)), name: nil, username: nil)
        pending.removePhoto(saved: withPhoto)
        XCTAssertEqual(pending.photo, .removed)
        XCTAssertFalse(pending.showsPhoto(saved: withPhoto))
    }

    func testSaveIsEnabledOnlyForAChangeWithValidFields() {
        var pending = ProfilePending()
        XCTAssertFalse(ProfileSaveRule.canSave(pending: pending, openFieldsValid: true, saving: false))
        pending.name = .changed("Bob")
        XCTAssertTrue(ProfileSaveRule.canSave(pending: pending, openFieldsValid: true, saving: false))
        XCTAssertFalse(ProfileSaveRule.canSave(pending: pending, openFieldsValid: false, saving: false))
        XCTAssertFalse(ProfileSaveRule.canSave(pending: pending, openFieldsValid: true, saving: true))
    }

    // MARK: - Leaving (section 6)

    func testLeaveDecision() {
        XCTAssertEqual(ProfileLeaveDecision.decide(hasPending: false, saving: false), .leave)
        XCTAssertEqual(ProfileLeaveDecision.decide(hasPending: true, saving: false), .ask)
        XCTAssertEqual(ProfileLeaveDecision.decide(hasPending: true, saving: true), .stay)
        XCTAssertEqual(ProfileLeaveDecision.decide(hasPending: false, saving: true), .stay)
    }

    @MainActor
    func testDontSaveDropsEverythingAndContinues() {
        let recorder = Recorder()
        let m = model(saved: ProfileSaved(name: "Alice", username: "alice"), recorder: recorder)
        m.usePhoto(media())
        var went = false
        XCTAssertFalse(m.requestLeave { went = true })
        XCTAssertTrue(m.leaveRequested)
        m.answerLeave(.discard)
        XCTAssertTrue(went)
        XCTAssertFalse(m.hasPending)
        XCTAssertEqual(recorder.uploads, 0)
    }

    @MainActor
    func testCancelStays() {
        let m = model(saved: ProfileSaved(name: "Alice", username: "alice"), recorder: Recorder())
        m.usePhoto(media())
        var went = false
        XCTAssertFalse(m.requestLeave { went = true })
        m.answerLeave(.cancel)
        XCTAssertFalse(went)
        XCTAssertTrue(m.hasPending)
        XCTAssertFalse(m.leaveRequested)
    }

    @MainActor
    func testNothingPendingLeavesAtOnce() {
        let m = model(saved: ProfileSaved(name: "Alice", username: "alice"), recorder: Recorder())
        XCTAssertTrue(m.requestLeave {})
        XCTAssertFalse(m.leaveRequested)
    }

    @MainActor
    func testSaveFromTheDialogContinuesOnlyOnSuccess() async {
        let recorder = Recorder()
        recorder.uploadFails = true
        let m = model(saved: ProfileSaved(name: "Alice", username: "alice"), recorder: recorder)
        m.usePhoto(media())
        var went = 0
        XCTAssertFalse(m.requestLeave { went += 1 })
        m.answerLeave(.save)
        await waitUntil { !m.saving && m.error != nil }
        XCTAssertEqual(went, 0, "a failed save keeps the person on the screen")
        XCTAssertTrue(m.hasPending)

        recorder.uploadFails = false
        XCTAssertFalse(m.requestLeave { went += 1 })
        m.answerLeave(.save)
        await waitUntil { went == 1 }
        XCTAssertFalse(m.hasPending)
    }

    // MARK: - The inline fields (sections 3 and 4)

    func testInlineFieldOpensClosesRevertsAndStaysOpenWhenInvalid() {
        var edit = InlineFieldEdit()
        XCTAssertEqual(edit.close(isValid: { _ in true }), .notOpen)
        edit.open(current: "Alice")
        XCTAssertTrue(edit.isOpen)
        edit.text = "bad"
        XCTAssertEqual(edit.close(isValid: { $0 != "bad" }), .stayedOpen)
        XCTAssertTrue(edit.isOpen)
        edit.text = "Alicia"
        XCTAssertEqual(edit.close(isValid: { $0 != "bad" }), .closed("Alicia"))
        XCTAssertFalse(edit.isOpen)

        edit.open(current: "Alicia")
        edit.text = "Zed"
        XCTAssertEqual(edit.revert(), "Alicia")
        XCTAssertFalse(edit.isOpen)
        XCTAssertEqual(edit.text, "Alicia")
    }

    @MainActor
    func testNameEditing() {
        let m = model(saved: ProfileSaved(name: "Alice", username: "alice"), recorder: Recorder())
        m.openName()
        m.nameEdit.text = "Alicia"
        m.nameTextChanged()
        XCTAssertTrue(m.canSave)
        m.revertName()
        XCTAssertFalse(m.hasPending, "Escape returns to what it was")
        m.openName()
        m.nameEdit.text = "  "
        m.closeName()
        XCTAssertEqual(m.shownName, nil, "an empty field closes to no name")
        XCTAssertEqual(m.pending.name, .changed(nil))
    }

    @MainActor
    func testUsernameIsLowercasedAndAnInvalidOneStaysOpen() {
        let m = model(saved: ProfileSaved(name: nil, username: "alice"), recorder: Recorder())
        m.openUsername()
        XCTAssertTrue(m.usernameEdit.isOpen)
        m.usernameEdit.text = "BOB_1"
        m.usernameTextChanged()
        XCTAssertEqual(m.usernameEdit.text, "bob_1")
        XCTAssertEqual(m.pending.username, .changed("bob_1"))
        m.usernameEdit.text = "x"
        m.usernameTextChanged()
        m.closeUsername()
        XCTAssertTrue(m.usernameEdit.isOpen, "an invalid username keeps the field open")
        XCTAssertNotNil(m.usernameReason)
        XCTAssertFalse(m.canSave)
    }

    @MainActor
    func testThePencilAlwaysOpensTheUsernameField() {
        let m = model(saved: ProfileSaved(name: nil, username: "alice"), recorder: Recorder())
        m.openUsername()
        XCTAssertTrue(m.usernameEdit.isOpen, "no rename limit: the field always opens")
    }

    @MainActor
    func testNoUsernameIsNotEditable() {
        let m = model(saved: ProfileSaved(name: nil, username: nil), recorder: Recorder())
        m.openUsername()
        XCTAssertFalse(m.usernameEdit.isOpen)
    }

    // MARK: - The save order (section 5)

    @MainActor
    func testSaveRunsPhotoThenUsernameThenProfile() async {
        let recorder = Recorder()
        let m = model(saved: ProfileSaved(name: "Alice", username: "alice"), recorder: recorder)
        m.usePhoto(media())
        m.openUsername()
        m.usernameEdit.text = "alicia"
        m.usernameTextChanged()
        await m.runSave()
        XCTAssertEqual(recorder.uploads, 1)
        XCTAssertEqual(recorder.renames, ["alicia"])
        XCTAssertEqual(recorder.published.count, 1)
        XCTAssertEqual(recorder.published.first?.0, "Alice")
        XCTAssertEqual(recorder.published.first?.1?.still, ref(9))
        XCTAssertFalse(m.hasPending)
        XCTAssertEqual(m.saved.username, "alicia")
        XCTAssertNotNil(m.notice)
    }

    @MainActor
    func testAFailedUploadKeepsEverythingPending() async {
        let recorder = Recorder()
        recorder.uploadFails = true
        let m = model(saved: ProfileSaved(name: "Alice", username: "alice"), recorder: recorder)
        m.usePhoto(media())
        await m.runSave()
        XCTAssertNotNil(m.error)
        XCTAssertTrue(m.hasPending)
        XCTAssertTrue(recorder.published.isEmpty)
    }

    @MainActor
    func testATakenUsernameReopensTheFieldAndTheUploadIsNotRepeated() async {
        let recorder = Recorder()
        recorder.renameError = UsernameRenameFailed(.taken)
        let m = model(saved: ProfileSaved(name: "Alice", username: "alice"), recorder: recorder)
        m.usePhoto(media())
        m.openUsername()
        m.usernameEdit.text = "bob"
        m.usernameTextChanged()
        m.closeUsername()
        await m.runSave()
        XCTAssertTrue(m.usernameEdit.isOpen)
        XCTAssertNotNil(m.error)
        XCTAssertTrue(recorder.published.isEmpty, "the profile step does not run after a failed step")

        recorder.renameError = nil
        m.usernameEdit.text = "bobby"
        m.usernameTextChanged()
        await m.runSave()
        XCTAssertEqual(recorder.uploads, 1, "the photo already uploaded is reused")
        XCTAssertEqual(recorder.renames, ["bob", "bobby"])
        XCTAssertFalse(m.hasPending)
    }

    @MainActor
    func testAProofStopsTheSaveAndCancellingKeepsTheUsernamePending() async {
        let recorder = Recorder()
        recorder.renameError = FreshAuthRequired()
        let m = model(saved: ProfileSaved(name: "Alice", username: "alice"), recorder: recorder)
        m.openUsername()
        m.usernameEdit.text = "bob"
        m.usernameTextChanged()
        await m.runSave()
        XCTAssertTrue(m.needsProof)
        XCTAssertNil(m.error)
        m.proofCancelled()
        XCTAssertFalse(m.needsProof)
        XCTAssertEqual(m.pending.username, .changed("bob"))
    }

    @MainActor
    func testRemovingThePhotoPublishesNoAvatar() async {
        let recorder = Recorder()
        let m = model(
            saved: ProfileSaved(avatar: ProfileAvatar(still: ref(1)), name: "Alice", username: nil),
            recorder: recorder)
        m.removePhoto()
        await m.runSave()
        XCTAssertEqual(recorder.published.count, 1)
        XCTAssertNil(recorder.published.first?.1)
        XCTAssertNil(m.saved.avatar)
    }

    // MARK: - An owed rename (moved from the old rename screen)

    @MainActor
    func testAnOwedRenameIsFinishedOnOpenAndAsksForAProofWhenStillOwed() async {
        var owed = true
        var finishes = 0
        var stillOwedAfter = true
        let ports = ProfileSavePorts(
            upload: { _ in throw CancellationError() }, rename: { $0 },
            publish: { _, _ in },
            renameOwed: { owed },
            finishOwedRename: {
                finishes += 1
                owed = stillOwedAfter
                return stillOwedAfter
            })
        let m = ProfileEditModel(saved: ProfileSaved(name: nil, username: "alice"), ports: ports)
        await m.finishOwedRename()
        XCTAssertEqual(finishes, 1)
        XCTAssertTrue(m.needsProof, "a rollback that needs a proof opens the proof block")
        XCTAssertTrue(m.proofFinishesOwedRename)

        stillOwedAfter = false
        m.proofConfirmed()
        for _ in 0..<100 where finishes < 2 { try? await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(finishes, 2)
        XCTAssertFalse(m.needsProof)

        await m.finishOwedRename()
        XCTAssertEqual(finishes, 2, "nothing owed: nothing runs")
    }

    @MainActor
    func testTheRecoveryEmailWarningComesFromThePort() {
        let ports = ProfileSavePorts(
            upload: { _ in throw CancellationError() }, rename: { $0 },
            publish: { _, _ in },
            renameTurnsOffRecoveryEmail: { true })
        XCTAssertTrue(ProfileEditModel(saved: ProfileSaved(), ports: ports).renameTurnsOffRecoveryEmail)
    }

    // MARK: - Crop math (section 2.2)

    func testCropAtZoomOneIsTheCentredShortSide() {
        let g = AvatarCropGeometry(imageWidth: 2000, imageHeight: 1000, viewport: 300)
        XCTAssertEqual(g.cropRect, CGRect(x: 500, y: 0, width: 1000, height: 1000))
    }

    func testZoomIsClampedAndTheCircleNeverLeavesThePicture() {
        var g = AvatarCropGeometry(imageWidth: 1000, imageHeight: 1000, viewport: 200)
        g.setZoom(10)
        XCTAssertEqual(g.zoom, 4)
        g.setZoom(0.1)
        XCTAssertEqual(g.zoom, 1)
        g.setOffset(CGSize(width: 500, height: -500))
        XCTAssertEqual(g.offset, .zero, "at zoom 1 a square picture cannot move")
        g.setZoom(2)
        g.setOffset(CGSize(width: 10_000, height: 0))
        // Displayed 400 pt for a 200 pt circle: at most 100 pt each way.
        XCTAssertEqual(g.offset.width, 100, accuracy: 0.001)
        let crop = g.cropRect
        XCTAssertEqual(crop.width, 500, accuracy: 0.001)
        XCTAssertEqual(crop.minX, 0, accuracy: 0.001, "moved fully right shows the left edge")
        XCTAssertGreaterThanOrEqual(crop.minY, 0)
        XCTAssertLessThanOrEqual(crop.maxX, 1000)
    }

    func testOutputSideNeverScalesUp() {
        XCTAssertEqual(AvatarCropGeometry.outputSide(cropSide: 2000, maxSide: 640), 640)
        XCTAssertEqual(AvatarCropGeometry.outputSide(cropSide: 300.7, maxSide: 640), 300)
        XCTAssertEqual(AvatarCropGeometry.outputSide(cropSide: 0.2, maxSide: 640), 1)
    }

    // MARK: - The JPEG size loop

    func testTheFirstQualityThatFitsIsKept() {
        var tried: [Double] = []
        let data = AvatarJPEG.fit(maxBytes: 100) { quality in
            tried.append(quality)
            return Data(count: quality > 0.65 ? 500 : 80)
        }
        XCTAssertEqual(data?.count, 80)
        XCTAssertEqual(tried, [0.9, 0.8, 0.7, 0.6])
        XCTAssertNil(AvatarJPEG.fit(maxBytes: 10) { _ in Data(count: 11) })
    }

    func testAStillIsASquareJPEGInsideTheBounds() throws {
        let image = try XCTUnwrap(noiseImage(width: 1600, height: 1200))
        let geometry = AvatarCropGeometry(imageWidth: 1600, imageHeight: 1200, viewport: 300)
        let still = try XCTUnwrap(AvatarJPEG.still(from: image, crop: geometry.cropRect))
        XCTAssertLessThanOrEqual(still.side, AvatarLimits.stillMaxSide)
        XCTAssertLessThanOrEqual(still.data.count, AvatarLimits.stillMaxBytes)
        let decoded = try XCTUnwrap(AvatarFormatCheck.still(
            still.data, ref: ref(1, size: UInt32(still.data.count), side: UInt16(still.side))))
        XCTAssertEqual(decoded.size.width, decoded.size.height)
    }

    /// Random pixels: the worst case for a JPEG's size.
    private func noiseImage(width: Int, height: Int) -> CGImage? {
        var bytes = [UInt8](repeating: 0, count: width * height * 4)
        var rng = SystemRandomNumberGenerator()
        for index in bytes.indices { bytes[index] = UInt8.random(in: 0...255, using: &rng) }
        guard let provider = CGDataProvider(data: Data(bytes) as CFData) else { return nil }
        return CGImage(
            width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }

    func testVideoWindow() {
        XCTAssertEqual(AvatarVideoWindow.length(durationSeconds: 4), 4)
        XCTAssertEqual(AvatarVideoWindow.length(durationSeconds: 30), 10)
        XCTAssertEqual(AvatarVideoWindow.clampStart(25, durationSeconds: 30), 20)
        XCTAssertEqual(AvatarVideoWindow.clampStart(-1, durationSeconds: 30), 0)
        XCTAssertEqual(AvatarVideoWindow.durationMs(durationSeconds: 30), 10_000)
        XCTAssertEqual(AvatarVideoWindow.durationMs(durationSeconds: 0.0001), 1)
    }

    // MARK: - The avatar cache (ADR-0084 section 3)

    func testRetryAfterAMinute() {
        let now = Date()
        XCTAssertTrue(AvatarRetryPolicy.mayFetch(lastFailure: nil, now: now))
        XCTAssertFalse(AvatarRetryPolicy.mayFetch(lastFailure: now.addingTimeInterval(-59), now: now))
        XCTAssertTrue(AvatarRetryPolicy.mayFetch(lastFailure: now.addingTimeInterval(-60), now: now))
    }

    func testDiskCacheKeysByBlobId() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("avatar-test-\(UUID().uuidString)")
        let cache = AvatarDiskCache(directory: dir)
        defer { cache.purge() }
        XCTAssertNil(cache.read(blobIdHex: "ab", kind: .still))
        try cache.write(Data([1, 2, 3]), blobIdHex: "ab", kind: .still)
        XCTAssertEqual(cache.read(blobIdHex: "ab", kind: .still), Data([1, 2, 3]))
        XCTAssertNil(cache.existing(blobIdHex: "ab", kind: .video))
        cache.purge()
        XCTAssertNil(cache.read(blobIdHex: "ab", kind: .still))
    }

    @MainActor
    func testADownloadedStillIsVerifiedCachedAndNotFetchedAgain() async throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("avatar-test-\(UUID().uuidString)")
        let disk = AvatarDiskCache(directory: dir)
        defer { disk.purge() }
        let images = AvatarImages(disk: disk)
        let jpeg = try XCTUnwrap(AvatarJPEG.jpeg(XCTUnwrap(noiseImage(width: 64, height: 64)), quality: 0.8))
        let avatar = ProfileAvatar(still: ref(4, size: UInt32(jpeg.count), side: 64))
        let fetches = Counter()
        images.fetch = { _ in
            await fetches.bump()
            return jpeg
        }
        XCTAssertNil(images.still(avatar), "nothing held: initials, and a load starts")
        await waitUntil { images.still(avatar) != nil }
        XCTAssertNotNil(disk.read(blobIdHex: avatar.still.blobIdHex, kind: .still))
        let fresh = AvatarImages(disk: disk)
        fresh.fetch = { _ in XCTFail("a blob held is never fetched again"); return Data() }
        XCTAssertNotNil(fresh.still(avatar))
        let count = await fetches.value
        XCTAssertEqual(count, 1)
    }

    @MainActor
    func testBytesThatAreNotTheStatedJPEGAreRefusedAndRetriedOnlyAfterAMinute() async {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("avatar-test-\(UUID().uuidString)")
        let disk = AvatarDiskCache(directory: dir)
        defer { disk.purge() }
        let images = AvatarImages(disk: disk)
        let avatar = ProfileAvatar(still: ref(5, size: 3, side: 64))
        let fetches = Counter()
        images.fetch = { _ in
            await fetches.bump()
            return Data([1, 2, 3])
        }
        var clock = Date()
        images.now = { clock }
        XCTAssertNil(images.still(avatar))
        await waitUntil { await fetches.value == 1 }
        try? await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertNil(images.still(avatar))
        try? await Task.sleep(nanoseconds: 50_000_000)
        var count = await fetches.value
        XCTAssertEqual(count, 1, "no retry inside the minute")
        XCTAssertNil(disk.read(blobIdHex: avatar.still.blobIdHex, kind: .still), "refused bytes are never kept")
        clock = clock.addingTimeInterval(61)
        _ = images.still(avatar)
        await waitUntil { await fetches.value == 2 }
        count = await fetches.value
        XCTAssertEqual(count, 2)
    }

    // MARK: - The envelope through the real core

    func testAProfileWithAnAvatarRoundTripsThroughTheCore() throws {
        let avatar = ProfileAvatar(still: ref(1), video: ref(2, size: 1_000_000, side: 480), videoDurationMs: 9_000)
        let wire = try composeProfileEnvelope(profile: FfiProfile(
            displayName: "Alice", username: nil, updatedAtMs: 5, accountId: nil,
            usernameCertificate: nil, grants: [], avatar: avatar.ffi))
        let decoded = try decodeProfileEnvelope(plaintext: wire)
        XCTAssertEqual(ProfileAvatar(decoded.avatar), avatar)
        XCTAssertNoThrow(try ProfileAvatarUpload.checkComposes(name: "Alice", avatar: avatar))
    }

    func testAnAvatarOutsideTheBoundsIsRefused() {
        let tooBig = ProfileAvatar(still: ref(1, size: UInt32(AvatarLimits.stillMaxBytes + 1)))
        XCTAssertThrowsError(try ProfileAvatarUpload.checkComposes(name: "Alice", avatar: tooBig))
    }

    func testAStoredClaimKeepsItsAvatar() throws {
        let claim = ProfileClaim(displayName: "A", username: nil, updatedAtMs: 1, avatar: ProfileAvatar(still: ref(3)))
        let decoded = try JSONDecoder().decode(ProfileClaim.self, from: JSONEncoder().encode(claim))
        XCTAssertEqual(decoded.avatar, claim.avatar)
        let own = OwnProfile(displayName: "A", updatedAtMs: 1, avatar: ProfileAvatar(still: ref(3)))
        XCTAssertEqual(try JSONDecoder().decode(OwnProfile.self, from: JSONEncoder().encode(own)), own)
        // A profile stored before the field existed still loads.
        let old = Data(#"{"displayName":"A","updatedAtMs":1}"#.utf8)
        XCTAssertNil(try JSONDecoder().decode(OwnProfile.self, from: old).avatar)
    }

    // MARK: - Helpers

    private actor Counter {
        private(set) var value = 0
        func bump() { value += 1 }
    }

    @MainActor
    private func waitUntil(timeout: TimeInterval = 3, _ condition: @MainActor () async -> Bool) async {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if await condition() { return }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("condition not met in time")
    }
}
