// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0026 Phase 4 — the arming controller.
///
/// Owns the user "Anti-censorship" preference (persisted in `UserDefaults`) and,
/// when it is on, fetches + verifies the signed bootstrap config against the
/// build-time-pinned trust anchor and publishes the armed
/// ``AntiCensorshipConfig`` so every `DiscoveryClient` (and later every other
/// client) routes through the obfuscated tunnel.
///
/// Fail-closed per ADR-0026: a fetch / verify failure leaves the ambient config
/// **disarmed** and surfaces a loud ``State/unavailable(_:)`` — never a silent
/// armed state with an unverified config, and the failure is signalled (not
/// silent) so the UI can show it.
@MainActor
final class AntiCensorshipManager: ObservableObject {
    /// The arming lifecycle, surfaced to the settings UI.
    enum State: Equatable {
        /// The user preference is off — direct path, as today.
        case disabled
        /// The preference is on but this build pinned no bootstrap anchor, so
        /// it cannot arm.
        case unconfigured
        /// Fetching + verifying the signed config.
        case arming
        /// The tunnel is armed; clients route through it.
        case armed
        /// The preference is on and a pin is present, but the fetch / verify
        /// failed — loud, no silent direct fallback.
        case unavailable(String)
    }

    /// Process-wide controller. `LakkoApp` arms it at boot; the settings view
    /// observes it + toggles the preference.
    static let shared = AntiCensorshipManager()

    /// The current arming state.
    @Published private(set) var state: State

    private static let defaultsKey = "lakko.antiCensorship.enabled"

    /// Whether the user has turned anti-censorship on.
    var isEnabled: Bool {
        UserDefaults.standard.bool(forKey: Self.defaultsKey)
    }

    init() {
        state = UserDefaults.standard.bool(forKey: Self.defaultsKey) ? .arming : .disabled
    }

    /// Apply the persisted preference: arm (fetch + verify + publish) when on,
    /// otherwise reset the ambient config to disarmed. Safe to call repeatedly;
    /// `LakkoApp` calls it once at boot.
    func armFromSettings() {
        // ADR-0041 Finding-1 — mirror the preference into the shared App
        // Group suite so the `LakkoNotificationService` (a separate process
        // that cannot read the app's private `UserDefaults.standard`) can
        // refuse a direct DS connection on a push wake when anti-censorship
        // is armed. Written here on every toggle (via `setEnabled`) AND once
        // at boot (`LakkoApp` calls this), so the mirror always tracks the
        // source-of-truth preference regardless of the arming outcome below.
        SharedStorage.setAntiCensorshipArmed(isEnabled)

        guard isEnabled else {
            AntiCensorshipConfig.current = .disarmed
            state = .disabled
            return
        }
        guard
            let anchor = BootstrapPin.trustAnchor,
            let domain = BootstrapPin.dohDomain,
            let path = BootstrapPin.configPath
        else {
            // Preference on, but no anchor was pinned into this build — cannot
            // arm. Stay disarmed (loud), never an insecure default.
            AntiCensorshipConfig.current = .disarmed
            state = .unconfigured
            NSLog("[anti-censorship] enabled but no bootstrap pin in this build — staying disarmed")
            return
        }

        state = .arming
        let nowSecs = UInt64(Date().timeIntervalSince1970)
        Task.detached(priority: .userInitiated) {
            do {
                // Synchronous FFI (drives a private tokio runtime); run off the
                // main thread.
                let configJson = try fetchBootstrapConfig(
                    dohDomain: domain,
                    path: path,
                    trustAnchor: anchor,
                    nowSecs: nowSecs
                )
                await MainActor.run {
                    AntiCensorshipConfig.current = AntiCensorshipConfig(
                        armed: true,
                        bootstrapConfigJson: configJson,
                        trustAnchor: anchor
                    )
                    self.state = .armed
                    NSLog("[anti-censorship] armed — clients route through the obfuscated tunnel")
                }
            } catch {
                await MainActor.run {
                    // Fail-closed: disarm + surface the failure loudly.
                    AntiCensorshipConfig.current = .disarmed
                    self.state = .unavailable("\(error)")
                    NSLog("[anti-censorship] config fetch/verify failed — staying disarmed: \(error)")
                }
            }
        }
    }

    /// Persist the preference and (re-)apply it.
    func setEnabled(_ on: Bool) {
        UserDefaults.standard.set(on, forKey: Self.defaultsKey)
        armFromSettings()
    }
}
