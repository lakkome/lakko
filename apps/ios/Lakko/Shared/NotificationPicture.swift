// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import UserNotifications

// ADR-0047 section 6 — the picture a message notification shows: the micro
// thumbnail of the message's first photo or video, carried inside the
// message. One rule for every notification iOS shows for Lakko: the ones
// the notification extension rewrites and the ones the app posts itself.
// Compiled into the app, the notification extension and the unit tests
// (Foundation and UserNotifications only).

/// The most a notification picture may be (the core's
/// `MAX_INLINE_THUMB_BYTES`).
public let notificationPictureMaxBytes = 4096

/// `bytes` when they may become a notification's picture: a JPEG (starting
/// `FF D8 FF`) of at most ``notificationPictureMaxBytes``; `nil` otherwise.
/// The core already applied the same bound; it is checked again before
/// anything is written or handed to the system.
public func notificationPicture(_ bytes: Data?) -> Data? {
    guard let bytes, bytes.count >= 3, bytes.count <= notificationPictureMaxBytes else { return nil }
    return [UInt8](bytes.prefix(3)) == [0xFF, 0xD8, 0xFF] ? bytes : nil
}

/// Write a notification picture to a new `.jpg` file in `directory` (the
/// process's own temporary directory), for `UNNotificationAttachment`, which
/// takes a picture only as a file and moves it into the system's
/// notification store. `nil` when the bytes are refused or the write fails
/// (nothing is left behind then).
///
/// In the notification extension this does not break the read-only
/// discipline (C-59): the file is in that process's own temporary directory,
/// never the App Group container or a store the app reads, it holds only the
/// picture the notification is about to show, and the system takes it away
/// (or ``notificationPictureAttachment(_:directory:)`` deletes it when
/// attaching fails).
public func writeNotificationPicture(_ bytes: Data, directory: URL) -> URL? {
    guard let picture = notificationPicture(bytes) else { return nil }
    let url = directory.appendingPathComponent("lakko-picture-\(UUID().uuidString).jpg")
    do {
        try picture.write(to: url, options: [.atomic])
        return url
    } catch {
        try? FileManager.default.removeItem(at: url)
        return nil
    }
}

/// The picture as a notification attachment, through a temporary `.jpg` in
/// `directory`; the file is deleted when the system refuses the attachment.
/// `nil` for refused bytes or a refused attachment: the notification then
/// goes without a picture.
public func notificationPictureAttachment(
    _ bytes: Data,
    directory: URL = FileManager.default.temporaryDirectory
) -> UNNotificationAttachment? {
    guard let url = writeNotificationPicture(bytes, directory: directory) else { return nil }
    do {
        return try UNNotificationAttachment(
            identifier: "picture", url: url,
            options: [UNNotificationAttachmentOptionsTypeHintKey: "public.jpeg"])
    } catch {
        try? FileManager.default.removeItem(at: url)
        return nil
    }
}
