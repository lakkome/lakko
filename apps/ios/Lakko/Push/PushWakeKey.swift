// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// push-gw's X25519 wake key (ADR-0078 section 3).
///
/// Every `x-lakko-wake` header is a wake sealed to this key, so the Delivery
/// Service forwards a blob it cannot read. Two sources, the
/// ``DeviceAttestKey`` pattern:
///
///   * a **build-time pin**, `Info.plist` `LakkoPushWakeKey` (base64url of 32
///     bytes): when set, the key is never fetched;
///   * otherwise `GET /push/wake-key`, fetched once per process and cached.
///
/// A wrong key costs a wake (push-gw cannot open it), never confidentiality:
/// the sealed wake carries nothing the server did not see in the clear
/// before. When the key cannot be had, a publish goes out without a wake and
/// recipients catch up on their next poll.
@MainActor
public enum PushWakeKey {

    private static var cached: Data?

    /// The pinned key, or `nil` when none is configured (or it is malformed).
    static func pinnedKey() -> Data? {
        guard let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoPushWakeKey") as? String
        else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let key = base64UrlDecodeNoPad(trimmed), key.count == 32 else {
            return nil
        }
        return key
    }

    /// push-gw's wake key, or `nil` when it cannot be had right now. A failed
    /// fetch is not cached, so the next publish tries again.
    public static func key(client: PushClient = PushClient()) async -> Data? {
        if let pinned = pinnedKey() { return pinned }
        if let cached { return cached }
        do {
            let key = try await client.wakeKey()
            cached = key
            return key
        } catch {
            NSLog("push wake key unavailable; publishing without a wake")
            return nil
        }
    }
}
