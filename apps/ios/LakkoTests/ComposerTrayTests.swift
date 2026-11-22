// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import XCTest
@testable import Lakko

/// `docs/ux/media-messages.md` section 11 — the composer tray's rules (limits,
/// order, when Send is on, what the message carries) and the words a chat
/// list row says for a message of photos, videos and files.
@MainActor
final class ComposerTrayTests: XCTestCase {

    private func ref(_ byte: UInt8, mime: String?, kind: String? = nil, thumb: ThumbRef? = nil) -> AttachmentRef {
        AttachmentRef(
            blobId: Data(repeating: byte, count: 32), key: Data(repeating: 1, count: 32),
            nonce: Data(repeating: 2, count: 12), mime: mime, size: 1_000, name: nil, kind: kind,
            durationMs: nil, waveform: nil, width: 400, height: 300, thumb: thumb)
    }

    private func photo(_ status: ComposerTrayItem.Status = .preparing) -> ComposerTrayItem {
        ComposerTrayItem(kind: .photo, status: status)
    }

    private func file(_ status: ComposerTrayItem.Status = .preparing) -> ComposerTrayItem {
        ComposerTrayItem(kind: .file, name: "a.pdf", status: status)
    }

    // MARK: - Limits

    func testAddKeepsTheOrderAdded() {
        var tray = ComposerTray()
        let a = photo(), b = file(), c = ComposerTrayItem(kind: .video)
        tray.add([a, b])
        tray.add([c])
        XCTAssertEqual(tray.items.map(\.id), [a.id, b.id, c.id])
    }

    func testAddingPastTenPhotosAddsWhatFitsAndNamesTheMediaLimit() {
        var tray = ComposerTray()
        tray.add((0..<7).map { _ in photo() })
        let (added, limit) = tray.add((0..<5).map { _ in ComposerTrayItem(kind: .video) })
        XCTAssertEqual(added.count, 3)
        XCTAssertEqual(limit, .media)
        XCTAssertEqual(tray.mediaCount, ComposerTray.maxMedia)
    }

    func testFilesHaveTheirOwnLimitBesideTheMediaOne() {
        var tray = ComposerTray()
        tray.add((0..<10).map { _ in photo() })
        let (added, limit) = tray.add((0..<11).map { _ in file() })
        XCTAssertEqual(added.count, 10)
        XCTAssertEqual(limit, .files)
        XCTAssertEqual(tray.fileCount, ComposerTray.maxFiles)
        XCTAssertEqual(tray.mediaCount, 10)
    }

    func testBothLimitsHitNamesTheMediaLimit() {
        var tray = ComposerTray()
        tray.add((0..<10).map { _ in photo() } + (0..<10).map { _ in file() })
        let (added, limit) = tray.add([photo(), file()])
        XCTAssertTrue(added.isEmpty)
        XCTAssertEqual(limit, .media)
    }

    func testRemovingFreesRoomUnderTheLimit() {
        var tray = ComposerTray()
        let items = (0..<10).map { _ in photo() }
        tray.add(items)
        tray.remove(items[3].id)
        let (added, limit) = tray.add([photo()])
        XCTAssertEqual(added.count, 1)
        XCTAssertNil(limit)
    }

    // MARK: - Send

    func testSendNeedsTextOrAnItem() {
        let tray = ComposerTray()
        XCTAssertFalse(tray.canSend(hasText: false))
        XCTAssertTrue(tray.canSend(hasText: true))
    }

    func testSendIsOffWhileAnItemUploadsOrFailed() {
        var tray = ComposerTray()
        let a = photo(.ready), b = photo(.uploading(0.4))
        tray.add([a, b])
        XCTAssertFalse(tray.canSend(hasText: true))
        tray.update(b.id) { $0.status = .failed }
        XCTAssertFalse(tray.canSend(hasText: true))
        XCTAssertTrue(tray.hasFailed)
        tray.remove(b.id)
        XCTAssertTrue(tray.canSend(hasText: false))
    }

    func testRefsAreTheReadyItemsInOrder() {
        var tray = ComposerTray()
        var a = photo(.ready), b = file(.ready)
        a.ref = ref(1, mime: "image/jpeg")
        b.ref = ref(2, mime: "application/pdf")
        tray.add([a, b])
        XCTAssertEqual(tray.refs.map(\.blobId), [Data(repeating: 1, count: 32), Data(repeating: 2, count: 32)])
        XCTAssertEqual(tray.mediaItems.map(\.id), [a.id])
    }

