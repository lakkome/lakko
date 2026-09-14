// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// ADR-0047 — media metadata survives the message log, an old row still
/// loads, and decrypted media temp files are bookkept and cleaned.
final class MediaPersistenceTests: XCTestCase {

    // MARK: - PersistedAttachment

    private func mediaRef() -> AttachmentRef {
        AttachmentRef(
            blobId: Data(repeating: 1, count: 32),
            key: Data(repeating: 2, count: 32),
            nonce: Data(repeating: 3, count: 12),
            mime: "video/mp4",
            size: 123_456,
            name: "video-20260913-101010.mp4",
            kind: "video_note",
            durationMs: 12_345,
            waveform: Data([0, 128, 255]),
            width: 480,
            height: 480,
            thumb: ThumbRef(
                blobId: Data(repeating: 4, count: 32),
                key: Data(repeating: 5, count: 32),
                nonce: Data(repeating: 6, count: 12),
                mime: "image/jpeg",
                size: 20_000,
                width: 240,
                height: 240
            )
        )
    }

    func testMediaFieldsRoundTripThroughJSON() throws {
        let ref = mediaRef()
        let persisted = PersistedAttachment(from: ref)
        let json = try JSONEncoder().encode(persisted)
        let decoded = try JSONDecoder().decode(PersistedAttachment.self, from: json)
        XCTAssertEqual(decoded, persisted)
        XCTAssertEqual(decoded.toAttachmentRef(), ref)
    }

    func testPlainFileStillRoundTrips() throws {
        let ref = AttachmentRef(
            blobId: Data(repeating: 9, count: 32), key: Data(repeating: 8, count: 32),
            nonce: Data(repeating: 7, count: 12), mime: "application/pdf", size: 10, name: "a.pdf"
        )
        let decoded = try JSONDecoder().decode(
            PersistedAttachment.self,
            from: JSONEncoder().encode(PersistedAttachment(from: ref))
        )
        XCTAssertEqual(decoded.toAttachmentRef(), ref)
        XCTAssertNil(decoded.kind)
    }

    func testRowWrittenBeforeTheFieldsExistedLoadsAsAFile() throws {
        let old = """
        {"blobIdB64":"\(Data(repeating: 1, count: 32).base64EncodedString())",
         "keyB64":"\(Data(repeating: 2, count: 32).base64EncodedString())",
         "nonceB64":"\(Data(repeating: 3, count: 12).base64EncodedString())",
         "mime":"audio/mp4","size":5,"name":"voice.m4a"}
        """
        let decoded = try JSONDecoder().decode(PersistedAttachment.self, from: Data(old.utf8))
        let ref = try XCTUnwrap(decoded.toAttachmentRef())
        XCTAssertNil(ref.kind)
        XCTAssertNil(ref.durationMs)
        XCTAssertEqual(InlineMedia.decision(for: ref).bubble, .file)
    }

    func testBadOptionalMetadataIsDroppedAndTheAttachmentSurvives() throws {
        let row = """
        {"blobIdB64":"\(Data(repeating: 1, count: 32).base64EncodedString())",
         "keyB64":"\(Data(repeating: 2, count: 32).base64EncodedString())",
         "nonceB64":"\(Data(repeating: 3, count: 12).base64EncodedString())",
         "mime":"audio/mp4","size":5,"name":"voice.m4a",
         "kind":"voice","durationMs":-4,"waveformB64":"\(Data(count: 200).base64EncodedString())",
         "width":"wide","thumb":{"nope":true}}
        """
        let decoded = try JSONDecoder().decode(PersistedAttachment.self, from: Data(row.utf8))
        let ref = try XCTUnwrap(decoded.toAttachmentRef())
        XCTAssertEqual(ref.kind, "voice")
        XCTAssertNil(ref.durationMs)
        XCTAssertNil(ref.waveform, "a waveform over 128 bytes is dropped")
        XCTAssertNil(ref.width)
        XCTAssertNil(ref.thumb)
        XCTAssertEqual(ref.size, 5)
    }

