// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0026 — the build-time-pinned bootstrap-rendezvous parameters for arming
/// the obfuscated tunnel, read from the app's `Info.plist` (the same pinning
/// pattern as `DeviceAttestKey` / `SfuConfig`).
///
/// Three keys, all set per build configuration:
///
///   * `LakkoBootstrapTrustAnchor` — base64 of the 32-byte Ed25519 verifying
///     key the signed bootstrap config is checked against.
///   * `LakkoBootstrapDohDomain` — the rendezvous domain resolved over JSON-DoH.
///   * `LakkoBootstrapConfigPath` — the path of the signed config on that domain
///     (e.g. `/bootstrap.json`).
///
/// Returns `nil` when any key is unset / blank / malformed, so a build that did
/// not pin the anchor simply cannot arm — there is no insecure default. The
/// trust anchor is the only security-bearing value; the domain + path are
/// transport hints (acceptance hinges on the Ed25519 signature, not the path).
enum BootstrapPin {
    /// The decoded 32-byte Ed25519 trust anchor, or `nil` if absent / malformed.
    static var trustAnchor: Data? {
        guard
            let raw = Bundle.main.object(forInfoDictionaryKey: "LakkoBootstrapTrustAnchor") as? String
        else {
            return nil
        }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard
            !trimmed.isEmpty,
            let data = Data(base64Encoded: trimmed),
            data.count == 32
        else {
            return nil
        }
        return data
    }

    /// The DoH-resolved rendezvous domain, or `nil` if unset / blank.
    static var dohDomain: String? {
        plistString("LakkoBootstrapDohDomain")
    }

    /// The signed-config path on the rendezvous domain, or `nil` if unset.
    static var configPath: String? {
        plistString("LakkoBootstrapConfigPath")
    }

    /// `true` only when all three are present — the precondition for arming.
    static var isComplete: Bool {
        trustAnchor != nil && dohDomain != nil && configPath != nil
    }

    private static func plistString(_ key: String) -> String? {
        guard let raw = Bundle.main.object(forInfoDictionaryKey: key) as? String else {
            return nil
        }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}
