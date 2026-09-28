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
        /// ADR-0056 — the raw 16-byte account id the redeemed grant belongs
        /// to, from the redeem response; `nil` when the redeem failed. A
        /// fresh device that named its account by username first learns the
        /// id here.
        public let accountId: Data?

        public init(
            phraseSeed: Data,
            sessionToken: String?,
            sessionExpiresAtMs: Int64?,
            accountId: Data? = nil
        ) {
            self.phraseSeed = phraseSeed
            self.sessionToken = sessionToken
            self.sessionExpiresAtMs = sessionExpiresAtMs
            self.accountId = accountId
        }
    }

    /// Which account a joining device is joining (ADR-0056).
    public enum JoinTarget {
        /// A signed-in device (or one parked on the identity prompt after a
        /// password sign-in): the account id is already known, and the
        /// Welcome is checked against it at join.
        case account(accountIdBytes: Data, accountIdB64: String)
        /// A fresh device with no session that named its account by the
        /// username typed: nothing before the session carries the account
        /// id. The device learns it from the grant redeem after the join,
        /// and only then checks the joined group against it.
        case byName
    }

    /// Why a join by name could not finish (ADR-0056).
    public enum JoinByNameError: Error, LocalizedError, Equatable {
        /// The other device linked this one but sent no recovery row, or its
        /// grant could not be redeemed: without the redeem the device does
        /// not know its account.
        case noAccess
        /// The group joined is not the DeviceGroup of the account the grant
        /// belongs to.
        case wrongAccount

        public var errorDescription: String? {
            switch self {
            case .noAccess:
                return "The other device linked this one but the sign-in could not be completed. Try again."
            case .wrongAccount:
                return "The other device belongs to a different account. Try again with the right username."
            }
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
        fileprivate let target: JoinTarget
        /// ADR-0056 — the device-identity seed of a join by name, held in
        /// memory until the redeem names the account it is persisted under.
        /// Empty for ``JoinTarget/account(accountIdBytes:accountIdB64:)``,
        /// whose seed is already in the Keychain.
        fileprivate var pendingDeviceSeed: Data
        fileprivate let redeem: (String) async throws -> DeviceAuthorizeRedeemResult

        fileprivate init(
            qrPayload: Data,
            rendezvous: Data,
            identity: Identity,
            target: JoinTarget,
            pendingDeviceSeed: Data,
            redeem: @escaping (String) async throws -> DeviceAuthorizeRedeemResult
        ) {
            self.qrPayload = qrPayload
            self.rendezvous = rendezvous
            self.identity = identity
            self.target = target
            self.pendingDeviceSeed = pendingDeviceSeed
            self.redeem = redeem
        }

        deinit {
            pendingDeviceSeed.resetBytes(in: 0..<pendingDeviceSeed.count)
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
            switch target {
            case .account(let accountIdBytes, let accountIdB64):
                return try await acceptForAccount(
                    welcomeBytes: welcomeBytes,
                    welcomeCursor: welcomeCursor,
                    recovery: recovery,
                    accountIdBytes: accountIdBytes,
                    accountIdB64: accountIdB64
                )
            case .byName:
                guard let cursor = welcomeCursor else {
                    throw JoinByNameError.noAccess
                }
                return try await acceptByName(
                    welcomeBytes: welcomeBytes,
                    welcomeCursor: cursor
                )
            }
        }

        private func acceptForAccount(
            welcomeBytes: Data,
            welcomeCursor: Int64?,
            recovery: Bool,
            accountIdBytes: Data,
            accountIdB64: String
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
            if let cursor = welcomeCursor {
                await receiveBundles(joined: joined, accountIdB64: accountIdB64, welcomeCursor: cursor)
            }

            // ADR-0018 factor (a) — opt-in recovery payload.
            if recovery, let cursor = welcomeCursor {
                guard let outcome = await runJoinerRecovery(
                    exportSecret: { label, context, length in
                        try joined.exportSecret(
                            identity: self.identity, label: label, context: context, length: length
                        )
                    },
                    welcomeCursor: cursor
                ) else { return nil }
                // The grant belongs to the account whose DeviceGroup this
                // device just joined; a redeem naming another account keeps
                // the identity (verified against the published bundle later)
                // but not the session.
                if let redeemed = outcome.accountId, redeemed != accountIdBytes {
                    NSLog("device grant redeem named another account (seed-only outcome)")
                    return JoinRecoveryOutcome(
                        phraseSeed: outcome.phraseSeed,
                        sessionToken: nil,
                        sessionExpiresAtMs: nil,
                        accountId: nil
                    )
                }
                return outcome
            }
            return nil
        }

        /// ADR-0056 — the join of a fresh device that named its account by
        /// username. The account id is unknown until the grant redeem, so
        /// the Welcome is joined as a plain group, the recovery row is
        /// opened under its exporter (the DeviceGroup exporter IS the
        /// underlying group's), the grant is redeemed, and only then is the
        /// joined group checked to be that account's DeviceGroup. A redeem
        /// that fails or a group of another account ends the join: without
        /// the account the device has nothing to land.
        private func acceptByName(
            welcomeBytes: Data,
            welcomeCursor: Int64
        ) async throws -> JoinRecoveryOutcome {
            let plain = try PairingOrchestrator.joinWelcomeByName(
                welcomeBytes: welcomeBytes,
                identity: identity
            )
            guard let outcome = await runJoinerRecovery(
                exportSecret: { label, context, length in
                    try plain.exportSecret(
                        identity: self.identity, label: label, context: context, length: length
                    )
                },
                welcomeCursor: welcomeCursor
            ) else {
                throw JoinByNameError.noAccess
            }
            guard let accountIdBytes = outcome.accountId, outcome.sessionToken != nil else {
                var seed = outcome.phraseSeed
                seed.resetBytes(in: 0..<seed.count)
                throw JoinByNameError.noAccess
            }
            let joined: DeviceGroup
            do {
                joined = try PairingOrchestrator.adoptJoinedDeviceGroup(
                    identity: identity,
                    accountIdBytes: accountIdBytes
                )
            } catch {
                var seed = outcome.phraseSeed
                seed.resetBytes(in: 0..<seed.count)
                throw error
            }
            let accountIdB64 = base64Encode(accountIdBytes)
            // The device-identity seed is keyed by account; it is persisted
            // only now, once the redeem named the account.
            if !pendingDeviceSeed.isEmpty {
                do {
                    try DeviceIdentityStore.putDeviceIdentitySeed(
                        accountIdB64: accountIdB64,
                        seed: pendingDeviceSeed
                    )
                } catch {
                    NSLog("device identity seed persist (join by name) failed: %@", error.localizedDescription)
                }
                pendingDeviceSeed.resetBytes(in: 0..<pendingDeviceSeed.count)
                pendingDeviceSeed = Data()
            }
            DeviceGroupLifecycle.persistSnapshot(
                identity: identity,
                accountIdB64: accountIdB64
            )
            await receiveBundles(joined: joined, accountIdB64: accountIdB64, welcomeCursor: welcomeCursor)
            return outcome
        }

        /// Phase 2a3d8-ui history catch-up bundle + ADR-0050 section 2.9
        /// state bundle, both best-effort after the join.
        private func receiveBundles(
            joined: DeviceGroup,
            accountIdB64: String,
            welcomeCursor cursor: Int64
        ) async {
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

            if let bundle = await StateBundle.receive(
                group: joined,
                identity: identity,
                rendezvousPseudonym: rendezvous,
                afterCursor: cursor
            ) {
                let applied = StateBundle.apply(bundle)
                NSLog("state bundle: %d contacts, %d pins", applied.contacts, applied.pins)
            }
        }

        /// Poll for the scanner's sealed recovery row, open it under
        /// the shared post-join exporter key, redeem the grant.
        /// Every failure degrades (row absent → `nil`; redeem
        /// failure → seed-only outcome) — the join is never broken
        /// by this path. Transient secrets are zeroized
        /// (best-effort under Swift `Data` value semantics).
        private func runJoinerRecovery(
            exportSecret: (String, Data, UInt32) throws -> Data,
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
                exporterKey = try exportSecret(
                    RecoveryPayload.recoveryExporterLabel,
                    rendezvous,
                    UInt32(RecoveryPayload.recoveryExporterKeyLength)
                )
                payload = try openWithKey(key: exporterKey, wire: sealedBlob)
                let parsed = try RecoveryPayload.parseRecoveryPayload(payload)
                grantToken = parsed.grantToken
                // Redeem the single-use grant for a session token. The
                // grant alone names the account (ADR-0056); the response
                // carries the account id. A redeem failure (endpoint down,
                // grant expired) degrades to a seed-only outcome.
                var redeemed: DeviceAuthorizeRedeemResult?
                do {
                    redeemed = try await redeem(base64UrlEncodeNoPad(parsed.grantToken))
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
                    sessionExpiresAtMs: redeemed?.sessionExpiresAtMs,
                    accountId: redeemed?.accountId
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

    /// Joining-device side entry point for a device that knows its
    /// account (signed in, or parked on the identity prompt). Builds the
    /// DeviceIdentity from the cached seed, generates a fresh rendezvous
    /// slot, produces a KeyPackage, encodes the pairing QR naming the
    /// account by `loginHandle` (ADR-0056). The view drives the rest via
    /// the returned [`JoinSession`].
    public static func prepareJoinFlow(
        loginHandle: Data,
        accountIdBytes: Data,
        accountIdB64: String,
        redeem: @escaping (String) async throws -> DeviceAuthorizeRedeemResult = {
            try await AuthClient().deviceAuthorizeRedeem(grantTokenB64: $0)
        }
    ) throws -> JoinSession {
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(
            accountIdB64: accountIdB64
        )
        let identity = try Identity.fromSeed(seed: seed)
        let keyPackage = try identity.createKeyPackage()
        let rendezvous = PairingCodec.generateRendezvousPseudonym()
        let qrPayload = try PairingCodec.buildPairingQrPayload(
            loginHandle: loginHandle,
            keyPackage: keyPackage,
            rendezvousPseudonym: rendezvous
        )
        return JoinSession(
            qrPayload: qrPayload,
            rendezvous: rendezvous,
            identity: identity,
            target: .account(accountIdBytes: accountIdBytes, accountIdB64: accountIdB64),
            pendingDeviceSeed: Data(),
            redeem: redeem
        )
    }

    /// ADR-0056 — joining-device side entry point for a fresh device that
    /// names its account by the typed username. No lookup and no account id:
    /// a fresh device-identity seed is held in memory, the QR carries the
    /// login handle, and ``JoinSession/accept(welcomeBytes:welcomeCursor:recovery:)``
    /// learns the account from the grant redeem.
    public static func prepareRestoreByNameFlow(
        loginHandle: Data,
        redeem: @escaping (String) async throws -> DeviceAuthorizeRedeemResult = {
            try await AuthClient().deviceAuthorizeRedeem(grantTokenB64: $0)
        }
    ) throws -> JoinSession {
        var seed = Data(count: DeviceIdentityStore.seedLength)
        let status = seed.withUnsafeMutableBytes { ptr -> OSStatus in
            guard let base = ptr.baseAddress else { return errSecAllocate }
            return SecRandomCopyBytes(kSecRandomDefault, DeviceIdentityStore.seedLength, base)
        }
        guard status == errSecSuccess else {
            throw DeviceIdentityStoreError.keychain(status: status, operation: "SecRandomCopyBytes")
        }
        let identity = try Identity.fromSeed(seed: seed)
        let keyPackage = try identity.createKeyPackage()
        let rendezvous = PairingCodec.generateRendezvousPseudonym()
        let qrPayload = try PairingCodec.buildPairingQrPayload(
            loginHandle: loginHandle,
            keyPackage: keyPackage,
            rendezvousPseudonym: rendezvous
        )
        return JoinSession(
            qrPayload: qrPayload,
            rendezvous: rendezvous,
            identity: identity,
            target: .byName,
            pendingDeviceSeed: seed,
            redeem: redeem
        )
    }

    /// ADR-0056 — join a DeviceGroup Welcome before the account is known:
    /// a plain group join. The group_id check waits for
    /// ``adoptJoinedDeviceGroup(identity:accountIdBytes:)``.
    static func joinWelcomeByName(welcomeBytes: Data, identity: Identity) throws -> Group {
        try Group.joinFromWelcome(welcome: welcomeBytes, identity: identity)
    }

    /// ADR-0056 — the deferred group_id check of a join by name: load the
    /// DeviceGroup of `accountIdBytes` from the identity's storage. It is
    /// there only if the Welcome joined was that account's DeviceGroup
    /// (its group_id is derived from the account id), so a group of any
    /// other account is refused here, as `DeviceGroup.joinFromWelcome`
    /// refuses it for a device that knew its account up front.
    static func adoptJoinedDeviceGroup(identity: Identity, accountIdBytes: Data) throws -> DeviceGroup {
        guard let group = try loadDeviceGroup(identity: identity, accountId: accountIdBytes) else {
            throw JoinByNameError.wrongAccount
        }
        return group
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
    ///   * The QR names a different account's username (ADR-0056:
    ///     `FfiError.Group` "login_handle mismatch").
    ///   * The QR carries a newer-version payload than this
    ///     binary recognises.
    ///   * The Welcome PUT fails (DS-mailbox unreachable).
    ///   * The DeviceGroup state on this device is corrupt.
    public static func runScanFlow(
        scannedBytes: Data,
        ownLoginHandle: Data,
        accountIdBytes: Data,
        accountIdB64: String,
        recovery: ScanRecovery? = nil
    ) async throws -> ScanResult {
        // Wipe the seed copy on every exit path — including a
        // thrown pairing error before the recovery block runs.
        defer { recovery?.wipeSeed() }

        // ADR-0056 — the payload names the account by the login handle of
        // the username typed on the new device; it must be this account's
        // own handle, else nothing is committed.
        let decoded = try PairingCodec.parsePairingQrPayload(
            wireBytes: scannedBytes,
            expectedLoginHandle: ownLoginHandle
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
