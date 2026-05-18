// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// High-level orchestration for the two add-device sides on iOS
/// (Phase 2a3b3, mirror of
/// `apps/web/src/devices/orchestrator.ts`).
///
/// Wraps the per-call sequence (DeviceIdentity build, pairing
/// codec, DeviceGroup operations, DS-mailbox round-trip,
/// snapshot persistence) into two flat call shapes the SwiftUI
/// view invokes:
///
///   * [`prepareJoinFlow`] — joining-device side. Long-lived
///     session object; the view renders the QR from
///     `qrPayload` and drives `awaitWelcome` →
///     `accept(welcomeBytes)`.
///
///   * [`runScanFlow`] — scanner-device side. One-shot async
///     function: decode the scanned QR, add the new device's
///     leaf, publish the Welcome, persist the snapshot.
public enum PairingOrchestrator {

    /// Joining-side session handle. The SwiftUI view:
    ///
    ///   1. Reads `qrPayload`, feeds it into the
    ///      `CIFilter.qrCodeGenerator` to render the QR.
    ///   2. Calls `awaitWelcome()` to long-poll the rendezvous
    ///      slot; on `.received(welcomeBytes)` calls
    ///      `accept(welcomeBytes:)` and surfaces success.
    ///   3. On cancel / timeout / error releases the handle —
    ///      the embedded `Identity` + `DeviceGroup` are
    ///      released as the session goes out of scope.
    public final class JoinSession {
        /// Wire-format pairing QR bytes for the rendering
        /// layer.
        public let qrPayload: Data
        /// 32-byte rendezvous pseudonym surfaced for debug.
        public let rendezvous: Data

        fileprivate let identity: Identity
        fileprivate let accountIdBytes: Data
        fileprivate let accountIdB64: String

        fileprivate init(
            qrPayload: Data,
            rendezvous: Data,
            identity: Identity,
            accountIdBytes: Data,
            accountIdB64: String
        ) {
            self.qrPayload = qrPayload
            self.rendezvous = rendezvous
            self.identity = identity
            self.accountIdBytes = accountIdBytes
            self.accountIdB64 = accountIdB64
        }

        /// Long-poll the rendezvous mailbox.
        public func awaitWelcome(
            timeoutSeconds: TimeInterval = WelcomeCourier.defaultTimeoutSeconds
        ) async throws -> WelcomeCourier.WelcomeOutcome {
            return try await WelcomeCourier.pollForWelcome(
                rendezvousPseudonym: rendezvous,
                timeoutSeconds: timeoutSeconds
            )
        }

        /// Drive the terminal joining-side step: join the
        /// DeviceGroup, persist the resulting snapshot.
        public func accept(welcomeBytes: Data) throws {
            // The deterministic group_id check inside
            // DeviceGroup.joinFromWelcome rejects a misrouted
            // Welcome before any state advance.
            _ = try DeviceGroup.joinFromWelcome(
                welcome: welcomeBytes,
                identity: identity,
                accountId: accountIdBytes
            )
            DeviceGroupLifecycle.persistSnapshot(
                identity: identity,
                accountIdB64: accountIdB64
            )
        }
    }

    /// Joining-device side entry point. Builds the DeviceIdentity
    /// from the cached seed, generates a fresh rendezvous slot,
    /// produces a KeyPackage, encodes the pairing QR. The view
    /// drives the rest via the returned [`JoinSession`].
    public static func prepareJoinFlow(
        accountIdBytes: Data,
        accountIdB64: String
    ) throws -> JoinSession {
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(
            accountIdB64: accountIdB64
        )
        let identity = try Identity.fromSeed(seed: seed)
        let keyPackage = try identity.createKeyPackage()
        let rendezvous = PairingCodec.generateRendezvousPseudonym()
        let qrPayload = try PairingCodec.buildPairingQrPayload(
            accountId: accountIdBytes,
            keyPackage: keyPackage,
            rendezvousPseudonym: rendezvous
        )
        return JoinSession(
            qrPayload: qrPayload,
            rendezvous: rendezvous,
            identity: identity,
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64
        )
    }

    /// Result of [`runScanFlow`].
    public struct ScanResult {
        /// Number of devices in the local DeviceGroup AFTER
        /// the add. Best-effort estimate today — full
        /// `devices()` accessor lands alongside 2a3c.
        public let memberCount: Int
    }

    /// Scanner-device side entry point. One-shot: decode the
    /// QR, run `addDeviceFromWire`, publish the Welcome,
    /// persist the snapshot.
    ///
    /// Throws the inner error verbatim when:
    ///   * The QR was scanned against a different account.
    ///   * The QR carries a newer-version payload than this
    ///     binary recognises.
    ///   * The Welcome PUT fails (DS-mailbox unreachable).
    ///   * The DeviceGroup state on this device is corrupt.
    public static func runScanFlow(
        scannedBytes: Data,
        accountIdBytes: Data,
        accountIdB64: String
    ) async throws -> ScanResult {
        let decoded = try PairingCodec.parsePairingQrPayload(
            wireBytes: scannedBytes,
            expectedAccountId: accountIdBytes
        )
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(
            accountIdB64: accountIdB64
        )
        let ensured = try DeviceGroupLifecycle.ensureDeviceGroup(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            deviceIdentitySeed: seed
        )

        let addition = try ensured.group.addDeviceFromWire(
            identity: ensured.identity,
            keyPackage: decoded.keyPackage
        )
        try await WelcomeCourier.publishWelcome(
            rendezvousPseudonym: decoded.rendezvousPseudonym,
            welcomeBytes: addition.welcome
        )
        // TODO(2a3b3-followup): once the DS group journal is
        // wired for DeviceGroup commits, broadcast
        // `addition.commit` here too so any other
        // already-paired devices on this account advance to
        // the new epoch. For the founder + first-pair case
        // the commit is a no-op (the founder is the only
        // existing member, and the merge-pending-commit
        // already advanced its local view inside
        // addDeviceFromWire).
        DeviceGroupLifecycle.persistSnapshot(
            identity: ensured.identity,
            accountIdB64: accountIdB64
        )
        return ScanResult(memberCount: ensured.bootstrapped ? 2 : 0)
    }
}
