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

    /// What the joiner recovered from the scanner's sealed rendezvous row
    /// (recovery payload v2). `phraseEntropy` is the 16-byte entropy of the
    /// recovery phrase (the keys — always present when this value is
    /// non-nil); the session token pair is access and is `nil` when the
    /// grant redeem failed (keys-only degradation). The consumer
    /// (`AuthRepository.completeFromPairing` / `completeFromDeviceRestore`,
    /// or the view model refusing it) owns zeroizing `phraseEntropy`
    /// through ``wipeEntropy()``.
    ///
    /// A class, not a struct, so the entropy lives in ONE mutable place:
    /// under `Data` copy-on-write, `var e = outcome.phraseEntropy;
    /// e.resetBytes(...)` zeroes a fresh copy and leaves the bytes that
    /// were received untouched. ``wipeEntropy()`` zeroes the outcome's own
    /// storage in place, which is the received buffer as long as nobody
    /// kept a copy of `phraseEntropy` (the refusal paths keep none).
    public final class JoinRecoveryOutcome {
        public private(set) var phraseEntropy: Data
        /// G-56 — the account's own predecessor seeds the recovery row v3
        /// carried, OLDEST FIRST (empty when the account never rotated).
        /// Secret; zeroed by ``wipeEntropy()``.
        public private(set) var predecessors: [FfiRecoveryPredecessor]
        /// The session pair the grant redeem issued (ADR-0073 section 3);
        /// `nil` when the redeem failed (keys-only outcome).
        public let session: SessionPair?
        /// ADR-0056 — the raw 16-byte account id the redeemed grant belongs
        /// to, from the redeem response; `nil` when the redeem failed. A
        /// fresh device that named its account by username first learns the
        /// id here.
        public let accountId: Data?
        /// ADR-0056 security pass — a join by name holds everything it
        /// received (the device-identity seed, the DeviceGroup snapshot,
        /// the history rows, the state bundle) in memory; this writes it.
        /// The restore calls it only after the seed of the phrase verified against
        /// the published identity AND the typed name resolved, at
        /// discovery, to the redeemed account. `nil` for a device that
        /// knew its account (it persisted at join, as before).
        public let persistJoin: (() async -> Void)?

        public init(
            phraseEntropy: Data,
            session: SessionPair?,
            accountId: Data? = nil,
            predecessors: [FfiRecoveryPredecessor] = [],
            persistJoin: (() async -> Void)? = nil
        ) {
            self.phraseEntropy = phraseEntropy
            self.predecessors = predecessors
            self.session = session
            self.accountId = accountId
            self.persistJoin = persistJoin
        }

        /// The access token of ``session``, or `nil`.
        public var sessionToken: String? { session?.sessionToken }

        /// Zero the entropy and the carried predecessor seeds in place.
        /// Idempotent.
        public func wipeEntropy() {
            phraseEntropy.withUnsafeMutableBytes { raw in
                guard let base = raw.baseAddress else { return }
                memset(base, 0, raw.count)
            }
            for index in predecessors.indices {
                predecessors[index].seed.withUnsafeMutableBytes { raw in
                    guard let base = raw.baseAddress else { return }
                    memset(base, 0, raw.count)
                }
            }
            predecessors = []
        }
    }

    /// Which account a joining device is joining (ADR-0056).
    public enum JoinTarget {
        /// A signed-in device (or one parked on the identity prompt after a
        /// password sign-in): the account id is already known. The Welcome
        /// is bound to the account by the recovery row sealed under the
        /// joined group's exporter (ADR-0078 section 6: the DeviceGroup id
        /// is derived from the account seed, which the device does not hold
        /// yet, so there is no id to check at join).
        case account(accountIdBytes: Data, accountIdB64: String)
        /// A fresh device with no session that named its account by the
        /// username typed: nothing before the session carries the account
        /// id. The device learns it from the grant redeem after the join.
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

        /// ADR-0061 section 6 — rendered by kind from the catalog, never as
        /// English prose (``AuthErrorCopy/deviceRestore(_:)`` reads it).
        public var errorDescription: String? {
            switch self {
            case .noAccess:
                return Localization.text("device_link.error.not_let_in")
            case .wrongAccount:
                return Localization.text("device_link.error.wrong_account")
            }
        }
    }

    /// The row the joining device took for its Welcome did not join (anyone
    /// who learned the rendezvous can post to it). The session waits past
    /// it on the next ``JoinSession/awaitWelcome(timeoutSeconds:)``; the
    /// screen goes back to waiting.
    public struct WelcomeNotOpened: Error, Equatable {}

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
        /// What `qrPayload` was built from, kept so the code route can build
        /// the same payload with a later card (UX section 2.1).
        fileprivate let loginHandle: Data
        fileprivate let keyPackage: Data
        /// ADR-0056 — the device-identity seed of a join by name, held in
        /// memory until the redeem names the account it is persisted under.
        /// Empty for ``JoinTarget/account(accountIdBytes:accountIdB64:)``,
        /// whose seed is already in the Keychain.
        fileprivate var pendingDeviceSeed: Data
        /// G-56 — the self-removal proof a re-pairing of a DeviceGroup member
        /// carries in its payload; empty for an ordinary join.
        fileprivate let selfRemovalProof: Data
        fileprivate let redeem: (String) async throws -> DeviceAuthorizeRedeemResult
        /// The last rendezvous row tried as the Welcome that did not join:
        /// the next wait reads past it.
        private var welcomeFloor: Int64 = 0

        fileprivate init(
            qrPayload: Data,
            rendezvous: Data,
            identity: Identity,
            target: JoinTarget,
            loginHandle: Data,
            keyPackage: Data,
            pendingDeviceSeed: Data,
            selfRemovalProof: Data = Data(),
            redeem: @escaping (String) async throws -> DeviceAuthorizeRedeemResult
        ) {
            self.qrPayload = qrPayload
            self.rendezvous = rendezvous
            self.identity = identity
            self.target = target
            self.loginHandle = loginHandle
            self.keyPackage = keyPackage
            self.pendingDeviceSeed = pendingDeviceSeed
            self.selfRemovalProof = selfRemovalProof
            self.redeem = redeem
        }

        deinit {
            pendingDeviceSeed.resetBytes(in: 0..<pendingDeviceSeed.count)
        }

        /// UX section 2.1 — the same payload as `qrPayload` (handle,
        /// KeyPackage, rendezvous) with `deviceCardEnvelope` as its card.
        /// The code route seals this when the code is typed, so a city that
        /// arrived after the screen opened is in it. Falls back to
        /// `qrPayload` when it cannot be built.
        public func payload(deviceCardEnvelope: Data?) -> Data {
            do {
                return try PairingCodec.buildPairingQrPayload(
                    loginHandle: loginHandle,
                    keyPackage: keyPackage,
                    rendezvousPseudonym: rendezvous,
                    deviceCardEnvelope: deviceCardEnvelope,
                    selfRemovalProof: selfRemovalProof
                )
            } catch {
                NSLog("[pairing] WARN payload not rebuilt with the card: \(error)")
                return qrPayload
            }
        }

        /// Long-poll the rendezvous mailbox.
        /// A row that did not join before (``WelcomeNotOpened``) is passed
        /// over: the wait reads past it.
        public func awaitWelcome(
            timeoutSeconds: TimeInterval = WelcomeCourier.defaultTimeoutSeconds
        ) async throws -> WelcomeCourier.WelcomeOutcome {
            return try await WelcomeCourier.pollForWelcome(
                rendezvousPseudonym: rendezvous,
                after: welcomeFloor,
                timeoutSeconds: timeoutSeconds
            )
        }

        /// The join of the Welcome row itself; a refusal there is the row
        /// not being this device's Welcome: remember it and say so.
        private func joinOrPassOver<T>(welcomeCursor: Int64?, _ join: () throws -> T) throws -> T {
            do {
                return try join()
            } catch {
                NSLog("[pairing] a rendezvous row did not join; waiting past it (%@)",
                      String(describing: type(of: error)))
                if let welcomeCursor { welcomeFloor = max(welcomeFloor, welcomeCursor) }
                throw WelcomeNotOpened()
            }
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
        /// With `recovery = true` also polls past the Welcome for the
        /// scanner's sealed recovery row, opens it under the shared
        /// post-join exporter key, redeems the grant for a session
        /// token, and returns the typed outcome. Absence of the row (the
        /// scanner holds no phrase, or its best-effort half failed)
        /// returns `nil` — the join itself is unaffected and the caller
        /// falls back to a passkey or phrase entry.
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
            // ADR-0078 section 6: the join binds no group id; the recovery
            // row opened under this group's exporter binds the account.
            let joined = try joinOrPassOver(welcomeCursor: welcomeCursor) {
                try DeviceGroup.joinFromWelcome(
                    welcome: welcomeBytes,
                    identity: identity,
                    accountId: accountIdBytes
                )
            }
            DeviceGroupLifecycle.clearFoundingMark(joined: joined)
            // ADR-0081 decision 3 — fail-closed: a joined DeviceGroup that is
            // not on disk is not reported as joined.
            try DeviceGroupLifecycle.persistSnapshotOrThrow(
                identity: identity,
                accountIdB64: accountIdB64
            )
            // G-56 — the DeviceGroup id this device joined, for a later
            // re-pairing that has to name it (the id is a function of the
            // account's earlier keys, which a parked device lacks).
            JoinedDeviceGroupIdStore().save(try joined.groupId(), accountIdB64: accountIdB64)
            // UX section 3 — joined: this device's own card goes at once.
            await MainActor.run { DeviceCardCenter.shared.noteJoined(accountIdB64: accountIdB64) }
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
                    NSLog("device grant redeem named another account (keys-only outcome)")
                    return JoinRecoveryOutcome(
                        phraseEntropy: outcome.phraseEntropy,
                        session: nil,
                        accountId: nil,
                        predecessors: outcome.predecessors
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
        /// underlying group's), the grant is redeemed, and the joined group
        /// is adopted as that account's DeviceGroup under its own id. A
        /// redeem that fails ends the join: without the account the device
        /// has nothing to land. A row whose phrase is not the redeemed
        /// account's is refused by the restore's check against the published
        /// identity, before anything is persisted.
        ///
        /// ADR-0056 security pass: NOTHING is written here. The device
        /// seed, the snapshot, the history rows and the state bundle are
        /// held in a ``PendingJoinByName`` whose ``PendingJoinByName/persist()``
        /// rides the outcome as ``JoinRecoveryOutcome/persistJoin``; the
        /// restore runs it only after the phrase seed verified against the
        /// published identity and the typed name resolved, at discovery, to
        /// the redeemed account. A refusal before that leaves this device
        /// exactly as it was.
        private func acceptByName(
            welcomeBytes: Data,
            welcomeCursor: Int64
        ) async throws -> JoinRecoveryOutcome {
            let plain = try joinOrPassOver(welcomeCursor: welcomeCursor) {
                try PairingOrchestrator.joinWelcomeByName(
                    welcomeBytes: welcomeBytes,
                    identity: identity
                )
            }
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
                outcome.wipeEntropy()
                throw JoinByNameError.noAccess
            }
            let joined: DeviceGroup
            do {
                joined = try PairingOrchestrator.adoptJoinedDeviceGroup(
                    identity: identity,
                    accountIdBytes: accountIdBytes,
                    groupId: try plain.groupId()
                )
            } catch {
                outcome.wipeEntropy()
                throw error
            }
            // The device-identity seed moves into the pending join, which
            // becomes its only owner (zeroed there on persist or on release).
            let pending = PendingJoinByName(
                identity: identity,
                joined: joined,
                accountIdBytes: accountIdBytes,
                deviceSeed: pendingDeviceSeed
            )
            pendingDeviceSeed = Data()
            // UX section 3 — joined: this device's own card goes as soon as
            // the account binds (after the restore landed it).
            let joinedAccount = pending.accountIdB64
            await MainActor.run { DeviceCardCenter.shared.noteJoined(accountIdB64: joinedAccount) }
            await fetchBundles(into: pending, welcomeCursor: welcomeCursor)
            return JoinRecoveryOutcome(
                phraseEntropy: outcome.phraseEntropy,
                session: outcome.session,
                accountId: accountIdBytes,
                predecessors: outcome.predecessors,
                persistJoin: { pending.persist() }
            )
        }

        /// Fetch (not apply) the history and state bundles of a join by
        /// name: both best-effort, both held by `pending` until the restore
        /// passed its checks.
        private func fetchBundles(into pending: PendingJoinByName, welcomeCursor cursor: Int64) async {
            do {
                let secret = try pending.joined.historyBundleSecret(identity: identity)
                let sealed = try await WelcomeCourier.pollForHistoryBundle(
                    rendezvousPseudonym: rendezvous,
                    welcomeCursor: cursor,
                    opens: { HistoryBundleCodec.opens(secret: secret, sealedBytes: $0) }
                )
                if case .received(let sealedBytes) = sealed {
                    pending.historySealed = sealedBytes
                }
            } catch {
                NSLog("history bundle fetch failed: %@", error.localizedDescription)
            }
            pending.stateBundle = await StateBundle.receive(
                group: pending.joined,
                identity: identity,
                rendezvousPseudonym: rendezvous,
                afterCursor: cursor
            )
        }

        /// Phase 2a3d8-ui history catch-up bundle + ADR-0050 section 2.9
        /// state bundle, both best-effort after the join.
        private func receiveBundles(
            joined: DeviceGroup,
            accountIdB64: String,
            welcomeCursor cursor: Int64
        ) async {
            do {
                let secret = try joined.historyBundleSecret(identity: identity)
                let sealed = try await WelcomeCourier.pollForHistoryBundle(
                    rendezvousPseudonym: rendezvous,
                    welcomeCursor: cursor,
                    opens: { HistoryBundleCodec.opens(secret: secret, sealedBytes: $0) }
                )
                if case .received(let sealedBytes) = sealed {
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
                let applied = StateBundle.apply(bundle, accountIdB64: accountIdB64)
                NSLog("state bundle: %d contacts, %d pins", applied.contacts, applied.pins)
            }
        }

        /// Poll for the scanner's sealed recovery row (v3), open it under
        /// the shared post-join exporter key, decode it with the core,
        /// redeem the grant. Every failure degrades (row absent, refused or
        /// undecodable → `nil`, which the caller shows as "could not hand
        /// over the keys"; redeem failure → keys-only outcome). Transient
        /// secrets are zeroized (best-effort under Swift `Data` value
        /// semantics). G-56: the decoded predecessor seeds ride the outcome;
        /// the restore stores them first, before the seed and the landing.
        private func runJoinerRecovery(
            exportSecret: (String, Data, UInt32) throws -> Data,
            welcomeCursor: Int64
        ) async -> JoinRecoveryOutcome? {
            var exporterKey = Data()
            var payload = Data()
            defer {
                exporterKey.resetBytes(in: 0..<exporterKey.count)
                payload.resetBytes(in: 0..<payload.count)
            }
            do {
                exporterKey = try exportSecret(
                    recoveryExporterLabel(),
                    rendezvous,
                    recoveryExporterKeyLen()
                )
            } catch {
                NSLog(
                    "recovery payload key not derived (join unaffected): %@",
                    error.localizedDescription
                )
                return nil
            }
            let key = exporterKey
            let outcome: RecoveryPayload.RecoveryPayloadOutcome
            do {
                // A recovery row that does not open and decode under this
                // join's key is passed over, and the poll keeps reading.
                outcome = try await RecoveryPayload.pollForRecoveryPayload(
                    rendezvousPseudonym: rendezvous,
                    afterCursor: welcomeCursor,
                    opens: { RecoveryPayload.opens(sealed: $0, key: key) }
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
            let decoded: FfiRecoveryPayload
            do {
                payload = try openWithKey(key: exporterKey, wire: sealedBlob)
                decoded = try decodeRecoveryPayload(plaintext: payload)
            } catch {
                NSLog(
                    "recovery payload open failed (join unaffected): %@",
                    error.localizedDescription
                )
                return nil
            }
            NSLog("[g56] recovery row: %d earlier key(s) carried", decoded.predecessors.count)
            // Redeem the single-use grant for a session token. The grant
            // alone names the account (ADR-0056); the response carries the
            // account id. A redeem failure (endpoint down, grant expired)
            // degrades to a keys-only outcome.
            var grant = decoded.grant
            defer { grant.resetBytes(in: 0..<grant.count) }
            var redeemed: DeviceAuthorizeRedeemResult?
            do {
                redeemed = try await redeem(base64UrlEncodeNoPad(grant))
            } catch {
                NSLog(
                    "device grant redeem failed (keys-only outcome): %@",
                    error.localizedDescription
                )
                redeemed = nil
            }
            return JoinRecoveryOutcome(
                phraseEntropy: decoded.phraseEntropy,
                session: redeemed?.session,
                accountId: redeemed?.accountId,
                predecessors: decoded.predecessors
            )
        }
    }

    /// ADR-0056 security pass — what a join by name received, held in
    /// memory until the restore has verified the phrase seed and checked
    /// the typed name at discovery. ``persist()`` is the one place any of it
    /// reaches the disk; releasing the object without calling it leaves the
    /// device as it was (the device seed is zeroed on release).
    final class PendingJoinByName {
        let identity: Identity
        let joined: DeviceGroup
        let accountIdBytes: Data
        let accountIdB64: String
        /// The sealed history bundle, opened and applied at ``persist()``.
        var historySealed: Data?
        /// The state bundle (contacts, pins, known groups), applied at
        /// ``persist()``.
        var stateBundle: StateBundle.V4?
        /// The only owner of the device-identity seed once the join moved
        /// it here; zeroed in place after the write and on release.
        private var deviceSeed: Data
        private(set) var persisted = false
        /// The account's wipes when the join was held (``AccountStoreGate/WipeMark``).
        ///
        /// ``persist()`` runs off the main actor (the restore awaits it as a
        /// nonisolated closure), after the restore's network checks, before
        /// the session lands. The device seed it writes is refused when the
        /// account was wiped on this device since: Forget device or Delete
        /// account on this device must not get a device key back. An earlier
        /// sign-out of the same account (sealed, not wiped) or a wipe before
        /// the join started refuses nothing: this is a sign-in, and the seed
        /// is the leaf the account's other device has just added.
        private let wipeMark: AccountStoreGate.WipeMark

        init(identity: Identity, joined: DeviceGroup, accountIdBytes: Data, deviceSeed: Data) {
            self.identity = identity
            self.joined = joined
            self.accountIdBytes = accountIdBytes
            self.accountIdB64 = base64Encode(accountIdBytes)
            self.deviceSeed = deviceSeed
            self.wipeMark = AccountStoreGate.wipeMark(base64Encode(accountIdBytes))
        }

        deinit {
            wipeDeviceSeed()
        }

        /// Write the device-identity seed, the DeviceGroup snapshot, the
        /// history rows and the state bundle under the redeemed account.
        /// Each write is best-effort, as it was at join time before the
        /// security pass. Runs once.
        func persist() {
            guard !persisted else { return }
            persisted = true
            // Wiped since the join was held: none of it is this device's any
            // more. (Each write below is gated again on its own, so a wipe
            // that lands from here on refuses the writes after it.)
            guard let check = AccountStoreGate.enter(unlessWipedSince: wipeMark) else {
                NSLog("join by name not persisted: the account was removed from this device")
                wipeDeviceSeed()
                historySealed = nil
                stateBundle = nil
                return
            }
            check.leave()
            if !deviceSeed.isEmpty {
                if let gate = AccountStoreGate.enter(unlessWipedSince: wipeMark) {
                    defer { gate.leave() }
                    do {
                        try DeviceIdentityStore.putDeviceIdentitySeed(
                            accountIdB64: accountIdB64,
                            seed: deviceSeed
                        )
                    } catch {
                        NSLog("device identity seed persist (join by name) failed: %@", error.localizedDescription)
                    }
                } else {
                    NSLog("device identity seed not persisted (join by name): the account was removed from this device")
                }
                wipeDeviceSeed()
            }
            // ADR-0081 decision 3 — the write fails closed; this step has no
            // caller to stop, so a failure is logged and the DeviceGroup is
            // not on disk: the next start finds no state and rejoins through
            // a sibling (decision 4) instead of running on a handle nothing
            // persisted.
            do {
                try DeviceGroupLifecycle.persistSnapshotOrThrow(
                    identity: identity,
                    accountIdB64: accountIdB64
                )
                // G-56 — the id a later re-pairing has to name.
                JoinedDeviceGroupIdStore().save(try joined.groupId(), accountIdB64: accountIdB64)
            } catch {
                NSLog("DeviceGroup snapshot (join by name) not persisted: %@", String(describing: error))
            }
            if var sealed = historySealed {
                historySealed = nil
                do {
                    var secret = try joined.historyBundleSecret(identity: identity)
                    defer { secret.resetBytes(in: 0..<secret.count) }
                    let applied = try HistoryBundleCodec.applyToMessageLog(
                        accountIdB64: accountIdB64,
                        secret: secret,
                        sealedBytes: sealed
                    )
                    NSLog("history catch-up: applied %d rows", applied)
                } catch {
                    NSLog("history bundle apply failed: %@", error.localizedDescription)
                }
                sealed.resetBytes(in: 0..<sealed.count)
            }
            if let bundle = stateBundle {
                stateBundle = nil
                let applied = StateBundle.apply(bundle, accountIdB64: accountIdB64)
                NSLog("state bundle: %d contacts, %d pins", applied.contacts, applied.pins)
            }
        }

        private func wipeDeviceSeed() {
            deviceSeed.withUnsafeMutableBytes { raw in
                guard let base = raw.baseAddress else { return }
                memset(base, 0, raw.count)
            }
            deviceSeed = Data()
        }
    }

    /// Joining-device side entry point for a device that knows its
    /// account (signed in, or parked on the identity prompt). Builds the
    /// DeviceIdentity from the cached seed, generates a fresh rendezvous
    /// slot, produces a KeyPackage, encodes the pairing QR naming the
    /// account by `loginHandle` (ADR-0056). `deviceCardEnvelope` is this
    /// device's own card for the confirming device to name it by (UX section
    /// 2.1); `nil` sends none. The view drives the rest via the returned
    /// [`JoinSession`].
    public static func prepareJoinFlow(
        loginHandle: Data,
        accountIdBytes: Data,
        accountIdB64: String,
        deviceCardEnvelope: Data? = nil,
        redeem: @escaping (String) async throws -> DeviceAuthorizeRedeemResult = {
            try await AuthClient().deviceAuthorizeRedeem(grantTokenB64: $0)
        }
    ) throws -> JoinSession {
        let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(
            accountIdB64: accountIdB64
        )
        let identity = try Identity.fromSeed(seed: seed)
        DeviceGroupLifecycle.noteHandle(identity, accountIdB64: accountIdB64)
        let keyPackage = try identity.createKeyPackage()
        let rendezvous = PairingCodec.generateRendezvousPseudonym()
        let qrPayload = try PairingCodec.buildPairingQrPayload(
            loginHandle: loginHandle,
            keyPackage: keyPackage,
            rendezvousPseudonym: rendezvous,
            deviceCardEnvelope: deviceCardEnvelope
        )
        return JoinSession(
            qrPayload: qrPayload,
            rendezvous: rendezvous,
            identity: identity,
            target: .account(accountIdBytes: accountIdBytes, accountIdB64: accountIdB64),
            loginHandle: loginHandle,
            keyPackage: keyPackage,
            pendingDeviceSeed: Data(),
            redeem: redeem
        )
    }

    /// G-56 — the join of a device that is ALREADY in the account's
    /// DeviceGroup (it came in by pairing and parked for the account's
    /// earlier keys) and pairs again from the parked screen. The confirming
    /// device cannot add a second leaf under the same device key (the core
    /// refuses: `DuplicateSignatureKey`), so this device keeps its device
    /// identity and signs, with its old leaf key, that its leaf may be
    /// replaced by the new KeyPackage in the commit leaving the DeviceGroup's
    /// latest epoch (learned by an empty knock, the ADR-0081 rejoin probe;
    /// this device's own epoch when the probe gets no answer). The proof
    /// rides in the pairing payload (`buildPairingQrWithSelfRemovalProof`);
    /// the confirming device replaces the old leaf in one commit
    /// (``runScanFlow(scannedBytes:ownLoginHandle:accountIdBytes:accountIdB64:recovery:)``).
    ///
    /// Falls back to ``prepareJoinFlow(loginHandle:accountIdBytes:accountIdB64:deviceCardEnvelope:redeem:)``
    /// when this device holds no DeviceGroup state it can read, or its leaf
    /// is no longer in it.
    public static func prepareRejoinPairingFlow(
        loginHandle: Data,
        accountIdBytes: Data,
        accountIdB64: String,
        deviceCardEnvelope: Data? = nil,
        snapshotStore: DeviceGroupSnapshotStore = DeviceGroupSnapshotStore(),
        joinedGroupIds: JoinedDeviceGroupIdStore = JoinedDeviceGroupIdStore(),
        latestEpoch: @escaping (_ groupId: Data) async throws -> UInt64? = { groupId in
            try await DsClient().knock(groupId: groupId, payloads: []).epoch
        },
        redeem: @escaping (String) async throws -> DeviceAuthorizeRedeemResult = {
            try await AuthClient().deviceAuthorizeRedeem(grantTokenB64: $0)
        }
    ) async throws -> JoinSession {
        let plain = {
            try prepareJoinFlow(
                loginHandle: loginHandle, accountIdBytes: accountIdBytes, accountIdB64: accountIdB64,
                deviceCardEnvelope: deviceCardEnvelope, redeem: redeem)
        }
        guard let seed = (try? DeviceIdentityStore.getDeviceIdentitySeed(accountIdB64: accountIdB64)) ?? nil,
              let snapshot = snapshotStore.getSnapshot(accountIdB64: accountIdB64),
              let groupId = joinedGroupIds.load(accountIdB64: accountIdB64) else {
            return try plain()
        }
        let held = try Identity.fromSeed(seed: seed)
        guard (try? held.restoreStorage(snapshot: snapshot)) != nil,
              let group = (try? loadDeviceGroupById(identity: held, accountId: accountIdBytes, groupId: groupId)) ?? nil,
              ((try? group.devices()) ?? []).contains(where: { $0.signaturePublicKey == held.signaturePublicKey() }),
              let heldEpoch = try? group.epoch() else {
            return try plain()
        }
        let probed: UInt64?
        do {
            probed = try await latestEpoch(groupId)
        } catch {
            NSLog("[g56] the DeviceGroup epoch probe failed; this device's own epoch is used")
            probed = nil
        }
        let epoch = probed ?? heldEpoch
        // The joining identity: the same device seed, an empty provider (the
        // Welcome is processed there, as on the ordinary join).
        let identity = try Identity.fromSeed(seed: seed)
        DeviceGroupLifecycle.noteHandle(identity, accountIdB64: accountIdB64)
        let keyPackage = try identity.createKeyPackage()
        let proof = try signDeviceGroupSelfRemoval(
            oldIdentity: identity, groupId: groupId, epoch: epoch, replacementKeyPackage: keyPackage)
        let rendezvous = PairingCodec.generateRendezvousPseudonym()
        let qrPayload = try PairingCodec.buildPairingQrPayload(
            loginHandle: loginHandle,
            keyPackage: keyPackage,
            rendezvousPseudonym: rendezvous,
            deviceCardEnvelope: deviceCardEnvelope,
            selfRemovalProof: proof
        )
        NSLog("[g56] this device is in the DeviceGroup already: it pairs again with a self-removal proof")
        return JoinSession(
            qrPayload: qrPayload,
            rendezvous: rendezvous,
            identity: identity,
            target: .account(accountIdBytes: accountIdBytes, accountIdB64: accountIdB64),
            loginHandle: loginHandle,
            keyPackage: keyPackage,
            pendingDeviceSeed: Data(),
            selfRemovalProof: proof,
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
        deviceCardEnvelope: Data? = nil,
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
            rendezvousPseudonym: rendezvous,
            deviceCardEnvelope: deviceCardEnvelope
        )
        return JoinSession(
            qrPayload: qrPayload,
            rendezvous: rendezvous,
            identity: identity,
            target: .byName,
            loginHandle: loginHandle,
            keyPackage: keyPackage,
            pendingDeviceSeed: seed,
            redeem: redeem
        )
    }

    /// ADR-0056 — join a DeviceGroup Welcome before the account is known:
    /// a plain group join, adopted afterwards by
    /// ``adoptJoinedDeviceGroup(identity:accountIdBytes:groupId:)``.
    static func joinWelcomeByName(welcomeBytes: Data, identity: Identity) throws -> Group {
        // ADR-0081 amendment — the DeviceGroup entry point: `Group.joinFromWelcome`
        // refuses a Welcome with no role table.
        try deviceGroupJoinWelcomeByName(welcome: welcomeBytes, identity: identity)
    }

    /// ADR-0056 — adopt the group a join by name joined as the DeviceGroup
    /// of `accountIdBytes`, under its own id (`groupId`). ADR-0078 section 6:
    /// the DeviceGroup id is derived from the account seed, not the account
    /// id, so the account the redeem named cannot be checked against it
    /// here; the recovery row sealed under this group's exporter is the
    /// binding. ``JoinByNameError/wrongAccount`` when the group is not in the
    /// identity's storage at all.
    static func adoptJoinedDeviceGroup(
        identity: Identity, accountIdBytes: Data, groupId: Data
    ) throws -> DeviceGroup {
        guard let group = try loadDeviceGroupById(
            identity: identity, accountId: accountIdBytes, groupId: groupId) else {
            throw JoinByNameError.wrongAccount
        }
        DeviceGroupLifecycle.clearFoundingMark(joined: group)
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
        /// Whether the recovery row (grant + phrase entropy) was published
        /// for the new device. `false` when this device holds no recovery
        /// phrase or the best-effort publish failed: the new device is
        /// linked but received no keys, and the person is told so
        /// (`device_link.keys_not_sent`).
        public let keysSent: Bool
    }

    /// Inputs for the recovery payload the scanner publishes alongside the
    /// Welcome, so the fresh device gets access (a single-use grant) and
    /// the keys (the entropy of the recovery phrase) with no password or
    /// phrase entry.
    public final class ScanRecovery {
        /// The 16-byte entropy of the recovery phrase this device holds,
        /// or `nil` when it holds none: then no recovery row is published.
        /// A defensive copy; consumed and zeroized by the scan flow.
        fileprivate var phraseEntropy: Data?
        /// Mints a single-use device grant
        /// (`AuthClient.deviceAuthorizeStart`, bearer);
        /// returns the URL-safe-base64 raw grant token.
        fileprivate let deviceAuthorizeStart: () async throws -> String
        /// G-56 — where the account's own predecessor seeds are read for the
        /// row (the slot the state bundle reads).
        fileprivate let ownPredecessors: OwnPredecessorStoring

        public init(
            phraseEntropy: Data?,
            ownPredecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore(),
            deviceAuthorizeStart: @escaping () async throws -> String
        ) {
            // Defensive copy — the scan flow consumes + wipes it.
            self.phraseEntropy = phraseEntropy.map { Data($0) }
            self.ownPredecessors = ownPredecessors
            self.deviceAuthorizeStart = deviceAuthorizeStart
        }

        /// The recovery inputs of the account's stored phrase: its entropy
        /// when ``PhraseStore`` holds a phrase the core accepts, else none.
        public convenience init(
            accountIdB64: String,
            phraseStore: PhraseStore = PhraseStore(),
            deviceAuthorizeStart: @escaping () async throws -> String
        ) {
            self.init(
                phraseEntropy: DevicePhrase.entropy(accountIdB64: accountIdB64, store: phraseStore),
                deviceAuthorizeStart: deviceAuthorizeStart
            )
        }

        /// Best-effort wipe of the entropy copy this object owns.
        fileprivate func wipe() {
            guard let count = phraseEntropy?.count else { return }
            phraseEntropy?.resetBytes(in: 0..<count)
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
        // Wipe the entropy copy on every exit path — including a
        // thrown pairing error before the recovery block runs.
        defer { recovery?.wipe() }

        // ADR-0056 — the payload names the account by the login handle of
        // the username typed on the new device; it must be this account's
        // own handle, else nothing is committed.
        let decoded = try PairingCodec.parsePairingQrPayload(
            wireBytes: scannedBytes,
            expectedLoginHandle: ownLoginHandle
        )
        // ADR-0081 decision 2 — the Add goes through the DeviceGroup commit
        // outbox: the entry (row, Welcome, N+1 snapshot) beside the N
        // snapshot, the commit and the N+1 registration in one request, N+1
        // made current, then the Welcome to the pairing RENDEZVOUS mailbox
        // (unclaimed by design, ADR-0053 section 2). The bundles below are
        // sealed on the same throwaway handle, at the post-add epoch, only
        // once the commit landed.
        let result = try await DeviceGroupCommits.commitChange(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            build: { ensured in
                // G-56 — a device already in the DeviceGroup pairing again
                // with its self-removal proof: its old leaf out and the new
                // one in, in ONE commit (the ADR-0081 rejoin replacement),
                // the proof in the removal-authorisation slot.
                if let replaced = try replacementCommit(ensured: ensured, decoded: decoded) {
                    return replaced
                }
                let addition = try ensured.group.addDeviceFromWire(
                    identity: ensured.identity,
                    keyPackage: decoded.keyPackage
                )
                // G-13 (ADR-0015): every DeviceGroup journal entry is wrapped;
                // an add-device commit carries no attestation.
                return DeviceGroupCommits.Built(
                    kind: .add,
                    row: DeviceGroupJournalEnvelope.encode(message: addition.commit, attestation: nil),
                    welcome: addition.welcome,
                    welcomeRendezvous: decoded.rendezvousPseudonym)
            },
            after: { ensured in
                // UX section 2.1 — the new device's row is named at once by
                // the card it put in its payload, until its own card arrives.
                // The commit landed, so the roster read here holds its leaf.
                if let card = PairingCodec.deviceCard(of: decoded),
                   let key = try? keyPackageSignatureKey(keyPackage: decoded.keyPackage),
                   let roster = try? ensured.group.devices() {
                    DeviceCardCenter.shared.seedFromPairing(
                        accountIdB64: accountIdB64, keyHex: hexEncode(key), card: card,
                        rosterKeysHex: roster.map { hexEncode($0.signaturePublicKey) })
                }
                return await sendBundles(
                    ensured: ensured, decoded: decoded, accountIdB64: accountIdB64, recovery: recovery)
            }
        )
        guard let result, case .landed = result.outcome else {
            // `nil` is not reachable (the build always commits); `.lost`: the
            // DeviceGroup moved under this commit. The person scans again.
            throw DeviceGroupCommits.Lost()
        }
        return ScanResult(
            memberCount: 0,
            newDeviceKey: try keyPackageSignatureKey(keyPackage: decoded.keyPackage),
            keysSent: result.value ?? false
        )
    }

    /// G-56 — refused: the pairing payload carries a self-removal proof that
    /// does not cover replacing the device's leaf at the DeviceGroup's
    /// current epoch (a stale epoch, another group, another KeyPackage). The
    /// person tries again; the joining device signs at the new epoch.
    public struct SelfRemovalProofNotCovering: Error, Equatable {}

    /// G-56 — the replacement commit for a pairing payload that carries a
    /// self-removal proof, built at the epoch of `ensured` (the commit's own:
    /// the outbox calls `build` on the handle the commit leaves), or `nil`
    /// when the payload carries no proof or the device's key has no leaf in
    /// the DeviceGroup (an ordinary add follows). Throws
    /// ``SelfRemovalProofNotCovering`` when the proof does not cover the
    /// replacement there: a removal peers would refuse under the policy is
    /// never committed, and without the policy an uncovered proof is still
    /// the joining device's word only.
    static func replacementCommit(
        ensured: DeviceGroupLifecycle.Ensured, decoded: DecodedPairingQr
    ) throws -> DeviceGroupCommits.Built? {
        guard let proof = decoded.selfRemovalProof else { return nil }
        let key = try keyPackageSignatureKey(keyPackage: decoded.keyPackage)
        guard let previous = try ensured.group.devices().first(where: { $0.signaturePublicKey == key }) else {
            return nil
        }
        guard deviceGroupSelfRemovalCovers(
            identity: ensured.identity, proof: proof, groupId: try ensured.group.groupId(),
            epoch: try ensured.group.epoch(), removedSignaturePubkey: key, keyPackage: decoded.keyPackage)
        else {
            NSLog("[g56] the pairing payload's self-removal proof does not cover this epoch; refused")
            throw SelfRemovalProofNotCovering()
        }
        let replaced = try ensured.group.replaceDeviceAt(
            identity: ensured.identity, leafIndex: previous.leafIndex, keyPackage: decoded.keyPackage)
        return DeviceGroupCommits.Built(
            kind: .rejoinReplace,
            row: DeviceGroupJournalEnvelope.encode(message: replaced.commit, attestation: proof),
            welcome: replaced.welcome,
            welcomeRendezvous: decoded.rendezvousPseudonym)
    }

    /// The account's own predecessors a recovery row carries: the seeds of
    /// the slot (oldest first, at most ``recoveryPredecessorsMax()``, the
    /// state bundle's choice when there are more) with their adoption times,
    /// minus any seed equal to the current one. Throws when the slot cannot
    /// be read.
    static func rowPredecessors(
        accountIdB64: String,
        currentSeed: Data,
        store: OwnPredecessorStoring,
        adoptions: PredecessorMailbox.AdoptionStore = PredecessorMailbox.AdoptionStore(),
        nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)
    ) throws -> [FfiRecoveryPredecessor] {
        let seeds = try store.loadChecked(accountIdB64: accountIdB64).filter { $0 != currentSeed }
        let carried = StateBundle.carriedOwnPredecessors(seeds: seeds) {
            adoptions.recordIfAbsent(accountIdB64: accountIdB64, keyB64: $0, nowMs: nowMs)
        }
        return carried.prefix(Int(recoveryPredecessorsMax())).compactMap { own in
            guard let seed = Data(base64Encoded: own.seedB64) else { return nil }
            return FfiRecoveryPredecessor(seed: seed, adoptedAtMs: UInt64(max(0, own.adoptedAtMs)))
        }
    }

    /// Compose, seal and publish the recovery row v3. Returns whether it was
    /// published.
    @MainActor
    private static func publishRecoveryRow(
        recovery: ScanRecovery,
        entropy: Data,
        ensured: DeviceGroupLifecycle.Ensured,
        rendezvous: Data,
        accountIdB64: String
    ) async -> Bool {
        var grant = Data()
        var exporterKey = Data()
        var payload = Data()
        var predecessors: [FfiRecoveryPredecessor] = []
        defer {
            grant.resetBytes(in: 0..<grant.count)
            exporterKey.resetBytes(in: 0..<exporterKey.count)
            payload.resetBytes(in: 0..<payload.count)
            for index in predecessors.indices {
                predecessors[index].seed.resetBytes(in: 0..<predecessors[index].seed.count)
            }
        }
        do {
            let currentSeed = try RecoveryMnemonic.fromEntropy(entropy: entropy).toSeed()
            do {
                predecessors = try rowPredecessors(
                    accountIdB64: accountIdB64, currentSeed: currentSeed, store: recovery.ownPredecessors)
            } catch {
                NSLog("[g56] the own predecessor slot could not be read: no recovery row published")
                return false
            }
            guard let grantBytes = base64UrlDecodeNoPad(
                try await recovery.deviceAuthorizeStart()
            ) else {
                throw RecoveryPayloadError.argument("grant token is not URL-safe base64")
            }
            grant = grantBytes
            exporterKey = try ensured.group.exportSecret(
                identity: ensured.identity,
                label: recoveryExporterLabel(),
                context: rendezvous,
                length: recoveryExporterKeyLen()
            )
            payload = try composeRecoveryPayload(
                grant: grant, phraseEntropy: entropy, predecessors: predecessors)
            let sealed = try sealWithKey(key: exporterKey, plaintext: payload)
            try await RecoveryPayload.publishRecoveryPayload(
                rendezvousPseudonym: rendezvous,
                sealedBlob: sealed
            )
            NSLog("[g56] recovery row published with %d earlier key(s)", predecessors.count)
            return true
        } catch {
            NSLog(
                "recovery payload publish failed (pairing unaffected): %@",
                error.localizedDescription
            )
            return false
        }
    }

    /// The bundles a pairing sends once its commit landed, sealed under the
    /// post-add epoch of `ensured`: the history bundle, the recovery payload
    /// and the state bundle. Each is best effort. Returns whether the
    /// recovery payload (the keys) was sent.
    @MainActor
    private static func sendBundles(
        ensured: DeviceGroupLifecycle.Ensured,
        decoded: DecodedPairingQr,
        accountIdB64: String,
        recovery: ScanRecovery?
    ) async -> Bool {
        // ADR-0060 section 7 — the setting and the reach-key versions reach
        // the new device from the sync runtime, which the caller reloads at the
        // post-add epoch (`reloadAfterLocalMembershipChange`): this handle
        // seals nothing more, so the two never share a sender generation.

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

        // Recovery payload v3 (G-56). Mint a single-use device grant, read
        // the account's own predecessor seeds, derive the exporter key at the
        // POST-ADD epoch (both devices share it after the joiner processes
        // the Welcome), compose (grant ‖ phrase entropy ‖ predecessors) with
        // the core, seal and publish the tagged rendezvous row. A device that
        // holds no recovery phrase, or cannot read its predecessor slot,
        // publishes no row ("cannot be read" is not "none": the joiner gets
        // the existing error and the person starts again). Any failure is
        // non-fatal for the pairing itself: `keysSent` is `false`. The server
        // never sees the entropy or the seeds; transient copies are zeroized.
        var keysSent = false
        if let recovery, let entropy = recovery.phraseEntropy {
            keysSent = await publishRecoveryRow(
                recovery: recovery, entropy: entropy, ensured: ensured,
                rendezvous: decoded.rendezvousPseudonym, accountIdB64: accountIdB64)
        } else if recovery != nil {
            NSLog("no recovery phrase on this device: no recovery row published")
        }

        // ADR-0050 section 2.9 — the state bundle: contacts, pins, the ids
        // of every group this device holds, their shared names, the profiles
        // it holds and the pending contacts (bundle v3), sealed under the per-pairing
        // exporter like the recovery row. Best-effort; a failure leaves the new
        // device with the chats it is admitted to and no contact names.
        do {
            // Bundle v3 — the profiles this device holds for other people
            // ride along (read here, on the main actor), never this
            // account's own.
            let bundle = StateBundle.build(
                accountIdB64: accountIdB64,
                profiles: ProfileDirectory.shared.profilesForBundle(accountIdB64: accountIdB64),
                ownKeysB64: StateBundle.ownKeysB64(accountIdB64: accountIdB64),
                // Bundle v4 — the newest epoch seeds of the subscribed channels.
                channelSeeds: ChannelSubscriptionSync.shared.carriedSeedsForBundle())
            try await StateBundle.publish(
                bundle: bundle,
                group: ensured.group,
                identity: ensured.identity,
                rendezvousPseudonym: decoded.rendezvousPseudonym
            )
        } catch {
            NSLog("state bundle publish failed: %@", error.localizedDescription)
        }

        return keysSent
    }
}
