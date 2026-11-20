// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import UIKit
import UniformTypeIdentifiers

/// `docs/ux/attachments.md` section 2.5 (amended 2026-11-18) — Paste in the
/// composer attaches a copied photo or video, through the same send preview
/// the attachment menu's Photo or Video opens. Text keeps pasting as text.
///
/// The decision is made from the pasteboard's type identifiers only, which
/// UIKit hands out without reading the content (and so without the paste
/// permission prompt); the content is read only once the person has chosen
/// Paste.
enum PastedMedia {
    /// The most items one paste attaches, the photo picker's limit.
    static let maxItems = 10

    /// UIKit's private type for a `UIImage` object put on the pasteboard. It
    /// declares no conformance, so it is named here.
    static let uiKitImageType = "com.apple.uikit.image"

    /// What one pasted item becomes.
    enum ItemKind: Equatable {
        /// A photo, loaded as the data of this type identifier.
        case photo(typeIdentifier: String)
        /// A `UIImage` object with no data representation of its own.
        case uiImage
        /// A video, loaded as the file of this type identifier.
        case video(typeIdentifier: String)
    }

    /// Whether a paste of items carrying these type identifiers (one list per
    /// item) attaches media rather than pasting text.
    ///
    /// Any text in the paste wins: a copy from a document (plain text and
    /// HTML, rich text, a note with a picture in it) pastes as text, as it
    /// always did. Otherwise a paste attaches when at least one item is a
    /// photo or a video. Anything else (a link alone, a copied file of
    /// another kind) is left to the field.
    static func attachesAsMedia(itemTypes: [[String]]) -> Bool {
        var sawMedia = false
        for types in itemTypes {
            for identifier in types {
                if identifier == uiKitImageType { sawMedia = true; continue }
                guard let type = UTType(identifier) else { continue }
                if type.conforms(to: .text) { return false }
                if type.conforms(to: .image) || type.conforms(to: .movie) { sawMedia = true }
            }
        }
        return sawMedia
    }

    /// What an item registered under these type identifiers (in its
    /// provider's order, most faithful first) is attached as, or `nil` when
    /// it is neither a photo nor a video.
    ///
    /// A photo wins over a video in the same item (a Live Photo copies as
    /// both), as the photo picker sends the still.
    static func itemKind(registered: [String]) -> ItemKind? {
        if let image = registered.first(where: { UTType($0)?.conforms(to: .image) == true }) {
            return .photo(typeIdentifier: image)
        }
        if registered.contains(uiKitImageType) { return .uiImage }
        if let movie = registered.first(where: { UTType($0)?.conforms(to: .movie) == true }) {
            return .video(typeIdentifier: movie)
        }
        return nil
    }

    /// The type identifiers of every item on `pasteboard`, read without its
    /// content.
    static func itemTypes(of pasteboard: UIPasteboard) -> [[String]] {
        let count = pasteboard.numberOfItems
        guard count > 0 else { return [] }
        return pasteboard.types(forItemSet: IndexSet(integersIn: 0..<count)) ?? []
    }

    /// One loaded item.
    enum Loaded {
        case photo(Data)
        case video(URL)
        /// The item could not be read; the payload says which kind it was.
        case failed(isVideo: Bool)
    }

    /// Read the pasted items, in order, at most ``maxItems``. A video is
    /// copied into the picked-video directory, as a picked one is.
    static func load(_ providers: [NSItemProvider]) async -> [Loaded] {
        var out: [Loaded] = []
        for provider in providers {
            guard out.count < maxItems else { break }
            guard let kind = itemKind(registered: provider.registeredTypeIdentifiers) else { continue }
            switch kind {
            case .photo(let typeIdentifier):
                if let data = await loadData(provider, typeIdentifier), !data.isEmpty {
                    out.append(.photo(data))
                } else {
                    out.append(.failed(isVideo: false))
                }
            case .uiImage:
                if let data = await loadImageObject(provider) {
                    out.append(.photo(data))
                } else {
                    out.append(.failed(isVideo: false))
                }
            case .video(let typeIdentifier):
                if let url = await loadMovie(provider, typeIdentifier) {
                    out.append(.video(url))
                } else {
                    out.append(.failed(isVideo: true))
                }
            }
        }
        return out
    }

    private static func loadData(_ provider: NSItemProvider, _ typeIdentifier: String) async -> Data? {
        await withCheckedContinuation { continuation in
            _ = provider.loadDataRepresentation(forTypeIdentifier: typeIdentifier) { data, _ in
                continuation.resume(returning: data)
            }
        }
    }

    private static func loadImageObject(_ provider: NSItemProvider) async -> Data? {
        guard provider.canLoadObject(ofClass: UIImage.self) else { return nil }
        return await withCheckedContinuation { continuation in
            _ = provider.loadObject(ofClass: UIImage.self) { object, _ in
                continuation.resume(returning: (object as? UIImage)?.pngData())
            }
        }
    }

    /// The file UIKit hands over lives only for the callback, so it is copied
    /// there, under the directory a picked video goes to.
    private static func loadMovie(_ provider: NSItemProvider, _ typeIdentifier: String) async -> URL? {
        await withCheckedContinuation { continuation in
            _ = provider.loadFileRepresentation(forTypeIdentifier: typeIdentifier) { received, _ in
                guard let received else {
                    continuation.resume(returning: nil)
                    return
                }
                let dir = PhotoVideoPrep.pickedVideoDirectory
                    .appendingPathComponent(UUID().uuidString, isDirectory: true)
                let ext = received.pathExtension.isEmpty
                    ? (UTType(typeIdentifier)?.preferredFilenameExtension ?? "mov")
                    : received.pathExtension
                let target = dir.appendingPathComponent("picked.\(ext)")
                do {
                    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                    try FileManager.default.copyItem(at: received, to: target)
                    continuation.resume(returning: target)
                } catch {
                    continuation.resume(returning: nil)
                }
            }
        }
    }
}