    func testWholeMessageRoundTripKeepsMedia() throws {
        let message = ChatMessage(messageId: 7, direction: .incoming, text: "", attachments: [mediaRef()], createdAt: "2026-09-13T10:10:10Z")
        let persisted = PersistedMessage(from: message)
        let decoded = try JSONDecoder().decode(PersistedMessage.self, from: JSONEncoder().encode(persisted))
        XCTAssertEqual(decoded.toChatMessage()?.attachments, [mediaRef()])
    }

    // MARK: - Temp files

    @MainActor
    private func makeTemp() -> (MediaTempFiles, URL) {
        let parent = FileManager.default.temporaryDirectory
            .appendingPathComponent("MediaPersistenceTests-\(UUID().uuidString)", isDirectory: true)
        return (MediaTempFiles(parent: parent), parent)
    }

    @MainActor
    func testWritesStayInsideAPerMessageDirectoryUnderTheChatRoot() throws {
        let (temp, parent) = makeTemp()
        defer { try? FileManager.default.removeItem(at: parent) }
        let a = try temp.write(Data([1]), key: "aa11", name: "../../evil.m4a", fallback: "x")
        let b = try temp.write(Data([2]), key: "bb22", name: "../../evil.m4a", fallback: "x")
        XCTAssertEqual(a.lastPathComponent, "evil.m4a")
        XCTAssertNotEqual(a, b, "two senders' identical names never collide")
        XCTAssertTrue(a.path.hasPrefix(temp.root.path + "/"))
        XCTAssertEqual(try Data(contentsOf: a), Data([1]))
        XCTAssertEqual(try Data(contentsOf: b), Data([2]))
        XCTAssertEqual(temp.url(for: "aa11"), a)
    }

    @MainActor
    func testKeysThatAreNotPlainBecomeSafeDirectoryNames() {
        XCTAssertEqual(MediaTempFiles.directoryName(for: "local-3"), "local-3")
        XCTAssertFalse(MediaTempFiles.directoryName(for: "..").contains("."))
        XCTAssertFalse(MediaTempFiles.directoryName(for: "a/b").contains("/"))
        XCTAssertFalse(MediaTempFiles.directoryName(for: "").isEmpty)
    }

    @MainActor
    func testAliasRemoveAndCleanup() throws {
        let (temp, parent) = makeTemp()
        defer { try? FileManager.default.removeItem(at: parent) }
        let local = try temp.write(Data([1, 2]), key: "local-1", name: "voice.m4a", fallback: "v")
        temp.alias("abcd", to: "local-1")
        temp.remove(key: "local-1")
        XCTAssertTrue(FileManager.default.fileExists(atPath: local.path), "an aliased file survives its first key")
        XCTAssertEqual(temp.url(for: "abcd"), local)
        temp.remove(key: "abcd")
        XCTAssertFalse(FileManager.default.fileExists(atPath: local.path))
        XCTAssertNil(temp.url(for: "abcd"))

        _ = try temp.write(Data([3]), key: "cc33", name: "gif.gif", fallback: "g")
        temp.cleanup()
        XCTAssertFalse(FileManager.default.fileExists(atPath: temp.root.path))
        XCTAssertTrue(temp.files.isEmpty)
    }

    @MainActor
    func testAdoptMovesARecordingUnderTheRoot() throws {
        let (temp, parent) = makeTemp()
        defer { try? FileManager.default.removeItem(at: parent) }
        let source = FileManager.default.temporaryDirectory.appendingPathComponent("take-\(UUID().uuidString).m4a")
        try Data([9]).write(to: source)
        let adopted = try temp.adopt(source, key: "local-2", name: "voice-1.m4a", fallback: "v")
        XCTAssertFalse(FileManager.default.fileExists(atPath: source.path))
        XCTAssertEqual(try Data(contentsOf: adopted), Data([9]))
        XCTAssertTrue(adopted.path.hasPrefix(temp.root.path + "/"))
    }

    @MainActor
    func testSweepRemovesLeftoverRoots() throws {
        let (temp, parent) = makeTemp()
        _ = try temp.write(Data([1]), key: "dd44", name: "x", fallback: "x")
        MediaTempFiles.sweepStale(parent: parent)
        XCTAssertFalse(FileManager.default.fileExists(atPath: parent.path))
    }
}
