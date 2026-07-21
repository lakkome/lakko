// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0038 S4 — the build-time-pinned "update available" notice parameters,
/// read from the app's `Info.plist` (the same pinning pattern as ``BootstrapPin``
/// / ``DeviceAttestKey`` / ``SfuConfig``).
///
/// Five keys, set per build configuration:
///
///   * `LakkoReleaseKey` — base64url of the 32-byte Ed25519 verifying key the
///     signed release manifest is checked against (the dedicated, offline
///     **publisher** key, ADR-0038 §3). **Default empty → the whole feature is
///     DORMANT** (no fetch, no verify, no banner). There is no serving-tier
///     fallback (unlike ``DeviceAttestKey``'s JWKS): the release key is pin-only,
///     so an unpinned build simply has the notice off — fail-safe-toward-silence.
///   * `LakkoReleaseManifestUrl` — the manifest URL (a raw 89-byte signed blob),
///     default `https://lakko.me/.well-known/lakko-release`.
///   * `LakkoAppStoreUrl` — the outbound App Store deep-link. **Default empty**
///     because no production App Store id exists yet; when empty the Update
///     button is hidden and (per the S4 gate) the banner stays dormant unless
///     `LakkoReleaseCheckDevOverride` is set.
///   * `LakkoCurrentRelease` — this build's monotone release integer (default 1),
///     compared as an integer against the manifest's `latest_release` /
///     `min_supported_release`.
///   * `LakkoReleaseCheckDevOverride` — a bool that forces the banner to show
///     even without a store URL, for pre-launch dogfood testing (default off).
///
/// The manifest URL / current release / store URL are transport + display hints;
/// the security boundary is the Ed25519 signature on the manifest, verified in
/// the audited Rust core against ``verifyingKey``.
enum ReleasePin {
    /// The manifest URL used when `LakkoReleaseManifestUrl` is unset / blank.
    static let defaultManifestUrl = "https://lakko.me/.well-known/lakko-release"

    /// The decoded 32-byte Ed25519 publisher verifying key, or `nil` when unset /
    /// blank / malformed. `nil` makes the whole feature dormant.
    static var verifyingKey: Data? {
        guard let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoReleaseKey") as? String else {
            return nil
        }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let key = base64UrlDecodeNoPad(trimmed), key.count == 32 else {
            return nil
        }
        return key
    }

    /// The signed-manifest URL — the `Info.plist` value when set, else
    /// ``defaultManifestUrl``.
    static var manifestUrl: URL? {
        let raw = (Bundle.main.object(forInfoDictionaryKey: "LakkoReleaseManifestUrl") as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let value = (raw?.isEmpty == false) ? raw! : defaultManifestUrl
        return URL(string: value)
    }

    /// The outbound App Store deep-link, or `nil` when unset / blank / malformed.
    /// `nil` hides the Update button (and, absent ``devOverride``, the banner).
    static var appStoreUrl: URL? {
        guard let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoAppStoreUrl") as? String else {
            return nil
        }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let url = URL(string: trimmed) else {
            return nil
        }
        return url
    }

    /// This build's monotone release integer (`LakkoCurrentRelease`), default 1.
    static var currentRelease: UInt64 {
        let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoCurrentRelease")
        if let n = raw as? Int, n >= 0 {
            return UInt64(n)
        }
        if let s = raw as? String {
            let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
            if let parsed = UInt64(trimmed) {
                return parsed
            }
        }
        return 1
    }

    /// The dev/internal override forcing the banner even without a store URL
    /// (`LakkoReleaseCheckDevOverride`), default `false`.
    static var devOverride: Bool {
        let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoReleaseCheckDevOverride")
        if let b = raw as? Bool {
            return b
        }
        if let s = raw as? String {
            return (s as NSString).boolValue
        }
        return false
    }
}
