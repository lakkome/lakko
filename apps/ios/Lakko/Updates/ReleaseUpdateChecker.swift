// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
#if canImport(UIKit)
import UIKit
#endif

/// ADR-0038 S4 — the "update available" notice controller (iOS).
///
/// Mirror of the structure ``AntiCensorshipManager`` / ``DeviceAttestKey``
/// establish: a process-scoped `@MainActor` observable that, on launch (throttled
/// to at most once per 24h), fetches a small signed release manifest, verifies its
/// Ed25519 signature against the build-time-pinned publisher key, computes the
/// update status against this build's release integer, and publishes a
/// ``UpdateBannerDecision`` the ``UpdateAvailableBanner`` renders.
///
/// **Fail-safe-toward-silence (ADR-0038 §3, UX §2).** This is an
/// availability / anti-phishing control, NOT an E2EE gate — its "fail" is *show
/// no nag*, distinct from invariant #4's fail-closed-refuse. So:
///   * an unpinned build (no `LakkoReleaseKey`) is fully dormant — no fetch;
///   * ANY fetch / verify failure resolves to "up to date" (no banner), so a
///     spoofed or garbage manifest can never drive an update nag;
///   * the transport is not the security boundary — the fetch rides the
///     anti-censorship tunnel when armed (so a censored network still reaches the
///     manifest) exactly like every other native REST call; the Ed25519 signature
///     is the trust anchor either way.
@MainActor
final class ReleaseUpdateChecker: ObservableObject {
    /// Process-wide controller. ``AppRoot`` observes it + triggers the launch
    /// check; the banner reads ``decision`` and routes Update / Dismiss back here.
    static let shared = ReleaseUpdateChecker()

    /// The resolved banner visibility — recomputed after each check and on
    /// dismiss. Drives the ``UpdateAvailableBanner`` mount in ``AppRoot``.
    @Published private(set) var decision: UpdateBannerDecision = .hidden

    // Persisted, non-secret UI state (a version integer + a timestamp — no PII,
    // no key material, so plain `UserDefaults`, invariant #1 not engaged).
    private static let kLastCheckAt = "lakko.release.lastCheckAtMillis"
    private static let kCachedLatest = "lakko.release.cachedLatestRelease"
    private static let kCachedMin = "lakko.release.cachedMinSupportedRelease"
    private static let kDismissedRelease = "lakko.release.dismissedRelease"
    private static let kDismissedAt = "lakko.release.dismissedAtMillis"

    /// Fetch throttle + cold-launch re-nag window (24h in millis).
    static let checkThrottleMillis: Int64 = 24 * 60 * 60 * 1000
    static let reNagAfterMillis: Int64 = 24 * 60 * 60 * 1000

    private let defaults: UserDefaults
    private var dismissedThisSession = false
    /// The `latest_release` the current decision was computed against — the key a
    /// dismiss is stored under.
    private var offeredLatest: UInt64 = 0

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Fetch + verify (throttled) then publish the banner decision. Called once at
    /// app launch; safe to call repeatedly (idempotent, cheap when not due).
    func checkOnLaunch(now: Date = Date()) async {
        // Dormant unless the publisher key is pinned into this build — default
        // empty means the feature is off (fail-safe-toward-silence). No fetch.
        guard let key = ReleasePin.verifyingKey, let url = ReleasePin.manifestUrl else {
            decision = .hidden
            return
        }
        let nowMillis = Self.millis(now)
        if isCheckDue(nowMillis: nowMillis) {
            await fetchAndVerify(url: url, key: key, nowMillis: nowMillis)
        }
        recompute(nowMillis: nowMillis)
    }

    /// The user dismissed the banner: gone for the rest of this run, and the
    /// dismissed target + timestamp persist so a later cold launch re-nags per the
    /// ``updateBannerDecision`` cadence (24h, or immediately on a newer release).
    func dismiss(now: Date = Date()) {
        dismissedThisSession = true
        defaults.set(String(offeredLatest), forKey: Self.kDismissedRelease)
        defaults.set(NSNumber(value: Self.millis(now)), forKey: Self.kDismissedAt)
        decision = .hidden
    }

