// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Process-wide gate for calling availability based on the
/// boot-time SFrame selector probe (G3-sframe-native N-4).
///
/// Mirror of `SFrameAvailability` on Android (Kotlin object).
/// The probe runs once during `LakkoApp.init`; its result —
/// `setFrameEncryptor:` + `setFrameDecryptor:` present on the
/// loaded WebRTC framework's `RTCRtpSender` / `RTCRtpReceiver`
/// or not — becomes a hard precondition on every call-start
/// surface.
///
/// When the probe returns NO (the framework was shipped without
/// the non-public selectors, or a milestone bump dropped them),
/// the call UI MUST disable every call entry point and surface a
/// "calling unavailable on this build" banner. There is no
/// silent fallback to non-E2EE media — security invariant #4 in
/// `CLAUDE.md` forbids it.
@MainActor
public final class SFrameAvailability: ObservableObject {
    public static let shared = SFrameAvailability()

    /// `true` once `runBootProbe` has run and resolved one way or
    /// the other. Stays `false` between process start and that
    /// first call; the call UI's "Start call" affordance gates on
    /// `isCallingAvailable`, which is computed only when this is
    /// `true`.
    @Published public private(set) var didProbe: Bool = false

    /// `nil` until `runBootProbe` resolves. Reading before the
    /// probe completes is a bug — gate on `didProbe` first or use
    /// `isCallingAvailable` (which returns `false` for the
    /// not-yet-probed case, the conservative fail-closed default).
    @Published public private(set) var probeResult: Bool? = nil

    /// `true` only when the probe ran AND it returned positive.
    /// Fail-closed: a not-yet-probed state reports `false`. The
    /// SwiftUI view layer can bind this property to disable the
    /// call buttons.
    public var isCallingAvailable: Bool {
        probeResult == true
    }

    private init() {}

    /// One-shot boot probe. Idempotent — repeated calls re-use the
    /// cached result inside `LakkoSFrameAttach.bootProbe()`. Logs
    /// a structured warning on failure so the audit pack can grep
    /// for the failure prefix `[lakko.sframe]` in TestFlight logs.
    public func runBootProbe() {
        let ok = LakkoSFrameAttach.bootProbe()
        self.probeResult = ok
        self.didProbe = true
        if !ok {
            // `print` is allowed at the iOS app boundary; the
            // CLAUDE.md "no println / eprintln" rule lives on the
            // Rust crates. Swift goes through OSLog via Foundation
            // by default; this print appears in unified-logging
            // under the app's subsystem.
            print("[lakko.sframe] N-4 selector probe returned NO — " +
                  "calls disabled per security invariant #4.")
        }
    }
}
