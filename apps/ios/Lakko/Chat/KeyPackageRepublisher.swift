// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// One KeyPackage ready to upload, minted on the long-lived identity.
struct KeyPackageUpload: Equatable {
    let accountId: Data
    let sessionToken: String
    let keyPackage: Data
    /// ADR-0050 — the device leaf's 32-byte signature key; discovery keys the
    /// row on `(account_id, device_id)`.
    var deviceId: Data = Data()
}

/// Publishes this device's MLS KeyPackage to discovery, from the long-lived
/// identity, crash-safely and at most one upload per burst of requests.
///
/// ## Why the order is mint → persist → upload
///
/// A KeyPackage is only useful if the private init / leaf keys behind it
/// are still on this device when a Welcome for it arrives — which can be
/// days later, after any number of relaunches. `createKeyPackage` writes
/// those keys into the in-memory OpenMLS provider, and the only durable
/// copy of the provider is the snapshot. So the snapshot is written durably
/// BEFORE the KeyPackage leaves the device, and a failed write aborts the
/// upload: publishing a KeyPackage whose private keys might not survive a
/// relaunch recreates exactly the defect this type exists to fix (every
/// Welcome to it fails), and discovery keeps one KeyPackage per account, so
/// it would replace a usable one. Fail closed — no upload.
///
/// ## Single-flight
///
/// Requests coalesce. A request made before the running pass has minted
/// joins that pass; a request made after it minted (the pass is uploading)
/// schedules exactly ONE trailing pass, however many arrive — the one
/// already minted may be the KeyPackage the triggering Welcome consumed. So
/// a burst of Welcomes in one inbox poll costs one upload, not one each,
/// and no request is ever answered by a KeyPackage minted before it.
@MainActor
final class KeyPackageRepublisher {

    /// The three steps, injected so the ordering is testable without FFI,
    /// Keychain or network.
    struct Ports {
        /// Mint a KeyPackage on the long-lived identity. `nil` when there is
        /// nothing to publish for (no bootstrapped identity, no session
        /// token) — a no-op, not an error.
        let mint: @MainActor () throws -> KeyPackageUpload?
        /// Durably persist the identity's provider snapshot. Throws on any
        /// failure.
        let persist: @MainActor () throws -> Void
        /// Upload the KeyPackage to discovery.
        let upload: @MainActor (KeyPackageUpload) async throws -> Void
    }

    /// Outcome of one pass, surfaced for tests and logs.
    enum PassOutcome: Equatable {
        case uploaded
        case nothingToPublish
        case mintFailed
        case persistFailedNoUpload
        case uploadFailed
    }

    private let ports: Ports
    /// Set by every request, cleared at the start of each pass BEFORE it
    /// mints. Still set when a pass ends means a request arrived after that
    /// pass minted, so another pass must run.
    private var pending = false
    /// The driver loop currently running, if any.
    private var driver: Task<Void, Never>?
    /// Every pass's outcome, in order. Test observability only.
    private(set) var outcomes: [PassOutcome] = []

    init(ports: Ports) {
        self.ports = ports
    }

    /// Request a republish. Returns the driver task; awaiting its `value`
    /// waits for every pass this request is covered by.
    @discardableResult
    func request() -> Task<Void, Never> {
        pending = true
        if let driver {
            return driver
        }
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            while self.pending {
                self.pending = false
                let outcome = await self.runPass()
                self.outcomes.append(outcome)
            }
            // No suspension between the loop's last `pending` check and
            // this line, so no request can slip in and find a stale driver.
            self.driver = nil
        }
        driver = task
        return task
    }

    private func runPass() async -> PassOutcome {
        let upload: KeyPackageUpload
        do {
            guard let minted = try ports.mint() else {
                return .nothingToPublish
            }
            upload = minted
        } catch {
            NSLog("keypackage republish: mint failed: \(error)")
            return .mintFailed
        }
        do {
            try ports.persist()
        } catch {
            NSLog(
                "keypackage republish: snapshot persist failed, NOT uploading " +
                "(the private keys would not survive a relaunch): \(error)"
            )
            return .persistFailedNoUpload
        }
        do {
            try await ports.upload(upload)
            return .uploaded
        } catch {
            NSLog("keypackage republish: upload failed: \(error)")
            return .uploadFailed
        }
    }
}