    // MARK: - The store

    /// A worker that reports and finishes as the test says.
    private func store(
        result: @escaping (ComposerTraySource) -> Result<AttachmentRef, Error>
    ) -> ComposerTrayStore {
        ComposerTrayStore(worker: { source, _, report in
            switch source {
            case .photo: report(.prepared(kind: .photo, name: nil, size: 10, durationMs: nil, thumbnail: Data([1])))
            case .video: report(.prepared(kind: .video, name: nil, size: 10, durationMs: 4_000, thumbnail: nil))
            case .file(_, let name, _): report(.prepared(kind: .file, name: name, size: 3, durationMs: nil, thumbnail: nil))
            case .audio(let url): report(.prepared(kind: .file, name: url.lastPathComponent, size: 3, durationMs: nil, thumbnail: nil))
            }
            report(.progress(0.5))
            return try result(source).get()
        })
    }

    private func settle(_ store: ComposerTrayStore) async {
        for _ in 0..<50 where store.tray.isBusy {
            await Task.yield()
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
    }

    func testStoreUploadsEachItemAndSendTakesEveryRefInOrder() async {
        var counter: UInt8 = 0
        let store = store { _ in
            counter += 1
            return .success(self.ref(counter, mime: "image/jpeg"))
        }
        store.add([.photo(Data([9])), .file(Data([1, 2, 3]), name: "a.txt", mime: "text/plain")])
        XCTAssertEqual(store.tray.items.count, 2)
        await settle(store)
        XCTAssertEqual(store.tray.items.map(\.status), [.ready, .ready])
        XCTAssertEqual(store.tray.items[1].name, "a.txt")
        XCTAssertTrue(store.tray.canSend(hasText: false))
        let refs = store.takeForSend()
        XCTAssertEqual(refs.count, 2)
        XCTAssertTrue(store.tray.isEmpty)
    }

    func testAFailedUploadIsRetriedInPlace() async {
        var fail = true
        let store = store { _ in
            if fail { return .failure(URLError(.notConnectedToInternet)) }
            return .success(self.ref(7, mime: "image/jpeg"))
        }
        store.add([.photo(Data([9]))])
        await settle(store)
        let id = store.tray.items[0].id
        XCTAssertEqual(store.tray.items[0].status, .failed)
        XCTAssertFalse(store.tray.canSend(hasText: true))
        fail = false
        store.retry(id)
        await settle(store)
        XCTAssertEqual(store.tray.item(id)?.status, .ready)
        XCTAssertEqual(store.tray.items.count, 1)
    }

    func testARefusedItemLeavesTheTrayAndSaysWhy() async {
        let store = store { _ in .failure(ComposerTrayRefusal(text: .key("attach.preview.file_too_big"))) }
        store.add([.file(Data([1]), name: "big.bin", mime: nil)])
        await settle(store)
        XCTAssertTrue(store.tray.isEmpty)
        XCTAssertEqual(store.problem?.text, UiText.key("attach.preview.file_too_big").text)
    }

    func testAddingPastTheLimitShowsTheNotice() {
        let store = store { _ in .success(self.ref(1, mime: "image/jpeg")) }
        store.add((0..<12).map { _ in .photo(Data([1])) })
        XCTAssertEqual(store.tray.mediaCount, 10)
        XCTAssertEqual(store.limitNotice, .media)
    }

    func testTheTrayOfAConversationOutlivesItsScreen() {
        let trays = ComposerTrays()
        let first = trays.store(for: "conv-a")
        XCTAssertTrue(first === trays.store(for: "conv-a"))
        XCTAssertFalse(first === trays.store(for: "conv-b"))
        trays.removeAll()
        XCTAssertFalse(first === trays.store(for: "conv-a"))
    }

    // MARK: - Preview words

    func testWordsForPhotosVideosAndFiles() {
        func words(_ refs: [AttachmentRef]) -> String? {
            ChatListFormatting.attachmentWords(ChatListFormatting.attachmentCounts(refs))
        }
        let p = ref(1, mime: "image/jpeg"), v = ref(2, mime: "video/mp4"), f = ref(3, mime: "application/pdf")
        XCTAssertEqual(words([p]), "Photo")
        XCTAssertEqual(words([p, p, p]), "3 photos")
        XCTAssertEqual(words([v]), "Video")
        XCTAssertEqual(words([v, v]), "2 videos")
        XCTAssertEqual(words([p, v, p, v]), "4 photos and videos")
        XCTAssertEqual(words([p, f]), "Photo")
        XCTAssertEqual(words([f]), "File")
        XCTAssertEqual(words([f, f, f]), "3 files")
        XCTAssertNil(words([]))
        // A voice message keeps its own word.
        XCTAssertNil(words([ref(4, mime: "audio/mp4", kind: "voice")]))
    }

    func testThePreviewLineUsesTheWordsWithoutText() {
        let refs = [ref(1, mime: "image/jpeg"), ref(2, mime: "image/png")]
        XCTAssertEqual(
            ChatListFormatting.preview(
                text: "", attachmentLabels: ChatListFormatting.attachmentLabels(refs), direction: .incoming,
                isGroup: false, senderName: nil),
            "2 photos")
        XCTAssertEqual(
            ChatListFormatting.preview(
                text: "Look", attachmentLabels: ChatListFormatting.attachmentLabels(refs), direction: .incoming,
                isGroup: false, senderName: nil),
            "Look")
    }

    func testTheRowLineForTextWithFilesPhotosOrNothing() {
        let f = ref(1, mime: "application/pdf"), p = ref(2, mime: "image/jpeg")
        func line(_ text: String, _ refs: [AttachmentRef]) -> String {
            ChatListFormatting.preview(
                text: ChatListFormatting.textWithFileWords(text, refs: refs),
                attachmentLabels: ChatListFormatting.attachmentLabels(refs), direction: .incoming,
                isGroup: false, senderName: nil)
        }
        // Files only, with text: the words, the separator, the text.
        XCTAssertEqual(line("Weekend plans", [f, f]), "2 files · Weekend plans")
        XCTAssertEqual(line("Weekend plans", [f]), "File · Weekend plans")
        // Photos or videos, with text: the text alone.
        XCTAssertEqual(line("Weekend plans", [p, f]), "Weekend plans")
        XCTAssertEqual(line("Weekend plans", [p]), "Weekend plans")
        // No text: the words alone.
        XCTAssertEqual(line("", [f, f]), "2 files")
        XCTAssertEqual(line("", [p, p]), "2 photos")
    }

    func testTheRowThumbnailIsTheFirstPhotoOrVideoWithItsStack() {
        let thumb = ThumbRef(
            blobId: Data(repeating: 5, count: 32), key: Data(repeating: 1, count: 32),
            nonce: Data(repeating: 2, count: 12), mime: "image/jpeg", size: 100, width: 32, height: 24)
        let refs = [ref(1, mime: "application/pdf"), ref(2, mime: "video/mp4", thumb: thumb), ref(3, mime: "image/jpeg")]
        let media = ChatListFormatting.previewMedia(refs)
        XCTAssertEqual(media?.thumb?.blobId, thumb.blobId)
        XCTAssertEqual(media?.isVideo, true)
        XCTAssertEqual(media?.count, 2)
        XCTAssertEqual(media?.stackedEdges, 1)
        XCTAssertEqual(ConversationPreviewMedia(thumb: nil, isVideo: false, count: 1).stackedEdges, 0)
        XCTAssertEqual(ConversationPreviewMedia(thumb: nil, isVideo: false, count: 7).stackedEdges, 2)
        XCTAssertNil(ChatListFormatting.previewMedia([ref(1, mime: "application/pdf")]))
    }

    // MARK: - Mosaic items

    func testTheMosaicTakesPhotosAndVideosInOrderUpToTheCap() {
        let refs = (0..<12).map { ref(UInt8($0), mime: $0 % 2 == 0 ? "image/jpeg" : "video/mp4") }
            + [ref(99, mime: "application/pdf")]
        let items = mosaicItems(refs)
        XCTAssertEqual(items.count, Int(mediaMosaicMaxItems()))
        XCTAssertEqual(items.first?.blobId, refs[0].blobId)
        let layout = mosaicLayout(Array(items.prefix(3)), column: nil)
        XCTAssertEqual(layout.tiles.count, 3)
        XCTAssertEqual(layout.width, 260)
    }
}
