// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import Foundation
import ImageIO
import LakkoCore
import UniformTypeIdentifiers

/// The camera background chosen on this device (UX
/// `docs/ux/call-backgrounds.md` section 8): `none`, `blur`, `strong-blur`,
/// `builtin:<id>` or `photo:<id>`, as the Rust core reads it. Not synced,
/// never sent.
///
/// Filed under the account, because a `photo:` choice names one of that
/// account's photos, and wiped with it. Written only while the account's
/// session is open (``AccountStoreGate``).
struct BackgroundChoiceStore {
    static let keyPrefix = "lakko.call.background."

    let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(accountIdB64: String) -> String { keyPrefix + accountIdB64 }

    /// The stored text, or empty (which reads as `none`).
    func stored(accountIdB64: String) -> String {
        defaults.string(forKey: Self.key(accountIdB64: accountIdB64)) ?? ""
    }

    /// Store `stored`. Not written for a sealed account.
    func set(_ stored: String, accountIdB64: String) {
        guard let hold = AccountStoreGate.enter(accountIdB64) else { return }
        defer { hold.leave() }
        defaults.set(stored, forKey: Self.key(accountIdB64: accountIdB64))
    }

    func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }
}

/// How a stored choice becomes what the camera stage applies.
enum BackgroundResolution {
    /// The effect for `stored`. The Rust reader decides (an unreadable value
    /// is `blur`); an own photo that cannot be loaded is `blur` too, so a
    /// missing file hides the room rather than showing it (UX section 3).
    static func effect(
        stored: String, loadPhoto: (String) -> CGImage?
    ) -> BackgroundEffect {
        let choice = backgroundChoiceFromStored(stored: stored)
        switch choice.kind {
        case .none: return .none
        case .blur: return .blur(strong: false)
        case .strongBlur: return .blur(strong: true)
        case .builtin: return .builtin(id: choice.id)
        case .photo:
            guard let image = loadPhoto(choice.id) else { return .blur(strong: false) }
            return .photo(id: choice.id, image: image)
        }
    }

    /// The stored text for a photo id.
    static func stored(photoId: String) -> String {
        backgroundChoiceToStored(choice: BackgroundChoice(kind: .photo, id: photoId))
    }

    /// The stored text for a built-in id.
    static func stored(builtinId: String) -> String {
        backgroundChoiceToStored(choice: BackgroundChoice(kind: .builtin, id: builtinId))
    }
}

/// The person's own background photos (UX section 2.3): at most twelve, the
/// longer side at most 1920 px, on this device only, under the account and
/// wiped with it (ADR-0077: written only while the account's session is
/// open).
///
/// `<Application Support>/call-backgrounds/<account>/` holds one JPEG per
/// photo and `index.json`, the ids with when each was added.
struct BackgroundPhotoStore {
    static let limit = 12
    static let maxSide = 1920
    static let directoryName = "call-backgrounds"
    static let indexName = "index.json"

    struct Photo: Codable, Equatable, Identifiable {
        let id: String
        let addedAtMs: UInt64
    }

    enum StoreError: Error, Equatable {
        /// The picture could not be read or written as a picture.
        case unusable
        /// The account's session has ended: nothing is written.
        case sealed
    }

    let root: URL

    init(root: URL = BackgroundPhotoStore.defaultRoot) {
        self.root = root
    }

