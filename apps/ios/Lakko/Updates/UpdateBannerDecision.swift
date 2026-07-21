// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// The pure "should the update banner show, and with what controls" policy
/// (ADR-0038 S4; UX spec §5 store-button gate + §6 dismiss/re-nag cadence).
///
/// This holds ONLY the decision — the IO that resolves the facts (fetch + verify
/// + status + persisted dismiss state) lives in ``ReleaseUpdateChecker``. Keeping
/// it a total function over a plain value makes the whole cadence unit-testable
/// without a network, `UserDefaults`, or the UniFFI verify.

/// What the banner should render, post-signature-verify.
public enum UpdateBannerDecision: Equatable {
    /// No banner.
    case hidden
    /// Show the banner. `showUpdateButton` gates the primary Update control — it
    /// is hidden when no store URL is configured, leaving a purely informational
    /// text notice (never a dead / greyed-out button, UX §5).
    case shown(showUpdateButton: Bool)
}

/// The facts a banner decision is computed from. `status`, `latestRelease`,
/// `storeUrlPresent`, `devOverride`, `currentRelease` come from the verified
/// manifest + build config; `dismissed*` come from the per-platform prefs store;
/// `nowMillis` + `reNagAfterMillis` drive the cold-launch cadence.
public struct UpdateBannerInputs {
    /// The signature-verified update status (fail-safe: an unverified / unfetched
    /// manifest resolves to `.upToDate` upstream, so this is only ever the
    /// authenticated verdict).
    public var status: UpdateStatus
    /// The offered `latest_release` — the key the dismissal is stored against.
    /// Meaningful only when `status != .upToDate`.
    public var latestRelease: UInt64
    /// A store deep-link URL is configured (`LakkoAppStoreUrl` non-empty).
    public var storeUrlPresent: Bool
    /// The dev/internal override forcing the banner without a store URL.
    public var devOverride: Bool
    /// The banner was already dismissed earlier in THIS process run.
    public var dismissedThisSession: Bool
    /// The `latest_release` the user last dismissed (persisted); `nil` if never.
    public var dismissedRelease: UInt64?
    /// When that dismiss happened, Unix millis (persisted); `nil` if never.
    public var dismissedAtMillis: Int64?
    /// Now, Unix millis.
    public var nowMillis: Int64
    /// The cold-launch re-nag window in millis (24h) for the actionable banner.
    public var reNagAfterMillis: Int64

    public init(
        status: UpdateStatus,
        latestRelease: UInt64,
        storeUrlPresent: Bool,
        devOverride: Bool,
        dismissedThisSession: Bool = false,
        dismissedRelease: UInt64? = nil,
        dismissedAtMillis: Int64? = nil,
        nowMillis: Int64,
        reNagAfterMillis: Int64
    ) {
        self.status = status
        self.latestRelease = latestRelease
        self.storeUrlPresent = storeUrlPresent
        self.devOverride = devOverride
        self.dismissedThisSession = dismissedThisSession
        self.dismissedRelease = dismissedRelease
        self.dismissedAtMillis = dismissedAtMillis
        self.nowMillis = nowMillis
        self.reNagAfterMillis = reNagAfterMillis
    }
}

/// The pure banner-visibility classifier. Total over ``UpdateBannerInputs``; no
/// IO. Branch order is load-bearing:
///
///   1. `.upToDate` → never a banner (UX §2 — no positive-confirmation surface).
///   2. Actionability gate — dormant unless a store URL exists to act on OR a dev
///      override is set (UX §5 optional hardening; the S4 gate). Avoids a nag
///      with no payoff and a dead Update control.
///   3. Dismissed this run → gone for the rest of the session (UX §6).
///   4. Cold-launch re-nag cadence keyed to the dismissed target (UX §6):
///      * a strictly-newer `latest_release` re-surfaces at once, bypassing both
///        the time window and the prior dismissal (the newer-release rule);
///      * else an actionable banner re-nags at most once per `reNagAfterMillis`
///        (≈ once/day); a text-only (dev-override) banner does NOT time-re-nag —
///        it shows once per NEW version only, since the user cannot act on it.
///
/// `.updateAvailable` and `.updateRequired` render identically in v1 (UX §2 /
/// ADR-0038 §5 — no blocking screen yet; the `min_supported` field is carried so
/// a future gate needs no wire change).
public func updateBannerDecision(_ i: UpdateBannerInputs) -> UpdateBannerDecision {
    switch i.status {
    case .upToDate:
        return .hidden
    case .updateAvailable, .updateRequired:
        break
    }

    // Actionability gate — no store URL and no override → dormant.
    guard i.storeUrlPresent || i.devOverride else {
        return .hidden
    }

    // Dismissed this run → gone until a later cold launch.
    if i.dismissedThisSession {
        return .hidden
    }

    if let dismissed = i.dismissedRelease {
        // A strictly-newer release is a fresh target — re-nag immediately.
        if i.latestRelease > dismissed {
            return .shown(showUpdateButton: i.storeUrlPresent)
        }
        // Same-or-older target already dismissed.
        if i.storeUrlPresent {
            // Actionable: re-nag at most once per window.
            let elapsed = i.nowMillis - (i.dismissedAtMillis ?? 0)
            guard elapsed >= i.reNagAfterMillis else {
                return .hidden
            }
        } else {
            // Text-only (dev override): once per new version only, no time re-nag.
            return .hidden
        }
    }

    return .shown(showUpdateButton: i.storeUrlPresent)
}
