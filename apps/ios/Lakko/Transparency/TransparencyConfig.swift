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
    /// network — like the operator key). Retained as the BACKWARD-COMPAT
    /// source for ``pinnedWitnessKeys``: a deployment pinning a single
    /// witness via the pre-ADR-0039 `LakkoTransparencyWitnessKey` key keeps
    /// working (the key-set helper folds it into a one-element set at
    /// threshold 1). New deployments use the `..._WitnessKeys` set below.
    /// Mirror of the web `pinnedWitnessKey()`.
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

    /// The build-time pinned WITNESS verifying key SET (each 32 bytes) for
    /// the K-of-N witness QUORUM gate (ADR-0039, G-30 finding-1). Read from
    /// `Info.plist` `LakkoTransparencyWitnessKeys` — a comma-separated list
    /// of base64url (standard-alphabet base64 is also accepted; padding is
    /// optional) 32-byte Ed25519 verifying keys.
    ///
    /// Parsing is lenient in the same spirit as ``pinnedWitnessKey`` (which
    /// mapped a bad key to "unconfigured"): a blank entry is skipped, and an
    /// entry that does not decode to exactly 32 bytes is DROPPED. Dropping is
    /// fail-SAFE — a smaller pinned-key pool can only make the quorum HARDER
    /// to satisfy (fewer distinct keys can contribute), never easier; a set
    /// that shrinks below the threshold simply makes the gate refuse every
    /// root (fail-closed).
    ///
    /// Returns `[]` when unset / all-blank / all-malformed → the witness gate
    /// is OFF (dormant, the operator-only transition posture, exactly the
    /// single-key no-key case).
    ///
    /// BACKWARD-COMPAT: when `LakkoTransparencyWitnessKeys` is unset (or
    /// contributes no valid key) but the OLD singular
    /// `LakkoTransparencyWitnessKey` is set, that one key is returned as a
    /// one-element set (pair it with the default ``witnessThreshold`` of 1) —
    /// so an existing single-witness deployment keeps working unchanged.
    /// Mirror of the web `pinnedWitnessKeys()`.
    public static var pinnedWitnessKeys: [Data] {
        if let raw = Bundle.main.object(
            forInfoDictionaryKey: "LakkoTransparencyWitnessKeys"
        ) as? String {
            let trimmedRaw = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmedRaw.isEmpty {
                var keys: [Data] = []
                for part in trimmedRaw.split(separator: ",", omittingEmptySubsequences: false) {
                    let entry = part.trimmingCharacters(in: .whitespacesAndNewlines)
                    if entry.isEmpty { continue } // skip blanks
                    // Drop an undecodable / wrong-length entry (mirrors the
                    // single-key leniency). `base64UrlDecodeNoPad` also accepts
                    // the standard alphabet + optional padding.
                    if let key = base64UrlDecodeNoPad(entry), key.count == 32 {
                        keys.append(key)
                    }
                }
                if !keys.isEmpty { return keys }
            }
        }
        // Backward-compat: fall back to the OLD singular pin as a 1-element set.
        if let single = pinnedWitnessKey {
            return [single]
        }
        return []
    }

    /// The K-of-N witness quorum THRESHOLD (ADR-0039) — the minimum number
    /// of DISTINCT pinned witnesses that must each have co-signed the served
    /// root. Read from `Info.plist` `LakkoTransparencyWitnessThreshold`;
    /// defaults to `1` (unset / unparseable / non-positive). Only consulted
    /// when ``pinnedWitnessKeys`` is non-empty. A threshold LARGER than the
    /// pinned-key count makes the quorum unsatisfiable → the gate fails closed
    /// (refuses every root), the correct posture for that misconfiguration.
    /// Mirror of the web `witnessThreshold()`.
    public static var witnessThreshold: Int {
        // Info.plist values can arrive as a String (the xcodegen default) or a
        // Number; accept either, coercing to a positive Int (else 1).
        let raw = Bundle.main.object(
            forInfoDictionaryKey: "LakkoTransparencyWitnessThreshold"
        )
        if let n = raw as? Int, n >= 1 { return n }
        if let s = raw as? String {
            let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
            if let n = Int(trimmed), n >= 1 { return n }
        }
        return 1
    }
}
