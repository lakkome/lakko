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
        ///
        /// Optional `welcomeCursor` (Phase 2a3d8-ui) opts in
        /// to the history catch-up bundle path: after the
        /// MLS join lands, the method polls the rendezvous
        /// mailbox past the Welcome cursor for the
        /// scanner-side bundle, opens it with the post-join
        /// exporter secret, and applies the rows to the local
        /// message log. Failures along that chain log + are
        /// swallowed — the join itself still succeeds.
        public func accept(
            welcomeBytes: Data,
            welcomeCursor: Int64? = nil
        ) async throws {
            // The deterministic group_id check inside
            // DeviceGroup.joinFromWelcome rejects a misrouted
            // Welcome before any state advance.
            let joined = try DeviceGroup.joinFromWelcome(
                welcome: welcomeBytes,
                identity: identity,
                accountId: accountIdBytes
            )
            DeviceGroupLifecycle.persistSnapshot(
                identity: identity,
                accountIdB64: accountIdB64
            )

            // Phase 2a3d8-ui — history catch-up bundle.
            if let cursor = welcomeCursor {
                do {
                    let sealed = try await WelcomeCourier.pollForHistoryBundle(
                        rendezvousPseudonym: rendezvous,
                        welcomeCursor: cursor
                    )
                    if case .received(let sealedBytes) = sealed {
                        let secret = try joined.historyBundleSecret(
                            identity: identity
                        )
                        let applied = try HistoryBundleCodec.applyToMessageLog(
                            accountIdB64: accountIdB64,
                            secret: secret,
                            sealedBytes: sealedBytes
                        )
                        NSLog("history catch-up: applied %d rows", applied)
                    }
                } catch {
                    NSLog("history bundle apply failed: %@", error.localizedDescription)
                }
            }
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

        // Phase 2a3d8-ui — best-effort history catch-up bundle.
        // Read the local message log, derive the post-add
        // exporter secret, seal the bundle, publish as the
        // *second* mailbox row. Any failure here is non-fatal —
        // the joining device still receives the Welcome and
        // ratchets forward.
        do {
            let secret = try ensured.group.historyBundleSecret(
                identity: ensured.identity
            )
            if let sealed = try HistoryBundleCodec.buildFromMessageLog(
                accountIdB64: accountIdB64,
                secret: secret
            ) {
                try await WelcomeCourier.publishHistoryBundle(
                    rendezvousPseudonym: decoded.rendezvousPseudonym,
                    sealedBytes: sealed
                )
            }
        } catch {
            NSLog("history bundle publish failed: %@", error.localizedDescription)
        }

        DeviceGroupLifecycle.persistSnapshot(
            identity: ensured.identity,
            accountIdB64: accountIdB64
        )
        return ScanResult(memberCount: ensured.bootstrapped ? 2 : 0)
    }
}
