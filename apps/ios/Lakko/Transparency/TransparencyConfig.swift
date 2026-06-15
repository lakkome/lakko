// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Build-time configuration for the transparency-log integration
/// (KT-log S3d).
///
/// Mirror of `apps/web/src/transparency/config.ts` (S3c). Where the web
/// reads two Vite env vars, the iOS client reads two `Info.plist` keys
/// (set per build configuration), exactly as ``SfuConfig`` reads
/// `LakkoSfuUrl` and ``DeviceAttestKey`` reads `LakkoDeviceAttestKey`:
///
///   * `LakkoTransparencyUrl` — the transparency service base URL
///     (e.g. `https://transparency.lakko.me`, no trailing path).
///     **Unset / blank → the whole log check is OFF.**
///
///   * `LakkoTransparencyOperatorKey` — base64 (standard alphabet) of
///     the operator's 32-byte Ed25519 verifying key. This is the trust
///     anchor for every signed root and is **pinned out-of-band at
///     build time**. The client NEVER trusts the
///     `/transparency/operator-key` endpoint for verification — a
///     malicious serving tier cannot swap the operator key. **Unset /
///     malformed / wrong-length → the log check is OFF** (the same skip
///     path as a missing URL).
///
/// The skip path is deliberately fail-OPEN for availability: the
/// transparency log is an equivocation-detection / gossip layer ON TOP
/// of the existing supersession/pin gate (ADR-0016), which remains the
/// primary, fail-closed trust mechanism. A deployment that has not yet
/// provisioned the operator key (or points at no transparency service)
/// simply runs without the extra check rather than refusing to start.
public enum TransparencyConfig {

    /// The configured transparency-service base URL, or `nil` if unset
    /// / blank. Trailing slashes are trimmed so callers can append
    /// `/transparency/...` cleanly.
    public static var baseUrl: String? {
        guard
            let raw = Bundle.main.object(
                forInfoDictionaryKey: "LakkoTransparencyUrl"
            ) as? String
        else {
            return nil
        }
        var trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        while trimmed.hasSuffix("/") {
            trimmed.removeLast()
        }
        return trimmed.isEmpty ? nil : trimmed
    }

    /// The build-time pinned operator verifying key (32 bytes), or
    /// `nil` when the `Info.plist` key is unset / blank / malformed /
    /// wrong-length.
    ///
    /// Standard-alphabet base64 (the wire shape every binary auth field
    /// uses, and what `GET /transparency/operator-key` itself returns)
    /// — but here it is read from the build config, never from the
    /// network.
    public static var pinnedOperatorKey: Data? {
        guard
            let raw = Bundle.main.object(
                forInfoDictionaryKey: "LakkoTransparencyOperatorKey"
            ) as? String
        else {
            return nil
        }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let key = base64Decode(trimmed), key.count == 32 else {
            return nil
        }
        return key
    }

    /// The build-time pinned WITNESS verifying key (32 bytes), or `nil`
    /// when the `Info.plist` key (`LakkoTransparencyWitnessKey`) is unset
    /// / blank / malformed / wrong-length (ADR-0020 split-view defence).
    ///
    /// Standard-alphabet base64, read from the build config (never the
    /// network — like the operator key). When set, the client REQUIRES a
    /// valid witness co-signature over every fetched root (a root without
    /// one is refused, fail-closed); when `nil`, the witness gate is OFF
    /// and the client keeps the operator-only posture (the transition
    /// fallback). Mirror of the web `pinnedWitnessKey()`.
    public static var pinnedWitnessKey: Data? {
        guard
            let raw = Bundle.main.object(
                forInfoDictionaryKey: "LakkoTransparencyWitnessKey"
            ) as? String
        else {
            return nil
        }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, let key = base64Decode(trimmed), key.count == 32 else {
            return nil
        }
        return key
    }
}
