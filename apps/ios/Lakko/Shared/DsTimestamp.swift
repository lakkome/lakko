// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The delivery service's own `created_at`, read as milliseconds.
///
/// One definition, in a Foundation-only file, because two PROCESSES now need
/// it and they cannot share a type that touches the FFI: the app reads a row's
/// age to decide whether a call still rings, and the notification-service
/// extension reads the same age for the same decision on a push wake. A second
/// copy in the extension would be a parser that can silently disagree with the
/// app about how old a call is — and the age is exactly what the ring window
/// is measured in.
///
/// It is the SERVER's stamp on purpose. The two devices in a call share no
/// clock, so a caller could otherwise keep an offer "fresh" by stamping it
/// itself; every age question in the call paths is asked of this string and
/// never of anything the sender wrote.
public enum DsTimestamp {

    /// Milliseconds since the Unix epoch, or `nil` when the string does not
    /// parse.
    ///
    /// A `nil` means DROP the row — never substitute a local clock, which
    /// would defeat the whole reason for reading the server's stamp.
    public static func parseMs(_ raw: String) -> Int64? {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = iso.date(from: raw) { return Int64(d.timeIntervalSince1970 * 1000) }
        iso.formatOptions = [.withInternetDateTime]
        if let d = iso.date(from: raw) { return Int64(d.timeIntervalSince1970 * 1000) }
        // The DS also serves `YYYY-MM-DD HH:MM:SS(.SSS)` (MariaDB TIMESTAMP(3)
        // rendered without a `T` or a zone). Read it as UTC, which is what the
        // column stores.
        let sql = DateFormatter()
        sql.locale = Locale(identifier: "en_US_POSIX")
        sql.timeZone = TimeZone(secondsFromGMT: 0)
        for format in ["yyyy-MM-dd HH:mm:ss.SSS", "yyyy-MM-dd HH:mm:ss"] {
            sql.dateFormat = format
            if let d = sql.date(from: raw) { return Int64(d.timeIntervalSince1970 * 1000) }
        }
        return nil
    }

    /// How old a row is, in milliseconds, clamped at zero.
    ///
    /// `nil` for a stamp that does not parse. Zero rather than a negative age
    /// when the server's clock reads ahead of this device's: an age is what
    /// the ring window is measured against, and a negative one would be a call
    /// that rings longer than the window allows.
    public static func ageMs(createdAt: String, nowMs: Int64) -> UInt64? {
        guard let rowMs = parseMs(createdAt) else { return nil }
        return UInt64(max(0, nowMs - rowMs))
    }
}