    static var defaultRoot: URL {
        let base = (try? FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent(directoryName, isDirectory: true)
    }

    /// The account's folder: the id, unpadded, with `/` as `_`.
    func directory(accountIdB64: String) -> URL {
        let safe = accountIdB64
            .replacingOccurrences(of: "=", with: "")
            .replacingOccurrences(of: "/", with: "_")
        return root.appendingPathComponent(safe, isDirectory: true)
    }

    private func indexURL(_ accountIdB64: String) -> URL {
        directory(accountIdB64: accountIdB64).appendingPathComponent(Self.indexName)
    }

    private func fileURL(_ id: String, _ accountIdB64: String) -> URL {
        directory(accountIdB64: accountIdB64).appendingPathComponent("\(id).jpg")
    }

    /// The photos, newest first. A photo whose file is gone is not listed.
    func list(accountIdB64: String) -> [Photo] {
        readIndex(accountIdB64)
            .filter { FileManager.default.fileExists(atPath: fileURL($0.id, accountIdB64).path) }
            .sorted { $0.addedAtMs > $1.addedAtMs }
    }

    /// The photo `id`, or `nil` when it is missing or cannot be read.
    func load(id: String, accountIdB64: String) -> CGImage? {
        guard backgroundIsPhotoId(id: id) else { return nil }
        let url = fileURL(id, accountIdB64)
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        return CGImageSourceCreateImageAtIndex(source, 0, nil)
    }

    /// A small copy of photo `id` for the strip, or `nil`.
    func thumbnail(id: String, accountIdB64: String, maxPixel: Int = 192) -> CGImage? {
        guard backgroundIsPhotoId(id: id),
              let source = CGImageSourceCreateWithURL(fileURL(id, accountIdB64) as CFURL, nil)
        else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixel,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

    /// Add `image` (already upright), scaled so its longer side is at most
    /// ``maxSide``. Over ``limit``, the oldest photos that are not
    /// `selectedId` are removed.
    @discardableResult
    func add(
        _ image: CGImage, accountIdB64: String, selectedId: String?,
        nowMs: UInt64 = UInt64(Date().timeIntervalSince1970 * 1000)
    ) throws -> Photo {
        guard let jpeg = Self.encode(image) else { throw StoreError.unusable }
        guard let hold = AccountStoreGate.enter(accountIdB64) else { throw StoreError.sealed }
        defer { hold.leave() }
        let dir = directory(accountIdB64: accountIdB64)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        var excluded = dir
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? excluded.setResourceValues(values)

        var index = readIndex(accountIdB64)
        let photo = Photo(id: Self.newId(nowMs: nowMs, taken: Set(index.map(\.id))), addedAtMs: nowMs)
        try jpeg.write(to: fileURL(photo.id, accountIdB64), options: [.atomic])
        index.append(photo)
        // Oldest first; the selected one and the new one stay.
        let removable = index
            .filter { $0.id != selectedId && $0.id != photo.id }
            .sorted { $0.addedAtMs < $1.addedAtMs }
        var excess = index.count - Self.limit
        var removed: Set<String> = []
        for old in removable where excess > 0 {
            removed.insert(old.id)
            excess -= 1
        }
        for id in removed {
            try? FileManager.default.removeItem(at: fileURL(id, accountIdB64))
        }
        index.removeAll { removed.contains($0.id) }
        try writeIndex(index, accountIdB64)
        return photo
    }

    /// Remove photo `id`. Nothing for a sealed account.
    func remove(id: String, accountIdB64: String) {
        guard let hold = AccountStoreGate.enter(accountIdB64) else { return }
        defer { hold.leave() }
        try? FileManager.default.removeItem(at: fileURL(id, accountIdB64))
        var index = readIndex(accountIdB64)
        index.removeAll { $0.id == id }
        try? writeIndex(index, accountIdB64)
    }

    /// The account wipe: every photo of the account.
    func delete(accountIdB64: String) throws {
        let dir = directory(accountIdB64: accountIdB64)
        guard FileManager.default.fileExists(atPath: dir.path) else { return }
        try FileManager.default.removeItem(at: dir)
    }

    // MARK: - Internals

    private func readIndex(_ accountIdB64: String) -> [Photo] {
        guard let data = try? Data(contentsOf: indexURL(accountIdB64)),
              let index = try? JSONDecoder().decode([Photo].self, from: data)
        else { return [] }
        return index.filter { backgroundIsPhotoId(id: $0.id) }
    }

    private func writeIndex(_ index: [Photo], _ accountIdB64: String) throws {
        let data = try JSONEncoder().encode(index)
        try data.write(to: indexURL(accountIdB64), options: [.atomic])
    }

    /// `p<ms>-<random>`, unique among `taken`; satisfies the Rust photo-id
    /// rule.
    static func newId(nowMs: UInt64, taken: Set<String>) -> String {
        while true {
            let id = "p\(nowMs)-\(String(UInt32.random(in: .min ... .max), radix: 16))"
            if !taken.contains(id) { return id }
        }
    }

    /// `image` scaled so its longer side is at most ``maxSide``, as JPEG.
    static func encode(_ image: CGImage) -> Data? {
        guard image.width > 0, image.height > 0 else { return nil }
        let longer = max(image.width, image.height)
        let scale = longer > maxSide ? Double(maxSide) / Double(longer) : 1
        let width = max(1, Int((Double(image.width) * scale).rounded()))
        let height = max(1, Int((Double(image.height) * scale).rounded()))
        let space = CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB()
        guard let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        else { return nil }
        context.interpolationQuality = .high
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        guard let scaled = context.makeImage() else { return nil }
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(
            data, UTType.jpeg.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(
            destination, scaled,
            [kCGImageDestinationLossyCompressionQuality: 0.9] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return data as Data
    }
}
