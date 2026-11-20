// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// UX `docs/ux/storage-full.md` — `507 storage-full` from the storage
/// service: shown where the sending limit is shown, with its own words; a
/// background upload treats it like any other failure.
final class StorageFullTests: XCTestCase {

    private let blobId = Data(repeating: 7, count: 32)

    private func passBody(_ value: String) -> String {
        #"{"pass":"\#(value)","kind":"upload","expires_at_unix_secs":4000000000,"budget":{"files":100,"bytes":1000000}}"#
    }

    private func client(_ host: String) -> StorageClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StorageStubURLProtocol.self]
        let session = URLSession(configuration: config)
        let access = BlobAccess(tokens: OfflineAnonTokens(), session: session, baseURL: { "http://\(host)" })
        return StorageClient(
            baseURL: "http://\(host)", session: session, access: access,
            deleteKey: { _ in "delete-key" }, deleteProof: { _, _ in "proof" })
    }

    func testOnly507WithTheStorageFullCodeIsStorageFull() {
        XCTAssertTrue(BlobAnswer.isStorageFull(status: 507, code: "storage-full"))
        XCTAssertFalse(BlobAnswer.isStorageFull(status: 507, code: nil))
        XCTAssertFalse(BlobAnswer.isStorageFull(status: 507, code: "other"))
        XCTAssertFalse(BlobAnswer.isStorageFull(status: 429, code: "storage-full"))
        XCTAssertEqual(
            BlobAnswer.classify(status: 507, body: Data(#"{"error":"storage-full"}"#.utf8)),
            .failed(status: 507, code: "storage-full"))
    }

    func testAPutRefusedForRoomThrowsStorageFullAndIsNotRetried() async throws {
        let host = StorageStubURLProtocol.scope([
            .init(status: 200, body: passBody("u1")),
            .init(status: 507, body: #"{"error":"storage-full"}"#),
        ])
        do {
            try await client(host).putBlob(blobId: blobId, ciphertext: Data([1, 2, 3]))
            XCTFail("storage full")
        } catch StorageClientError.storageFull {}
        let paths = StorageStubURLProtocol.requests(host).map { $0.url?.path }
        XCTAssertEqual(paths, ["/blob/pass/upload", "/blob/\(StorageClient.blobIdHex(blobId))"], "one PUT, no new pass")
    }

    func testA507WithoutTheCodeIsTheGenericFailure() async throws {
        let host = StorageStubURLProtocol.scope([
            .init(status: 200, body: passBody("u1")),
            .init(status: 507, body: "<html>"),
        ])
        do {
            try await client(host).putBlob(blobId: blobId, ciphertext: Data([1, 2, 3]))
            XCTFail("generic failure")
        } catch StorageClientError.http(let status, _, _) {
            XCTAssertEqual(status, 507)
        }
    }

    func testTheBackupTreats507LikeAnyOtherFailedUpload() async throws {
        let host = StorageStubURLProtocol.scope([.init(status: 507, body: "storage-full")])
        do {
            try await client(host).putBackup(envelopeBytes: Data([1, 2, 3]))
            XCTFail("a failed upload")
        } catch StorageClientError.http(let status, _, _) {
            XCTAssertEqual(status, 507)
        }
        XCTAssertEqual(StorageStubURLProtocol.requests(host).count, 1, "no retry")
    }

    func testAnAttachmentSendSaysTheServerHasNoRoom() {
        let text = AttachmentBridgeError.storageFull.userText(fileName: "a.jpg").text
        XCTAssertEqual(text, Localization.text("chat.attachment.full.send"))
        XCTAssertNotEqual(text, "chat.attachment.full.send", "the key is in the catalog")
        XCTAssertNotEqual(text, AttachmentBridgeError.uploadLimited.userText(fileName: nil).text)
    }

    func testTheGroupPhotoKeyIsInTheCatalog() {
        XCTAssertNotEqual(Localization.text("group_photo.error.full"), "group_photo.error.full")
    }

    @MainActor
    func testAProfilePhotoRefusedForRoomSaysSoAndKeepsEverythingPending() async {
        final class Count { var published = 0 }
        let count = Count()
        let ports = ProfileSavePorts(
            upload: { _ in throw AttachmentBridgeError.storageFull },
            rename: { $0 },
            publish: { _, _ in count.published += 1 })
        let m = ProfileEditModel(saved: ProfileSaved(name: "Alice", username: "alice"), ports: ports)
        m.usePhoto(PendingAvatarMedia(stillJPEG: Data(repeating: 1, count: 10), stillSide: 640))
        await m.runSave()
        XCTAssertEqual(m.error?.text, Localization.text("profile.save.error.full"))
        XCTAssertNotEqual(m.error?.text, "profile.save.error.full")
        XCTAssertTrue(m.hasPending)
        XCTAssertEqual(count.published, 0)
    }

    // The group, channel and profile photos upload through
    // `ProfileAvatarUpload`: the refusals must reach them as the kinds their
    // screens catch.

    @MainActor
    func testAPhotoUploadAtTheSendingLimitIsUploadLimited() async throws {
        let host = StorageStubURLProtocol.scope([
            .init(status: 200, body: passBody("u1")),
            .init(status: 429, body: #"{"error":"blob-upload-budget-exhausted"}"#),
            .init(status: 200, body: passBody("u2")),
            .init(status: 429, body: #"{"error":"blob-upload-budget-exhausted"}"#),
        ])
        let media = PendingAvatarMedia(stillJPEG: Data(repeating: 1, count: 10), stillSide: 640)
        do {
            _ = try await ProfileAvatarUpload.upload(media, storage: client(host))
            XCTFail("the limit")
        } catch AttachmentBridgeError.uploadLimited {}
    }

    @MainActor
    func testAPhotoUploadWithNoRoomIsStorageFull() async throws {
        let host = StorageStubURLProtocol.scope([
            .init(status: 200, body: passBody("u1")),
            .init(status: 507, body: #"{"error":"storage-full"}"#),
        ])
        let media = PendingAvatarMedia(stillJPEG: Data(repeating: 1, count: 10), stillSide: 640)
        do {
            _ = try await ProfileAvatarUpload.upload(media, storage: client(host))
            XCTFail("storage full")
        } catch AttachmentBridgeError.storageFull {}
    }
}
