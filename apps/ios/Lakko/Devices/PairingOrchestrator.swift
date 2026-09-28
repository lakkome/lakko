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

    /// ADR-0018 factor (a) — what the joiner recovered from the
    /// scanner's sealed rendezvous row. `phraseSeed` is the 64-byte
    /// AccountIdentity seed (IDENTITY — always present when this
    /// value is non-nil); the session token pair is ACCESS and is
    /// `nil` when the grant redeem failed (seed-only degradation).
    /// The consumer (`AuthRepository.completeFromPairing` /
    /// `completeFromDeviceRestore`) owns zeroizing `phraseSeed`
    /// (best-effort under Swift `Data` value semantics).
    public struct JoinRecoveryOutcome {
        public let phraseSeed: Data
        public let sessionToken: String?
        public let sessionExpiresAtMs: Int64?

        public init(
            phraseSeed: Data,
            sessionToken: String?,
            sessionExpiresAtMs: Int64?
        ) {
            self.phraseSeed = phraseSeed
            self.sessionToken = sessionToken
            self.sessionExpiresAtMs = sessionExpiresAtMs
        }
    }

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
        ///
        /// With `recovery = true` (ADR-0018 factor (a)) also polls
        /// past the Welcome for the scanner's sealed recovery row,
        /// opens it under the shared post-join exporter key, redeems
        /// the grant for a session token, and returns the typed
        /// outcome. Absence of the row (older scanner, or its
        /// best-effort half failed) returns `nil` — the join itself
        /// is unaffected and the caller falls back to phrase entry.
        @discardableResult
        public func accept(
            welcomeBytes: Data,
            welcomeCursor: Int64? = nil,
            recovery: Bool = false
        ) async throws -> JoinRecoveryOutcome? {
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

            // ADR-0050 section 2.9 — the state bundle (contacts, pins, known
            // group ids), best-effort like the history bundle.
            if let cursor = welcomeCursor,
               let bundle = await StateBundle.receive(
                   group: joined,
                   identity: identity,
                   rendezvousPseudonym: rendezvous,
                   afterCursor: cursor
               ) {
                let applied = StateBundle.apply(bundle)
                NSLog("state bundle: %d contacts, %d pins", applied.contacts, applied.pins)
            }

            // ADR-0018 factor (a) — opt-in recovery payload.
            if recovery, let cursor = welcomeCursor {
                return await runJoinerRecovery(
                    joined: joined,
                    welcomeCursor: cursor
                )
            }
            return nil
        }

        /// Poll for the scanner's sealed recovery row, open it under
        /// the shared post-join exporter key, redeem the grant.
        /// Every failure degrades (row absent → `nil`; redeem
        /// failure → seed-only outcome) — the join is never broken
        /// by this path. Transient secrets are zeroized
        /// (best-effort under Swift `Data` value semantics).
        private func runJoinerRecovery(
            joined: DeviceGroup,
            welcomeCursor: Int64
        ) async -> JoinRecoveryOutcome? {
            let outcome: RecoveryPayload.RecoveryPayloadOutcome
            do {
                outcome = try await RecoveryPayload.pollForRecoveryPayload(
                    rendezvousPseudonym: rendezvous,
                    afterCursor: welcomeCursor
                )
            } catch {
                NSLog(
                    "recovery payload poll failed (join unaffected): %@",
                    error.localizedDescription
                )
                return nil
            }
            guard case .received(let sealedBlob) = outcome else {
                return nil
            }
            var exporterKey = Data()
            var payload = Data()
            var grantToken = Data()
            defer {
                exporterKey.resetBytes(in: 0..<exporterKey.count)
                payload.resetBytes(in: 0..<payload.count)
                grantToken.resetBytes(in: 0..<grantToken.count)
            }
            do {
                exporterKey = try joined.exportSecret(
                    identity: identity,
                    label: RecoveryPayload.recoveryExporterLabel,
                    context: rendezvous,
                    length: UInt32(RecoveryPayload.recoveryExporterKeyLength)
                )
                payload = try openWithKey(key: exporterKey, wire: sealedBlob)
                let parsed = try RecoveryPayload.parseRecoveryPayload(payload)
                grantToken = parsed.grantToken
                // Redeem the single-use grant for a session token.
                // A redeem failure (endpoint down, grant expired)
                // degrades to a seed-only outcome — the identity
                // still restores; only ACCESS needs another factor.
                var redeemed: DeviceAuthorizeRedeemResult?
                do {
                    redeemed = try await AuthClient().deviceAuthorizeRedeem(
                        accountId: accountIdBytes,
                        grantTokenB64: base64UrlEncodeNoPad(parsed.grantToken)
                    )
                } catch {
                    NSLog(
                        "device grant redeem failed (seed-only outcome): %@",
                        error.localizedDescription
                    )
                    redeemed = nil
                }
                return JoinRecoveryOutcome(
                    phraseSeed: parsed.phraseSeed,
                    sessionToken: redeemed?.sessionToken,
                    sessionExpiresAtMs: redeemed?.sessionExpiresAtMs
                )
            } catch {
                NSLog(
                    "recovery payload open failed (join unaffected): %@",
                    error.localizedDescription
                )
                return nil
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
        /// ADR-0050 section 2.8 — the added device's 32-byte signature key:
        /// its DeviceGroup leaf, its conversation leaf and its discovery
        /// `device_id` are one key. The caller hands it to
        /// ``ChatRepository/admitDevice(deviceKey:)``.
        public let newDeviceKey: Data
    }

    /// ADR-0018 factor (a) — inputs for the best-effort recovery
    /// payload the scanner publishes alongside the Welcome, so the
    /// fresh device gets ACCESS (a single-use grant) + IDENTITY (the
    /// phrase seed) with no password or phrase entry.
    public final class ScanRecovery {
        /// Defensive copy of the 64-byte account-identity (phrase)
        /// seed; consumed and zeroized by the scan flow.
        fileprivate var accountIdentitySeed: Data
        /// Mints a single-use device grant
        /// (`AuthClient.deviceAuthorizeStart`, bearer);
        /// returns the URL-safe-base64 raw grant token.
        fileprivate let deviceAuthorizeStart: () async throws -> String

        public init(
            accountIdentitySeed: Data,
            deviceAuthorizeStart: @escaping () async throws -> String
        ) {
            // Defensive copy — the scan flow consumes + wipes it.
            self.accountIdentitySeed = Data(accountIdentitySeed)
            self.deviceAuthorizeStart = deviceAuthorizeStart
        }

        /// Best-effort wipe of the seed copy this object owns.
        fileprivate func wipeSeed() {
            accountIdentitySeed.resetBytes(in: 0..<accountIdentitySeed.count)
        }
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
        accountIdB64: String,
        recovery: ScanRecovery? = nil
    ) async throws -> ScanResult {
        // Wipe the seed copy on every exit path — including a
        // thrown pairing error before the recovery block runs.
        defer { recovery?.wipeSeed() }

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

        let groupId = try ensured.group.groupId()
        let ds = DsClient()
        // ADR-0053 point 2 — claim-on-open (idempotent; also the claim of a
        // DeviceGroup this flow just founded).
        await DeviceGroupDsAccess.claim(group: ensured.group, identity: ensured.identity, ds: ds)

        // ADR-0053 point 3 — the N+1 key is registered, proven under N, before
        // the snapshot is written and anything is published; the commit then
        // goes out signed under N.
        _ = try await DeviceGroupDsAccess.directCommit(
            group: ensured.group,
            identity: ensured.identity,
            ds: ds,
            create: {
                try ensured.group.addDeviceFromWire(
                    identity: ensured.identity,
                    keyPackage: decoded.keyPackage
                )
            },
            prepare: { addition in
                // G-7 — persist the advanced DeviceGroup snapshot BEFORE
                // any publish. addDeviceFromWire already merged the pending
                // commit, so the in-memory view is one epoch ahead; writing
                // it to disk first closes the window where a crash between
                // the publishes and the persist leaves the on-disk envelope
                // an epoch behind (which would make the next launch try to
                // add the same device twice).
                DeviceGroupLifecycle.persistSnapshot(
                    identity: ensured.identity,
                    accountIdB64: accountIdB64
                )
                return addition
            },
            publish: { addition, auth in
                // The Welcome rides the pairing RENDEZVOUS mailbox, which stays
                // unclaimed by design (ADR-0053 section 2): unsigned.
                try await WelcomeCourier.publishWelcome(
                    rendezvousPseudonym: decoded.rendezvousPseudonym,
                    welcomeBytes: addition.welcome
                )

                // G-5 — broadcast the Commit to the DeviceGroup DS journal
                // so any OTHER already-paired devices on this account
                // advance to the new epoch; their DeviceGroup poll loop runs
                // processIncoming on this row. MLS requires in-order epoch
                // processing, so this is on the critical path (not
                // best-effort) — a missed commit would permanently strand a
                // third device. For the founder + first-pair case there is
                // no other member, so the row is a harmless no-op (the
                // joiner ratchets forward from the Welcome). Mirrors the
                // DeviceRemoval.removeDeviceAt broadcast.
                // G-13 (ADR-0015): every DeviceGroup journal entry is wrapped; an
                // add-device commit carries no attestation.
                let envelope = DeviceGroupJournalEnvelope.encode(
                    message: addition.commit,
                    attestation: nil
                )
                _ = try await ds.publishGroup(groupId: groupId, payload: envelope, auth: auth)
                return addition
            }
        )

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

        // ADR-0018 factor (a) — best-effort recovery payload.
        // Mint a single-use device grant, derive the exporter key
        // at the POST-ADD epoch (both devices share it after the
        // joiner processes the Welcome), seal
        // (grant ‖ account-identity seed) and publish the tagged
        // rendezvous row. Any failure is non-fatal: the pairing
        // completes and the joiner falls back to phrase entry.
        // The server never sees the seed (it travels only inside
        // the AEAD blob — invariant #2); transient copies are
        // zeroized below (best-effort under Swift `Data` value
        // semantics).
        if let recovery {
            var grant = Data()
            var exporterKey = Data()
            var payload = Data()
            do {
                guard let grantBytes = base64UrlDecodeNoPad(
                    try await recovery.deviceAuthorizeStart()
                ) else {
                    throw RecoveryPayloadError.argument(
                        "grant token is not URL-safe base64"
                    )
                }
                grant = grantBytes
                exporterKey = try ensured.group.exportSecret(
                    identity: ensured.identity,
                    label: RecoveryPayload.recoveryExporterLabel,
                    context: decoded.rendezvousPseudonym,
                    length: UInt32(RecoveryPayload.recoveryExporterKeyLength)
                )
                payload = try RecoveryPayload.buildRecoveryPayload(
                    grantToken: grant,
                    phraseSeed: recovery.accountIdentitySeed
                )
                let sealed = try sealWithKey(key: exporterKey, plaintext: payload)
                try await RecoveryPayload.publishRecoveryPayload(
                    rendezvousPseudonym: decoded.rendezvousPseudonym,
                    sealedBlob: sealed
                )
            } catch {
                NSLog(
                    "recovery payload publish failed (pairing unaffected): %@",
                    error.localizedDescription
                )
            }
            grant.resetBytes(in: 0..<grant.count)
            exporterKey.resetBytes(in: 0..<exporterKey.count)
            payload.resetBytes(in: 0..<payload.count)
        }

        // ADR-0050 section 2.9 — the state bundle: contacts, pins and the ids
        // of every group this device holds, sealed under the per-pairing
        // exporter like the recovery row. Best-effort; a failure leaves the new
        // device with the chats it is admitted to and no contact names.
        do {
            try await StateBundle.publish(
                bundle: StateBundle.build(accountIdB64: accountIdB64),
                group: ensured.group,
                identity: ensured.identity,
                rendezvousPseudonym: decoded.rendezvousPseudonym
            )
        } catch {
            NSLog("state bundle publish failed: %@", error.localizedDescription)
        }

        return ScanResult(
            memberCount: ensured.bootstrapped ? 2 : 0,
            newDeviceKey: try keyPackageSignatureKey(keyPackage: decoded.keyPackage)
        )
    }
}
