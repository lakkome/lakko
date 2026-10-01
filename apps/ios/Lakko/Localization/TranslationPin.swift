// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0061 section 5 — the build-time pin of the signed translation update,
/// read from `Info.plist` (the ``ReleasePin`` pattern).
///
///   * `LakkoTranslationKey` — base64url of the 32-byte Ed25519 verifying key
///     of the dedicated translation publisher (not the release key).
///     **Default empty → dormant**: no fetch, no stored bundle is used.
///   * `LakkoTranslationBundleUrl` — the bundle URL, default
///     `https://lakko.me/.well-known/lakko-translations`.
///
/// The release floor a bundle is checked against is
/// ``ReleasePin/currentRelease``, the same number the release notice uses.
enum TranslationPin {
    /// The bundle URL used when `LakkoTranslationBundleUrl` is unset / blank.
    static let defaultBundleUrl = "https://lakko.me/.well-known/lakko-translations"

    /// The decoded 32-byte verifying key, or `nil` when unset / blank /
    /// malformed. `nil` makes the whole feature dormant.
    static var verifyingKey: Data? {
        guard let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoTranslationKey") as? String else {
            return nil
        }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let key = base64UrlDecodeNoPad(trimmed), key.count == 32 else {
            return nil
        }
        return key
    }

    /// The bundle URL — the `Info.plist` value when set, else
    /// ``defaultBundleUrl``. Every device fetches the same URL whatever its
    /// language, so the request does not reveal the language.
    static var bundleUrl: URL? {
        let raw = (Bundle.main.object(forInfoDictionaryKey: "LakkoTranslationBundleUrl") as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let value = (raw?.isEmpty == false) ? raw! : defaultBundleUrl
        return URL(string: value)
    }
}