    /// Open the configured App Store deep-link. No-op when no store URL is set
    /// (the button is hidden in that case, so this is defensive).
    func openStore() {
        guard let url = ReleasePin.appStoreUrl else { return }
        #if canImport(UIKit)
        UIApplication.shared.open(url)
        #endif
    }

    // MARK: - Internals

    private func isCheckDue(nowMillis: Int64) -> Bool {
        guard let last = (defaults.object(forKey: Self.kLastCheckAt) as? NSNumber)?.int64Value else {
            return true
        }
        return nowMillis - last >= Self.checkThrottleMillis
    }

    private func fetchAndVerify(url: URL, key: Data, nowMillis: Int64) async {
        do {
            var request = URLRequest(url: url)
            request.httpMethod = "GET"
            // ADR-0026 — ride the obfuscated tunnel when armed (transport is not
            // the security boundary; the signature is), disarmed = direct.
            let (data, response) = try await AntiCensorshipTransport.perform(
                request, session: .shared, config: .current)
            if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                // Not a signed verdict — do NOT bump the throttle (retry next
                // launch) and do NOT touch the cache. Resolve to up-to-date.
                NSLog("[release] manifest fetch HTTP \(http.statusCode) — treating as up to date")
                return
            }
            // Freshness DISABLED per ADR-0038 (Int64.max) — a stale manifest can
            // only suppress a notice, never forge one (the LOW replay residual).
            let verified = try verifyReleaseManifest(
                manifest: data,
                verifyingKey: key,
                nowMillis: nowMillis,
                maxStalenessMillis: Int64.max
            )
            // Cache the signature-verified numbers + record the successful check,
            // so a relaunch inside the 24h window drives the banner from the cache
            // without re-fetching.
            defaults.set(String(verified.latestRelease), forKey: Self.kCachedLatest)
            defaults.set(String(verified.minSupportedRelease), forKey: Self.kCachedMin)
            defaults.set(NSNumber(value: nowMillis), forKey: Self.kLastCheckAt)
        } catch {
            // Fail-safe: ANY failure resolves to up-to-date. Keep the last
            // signature-verified cache (an authentic prior verdict is not a spoof)
            // and do NOT bump the throttle, so a transient outage retries next
            // launch rather than pinning silence for 24h.
            NSLog("[release] manifest fetch/verify failed — treating as up to date: \(error)")
        }
    }

    private func recompute(nowMillis: Int64) {
        guard
            let latestStr = defaults.string(forKey: Self.kCachedLatest),
            let latest = UInt64(latestStr),
            let minStr = defaults.string(forKey: Self.kCachedMin),
            let minSupported = UInt64(minStr)
        else {
            // Never verified a manifest → nothing authenticated to show.
            offeredLatest = 0
            decision = .hidden
            return
        }
        let status = releaseUpdateStatus(
            currentRelease: ReleasePin.currentRelease,
            latestRelease: latest,
            minSupportedRelease: minSupported
        )
        offeredLatest = latest
        let inputs = UpdateBannerInputs(
            status: status,
            latestRelease: latest,
            storeUrlPresent: ReleasePin.appStoreUrl != nil,
            devOverride: ReleasePin.devOverride,
            dismissedThisSession: dismissedThisSession,
            dismissedRelease: persistedDismissedRelease(),
            dismissedAtMillis: persistedDismissedAt(),
            nowMillis: nowMillis,
            reNagAfterMillis: Self.reNagAfterMillis
        )
        decision = updateBannerDecision(inputs)
    }

    private func persistedDismissedRelease() -> UInt64? {
        guard let s = defaults.string(forKey: Self.kDismissedRelease) else { return nil }
        return UInt64(s)
    }

    private func persistedDismissedAt() -> Int64? {
        (defaults.object(forKey: Self.kDismissedAt) as? NSNumber)?.int64Value
    }

    private static func millis(_ date: Date) -> Int64 {
        Int64(date.timeIntervalSince1970 * 1000)
    }
}
