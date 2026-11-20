// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0073 "Reset account keys" — the client-side rules, free of state:
//
//   * the landing rule of section 4.2 (`KeyResetLanding`): what a device does
//     with the identity the directory publishes, on EVERY session landing;
//   * the signed-out state of section 3.5 (`KeysSignOutState`);
//   * the publish steps a reset owes after its server step (`KeyResetPublish`)
//     and the record that survives a relaunch (`KeyResetPublishRecord`);
//   * the typed refusals of discovery's identity upload
//     (`IdentityUploadRefusal`);
//   * the attestation check against the pinned attest key
//     (`KeyResetAttestationVerifying`).
//
// `AuthRepository` is the state around them. Mirror of the web
// `apps/web/src/auth/key-reset.ts`.

// MARK: - The landing rule (section 4.2)

/// What a session landing does about the account's published identity.
enum KeyResetLanding {

    /// What the directory served (`GET /discovery/identity/{account}`).
    enum Observation: Equatable {
        /// The fetch failed (network, non-404 status, undecodable body).
        case unreachable
        /// 404 — nothing published for this account.
        case absent
        /// A bundle is published: its decoded Ed25519 key (`nil` when the
        /// served value did not decode) and its key generation.
        case present(signaturePubkey: Data?, keyGeneration: UInt64)
    }

    /// Whether the landing uploads this device's bundle.
    enum Publish: Equatable {
        /// Nothing is uploaded (the directory could not be read, or this
        /// landing is one that does not upload).
        case skip
        /// Re-publish the same key (keeps the row current; no genesis
        /// attestation, the key is already logged).
        case republish
        /// The account has no bundle: the first publish, with the ADR-0036
        /// genesis attestation.
        case firstPublish
    }

    /// The verdict.
    enum Verdict: Equatable {
        /// Land the session, uploading per `publish`.
        case land(publish: Publish)
        /// Rule 3: the directory publishes OTHER keys of a HIGHER
        /// generation. This device's keys are stale: the signed-out screen,
        /// the wipe runs by itself. Nothing is uploaded.
        case staleKeys
        /// Rule 4: the directory publishes other keys at the SAME (or a
        /// lower) generation, or a key that does not decode. An anomaly,
        /// not a known reset: the blocking screen, the wipe on the tap.
        /// Nothing is uploaded.
        case mismatch
    }

    /// Decide. `uploads` is whether THIS landing uploads at all: an explicit
    /// sign-in does (it re-publishes an unchanged key and makes the first
    /// publish of an account with no bundle); the silent restore does not,
    /// as before ADR-0073. The two refusals never depend on it.
    static func decide(
        observed: Observation,
        derivedSignaturePubkey: Data,
        ownKeyGeneration: UInt64,
        uploads: Bool
    ) -> Verdict {
        switch observed {
        case .unreachable:
            return .land(publish: .skip)
        case .absent:
            return .land(publish: uploads ? .firstPublish : .skip)
        case .present(let published, let publishedGeneration):
            guard let published, published == derivedSignaturePubkey else {
                return publishedGeneration > ownKeyGeneration ? .staleKeys : .mismatch
            }
            return .land(publish: uploads ? .republish : .skip)
        }
    }
}

// MARK: - The signed-out state (section 3.5, UX 15.2)

/// Why this device's keys are not the account's keys any more, and where
/// the sign-out is. Rendered by the removed-device screen in its reset
/// variant; replaces the whole app while set.
public enum KeysSignOutState: Equatable {
    /// The keys were reset on another device; the automatic wipe is
    /// running.
    case removing
    /// The wipe is done: one button, "Sign in".
    case removed
    /// This device's keys do not match the published identity at the same
    /// generation: the wipe runs on the tap ("Sign out").
    case mismatch
}

// MARK: - Discovery's refusals (section 4.1)

/// The typed refusals of `POST /discovery/identity` under ADR-0073.
enum IdentityUploadRefusal: Equatable {
    /// `409 identity-change-needs-attestation`: a key change without a
    /// proof or an attestation. What a publish-back of old keys gets.
    case needsAttestation
    /// `400 key-reset-attestation-invalid`.
    case attestationInvalid
    /// `409 key-generation-not-newer`: the reset already landed (a retry),
    /// or was overtaken by a later one. On a retry: done.
    case generationNotNewer
    /// `429 identity-changed-too-recently`: the cool-down, retry later.
    case changedTooRecently(retryAfterSecs: Int?)
    /// `503 key-reset-disabled`: the server has no pinned attest key.
    case keyResetDisabled
    /// `401 key-generation-superseded`: this token's generation is below
    /// the row's. The device is being signed out.
    case superseded
    /// Not one of the codes above.
    case other

    /// Read the refusal out of a discovery error; `nil` when `error` is not
    /// a `DiscoveryClientError.http`.
    static func of(_ error: Error) -> IdentityUploadRefusal? {
        guard case .http(let status, _, let body) = error as? DiscoveryClientError else {
            return nil
        }
        return classify(status: status, body: body)
    }

    static func classify(status: Int, body: String) -> IdentityUploadRefusal {
        let json = (try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any]
        let code = json?["error"] as? String
        switch (status, code) {
        case (409, "identity-change-needs-attestation"): return .needsAttestation
        case (400, "key-reset-attestation-invalid"): return .attestationInvalid
        case (409, "key-generation-not-newer"): return .generationNotNewer
        case (429, "identity-changed-too-recently"):
            return .changedTooRecently(retryAfterSecs: retryAfterSecs(json?["retry_after_secs"]))
        case (503, "key-reset-disabled"): return .keyResetDisabled
        case (401, "key-generation-superseded"): return .superseded
        default: return .other
        }
    }

    /// `retry_after_secs` as a whole number of seconds, whatever numeric
    /// type `JSONSerialization` read it as (`120`, `120.0`). A missing,
    /// non-numeric, boolean or non-positive value reads as `nil`.
    static func retryAfterSecs(_ value: Any?) -> Int? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        let secs = number.doubleValue
        guard secs.isFinite, secs > 0 else { return nil }
        return Int(min(secs.rounded(.up), Double(Int32.max)))
    }
}

// MARK: - Waiting for the directory (UX 15.4, amended 2026-11-12)

/// The directory's cool-down on a reset's publish: `429
/// identity-changed-too-recently` with how long is left. The device waits
/// that long and then publishes by itself.
public struct KeyResetPublishWait: Equatable, Sendable {
    /// The account the wait is for.
    public let accountIdB64: String
    /// Unix ms at which the directory accepts the publish.
    public let untilMs: Int64

    public init(accountIdB64: String, untilMs: Int64) {
        self.accountIdB64 = accountIdB64
        self.untilMs = untilMs
    }

    /// The longest wait taken from a refusal; a longer one is read as this.
    static let maxWaitSecs = 24 * 60 * 60

    /// The wait a failed publish asks for, or `nil` when `error` is not the
    /// cool-down refusal or carries no readable time (that refusal keeps
    /// the "could not finish" copy and Try again).
    static func after(_ error: Error, accountIdB64: String, nowMs: Int64) -> KeyResetPublishWait? {
        let cause = (error as? KeyResetPublishFailed)?.underlying ?? error
        guard case .changedTooRecently(let secs)? = IdentityUploadRefusal.of(cause),
              let secs, secs > 0 else { return nil }
        return KeyResetPublishWait(
            accountIdB64: accountIdB64,
            untilMs: nowMs + Int64(min(secs, maxWaitSecs)) * 1000)
    }
}

/// What a publish-owed screen shows (UX 15.4): the "could not finish" copy
/// with Try again, or, while the directory's cool-down runs, the waiting
/// copy with the minutes left and no Try again. Pure: the screens render it
/// once a second.
public struct PublishOwedDisplay: Equatable {
    public enum Message: Equatable {
        /// A catalog key with no argument.
        case copy(key: String)
        /// A catalog key with `{minutes}`
        /// (``PublishOwedDisplay/waitingKey(showsWords:)``).
        case waiting(key: String, minutes: Int)
    }

    public let message: Message
    /// Whether Try again is on the screen.
    public let showsTryAgain: Bool
    /// The wait is over: the screen retries by itself now.
    public let retryDue: Bool

    /// The waiting copy: the one that promises the new words for a
    /// phrase-protected account, the wordless one for any other (ADR-0075).
    public static func waitingKey(showsWords: Bool) -> String {
        showsWords ? "recovery_phrase.fresh.publish_waiting" : "account.keys_changed.waiting"
    }

    /// The retry runs this long after the time the directory named, so a
    /// time rounded down by the server does not meet the cool-down again.
    /// Chosen, not measured.
    public static let retryMarginMs: Int64 = 1_000

    /// The minutes the waiting copy names: `ceil(remaining / 60 s)`, at
    /// least 1.
    public static func minutes(remainingMs: Int64) -> Int {
        guard remainingMs > 60_000 else { return 1 }
        return Int((remainingMs + 59_999) / 60_000)
    }

    /// - Parameters:
    ///   - copyKey: the "could not finish" key for this account
    ///     (``KeysChangedCopy/publishOwedKey(showsWords:needsSignIn:)``).
    ///   - showsWords: the account is phrase-protected and its new words
    ///     are shown once published.
    ///   - waitUntilMs: the running cool-down, if any.
    ///   - needsSignIn: the stored attestation is too old; a sign-in comes
    ///     first, whatever the directory said.
    public static func decide(
        copyKey: String, showsWords: Bool, waitUntilMs: Int64?, needsSignIn: Bool, nowMs: Int64
    ) -> PublishOwedDisplay {
        guard !needsSignIn, let until = waitUntilMs else {
            return PublishOwedDisplay(message: .copy(key: copyKey), showsTryAgain: true, retryDue: false)
        }
        return PublishOwedDisplay(
            message: .waiting(key: waitingKey(showsWords: showsWords), minutes: minutes(remainingMs: until - nowMs)),
            showsTryAgain: false,
            retryDue: nowMs >= until + retryMarginMs)
    }
}

// MARK: - The baseline backup of a parked pair (base scenario 4b)

/// Whether the baseline backup of a route that landed a parked pair (the
/// new phrase after a reset, the restore prompt) goes up, by what auth
/// answered to the refresh run first.
enum BaselineBackupGate: Equatable {
    /// Auth refreshed the pair: upload.
    case upload
    /// Auth could not be asked: keep the phrase cache, no upload.
    case cacheOnly
    /// Superseded, revoked or invalid: nothing at all.
    case skip

    static func decide(_ outcome: SessionRefreshOutcome) -> BaselineBackupGate {
        switch outcome {
        case .refreshed: return .upload
        case .noCredential, .failed: return .cacheOnly
        // ADR-0099 step 3 — a suspended account uploads nothing.
        case .superseded, .revoked, .invalid, .suspended: return .skip
        }
    }
}

// MARK: - The publish a reset owes (sections 2.3, 4.2; UX 15.4)

/// The verified claims of a key-reset attestation a client needs.
public struct KeyResetClaims: Equatable, Sendable {
    public let keyGeneration: UInt64
    public let signedAtMs: Int64

    public init(keyGeneration: UInt64, signedAtMs: Int64) {
        self.keyGeneration = keyGeneration
        self.signedAtMs = signedAtMs
    }
}

/// What a reset still owes after `POST /account/keys/reset/finish`
/// succeeded, persisted per account until every step landed
/// (``RecoveryMarkers``). The attestation is a public, signed artifact; the
/// record holds no key material.
public struct KeyResetPublishRecord: Codable, Equatable {
    /// The 145-byte attestation, re-used verbatim on every retry.
    public var attestation: Data
    /// The generation it names (for the log and the directory comparison).
    public var keyGeneration: UInt64
    /// Its mint time, for the client-side 30-day bound.
    public var signedAtMs: Int64
    /// The NEW AccountIdentity Ed25519 public key it binds.
    public var newSignaturePubkey: Data
    /// `POST /discovery/identity` with the attestation landed.
    public var discoveryDone: Bool = false
    /// This device's KeyPackage, bound by the NEW account key, is published.
    public var keyPackageDone: Bool = false
    /// The DeviceGroup was re-founded and its journal re-rooted on the DS.
    public var deviceGroupDone: Bool = false

    public init(attestation: Data, keyGeneration: UInt64, signedAtMs: Int64, newSignaturePubkey: Data) {
        self.attestation = attestation
        self.keyGeneration = keyGeneration
        self.signedAtMs = signedAtMs
        self.newSignaturePubkey = newSignaturePubkey
    }

    enum CodingKeys: String, CodingKey {
        case attestation
        case keyGeneration = "key_generation"
        case signedAtMs = "signed_at_ms"
        case newSignaturePubkey = "new_signature_pubkey"
        case discoveryDone = "discovery_done"
        case keyPackageDone = "key_package_done"
        case deviceGroupDone = "device_group_done"
    }
}

/// The steps and their order, pure.
enum KeyResetPublish {

    /// One publish step. `verify` always runs first and is never recorded
    /// as done: the attestation is checked against the pinned key on every
    /// retry, so a record that was tampered with on disk publishes nothing.
    enum Step: String, Equatable, CaseIterable {
        case verify
        case discovery
        case keyPackage
        case deviceGroup
    }

    /// The client-side bound on applying a stored attestation: 30 days from
    /// its mint (ADR-0073 section 4.2; chosen, not measured).
    static let maxAttestationAgeMs: Int64 = 30 * 24 * 60 * 60 * 1000

    /// The steps still to run, in order, for `record`. Empty when nothing
    /// is owed (the record should then be cleared).
    static func remaining(_ record: KeyResetPublishRecord) -> [Step] {
        var steps: [Step] = []
        if !record.discoveryDone { steps.append(.discovery) }
        if !record.keyPackageDone { steps.append(.keyPackage) }
        if !record.deviceGroupDone { steps.append(.deviceGroup) }
        return steps.isEmpty ? [] : [.verify] + steps
    }

    /// Whether every step landed.
    static func isComplete(_ record: KeyResetPublishRecord) -> Bool {
        record.discoveryDone && record.keyPackageDone && record.deviceGroupDone
    }

    /// Record `step` as done. `verify` records nothing.
    static func marking(_ record: KeyResetPublishRecord, done step: Step) -> KeyResetPublishRecord {
        var next = record
        switch step {
        case .verify: break
        case .discovery: next.discoveryDone = true
        case .keyPackage: next.keyPackageDone = true
        case .deviceGroup: next.deviceGroupDone = true
        }
        return next
    }

    /// Whether the stored attestation may still be applied, or a fresh
    /// sign-in is asked for first (UX 15.4: "Sign in again to finish
    /// publishing your new keys"). `requireFresh` is `false` right after an
    /// explicit sign-in, which confirmed the account is still the person's.
    enum Freshness: Equatable {
        case fresh
        case signInRequired
    }

    static func freshness(signedAtMs: Int64, nowMs: Int64, requireFresh: Bool) -> Freshness {
        guard requireFresh else { return .fresh }
        return nowMs - signedAtMs > maxAttestationAgeMs ? .signInRequired : .fresh
    }

    /// What the discovery upload came to.
    enum DiscoveryOutcome: Equatable {
        /// Landed, or already landed (`409 key-generation-not-newer`).
        case done
        /// This device was signed out meanwhile.
        case superseded
        /// Retry later; the error is the caller's to show.
        case retry
    }

    /// Read the discovery step's answer out of an upload error.
    static func discoveryOutcome(_ error: Error) -> DiscoveryOutcome {
        switch IdentityUploadRefusal.of(error) {
        case .generationNotNewer: return .done
        case .superseded: return .superseded
        default: return .retry
        }
    }
}

/// A publish step failed; the record keeps what landed before it.
public struct KeyResetPublishFailed: Error {
    public let step: String
    public let underlying: Error

    public init(step: String, underlying: Error) {
        self.step = step
        self.underlying = underlying
    }
}

/// The stored attestation is older than the client-side bound: sign in
/// again before it is applied (UX 15.4).
public struct KeyResetSignInRequired: Error, Equatable {
    public init() {}
}

/// ``AuthRepository/confirmFreshPhraseSaved()`` was called while the new
/// keys are not published yet.
public struct FreshPublishPending: Error {
    public init() {}
}

// MARK: - The attestation check

/// Verifies a key-reset attestation the way a client must before it carries
/// it anywhere: signature under the attest key, binding to this account
/// (through the derived `account_ref` and DeviceGroup journal id) and to
/// the new key, and — when `maxAgeMs` is given — the freshness bound.
/// Injectable so the flow can run under a scripted verifier in tests.
protocol KeyResetAttestationVerifying {
    func verify(
        attestation: Data,
        accountId: Data,
        newSignaturePubkey: Data,
        nowMs: Int64,
        maxAgeMs: Int64?
    ) async throws -> KeyResetClaims
}

/// The production verifier: the Rust core's `verifyKeyResetAttestation`
/// under the pinned auth attest key (`DeviceAttestKey`), the same key the
/// device-attest and backup-head verifiers use.
///
/// When the build pins no key (`LakkoDeviceAttestKey` unset), the key is
/// taken from the auth origin's JWKS endpoint, the weaker posture the
/// device-attest path already documents; a JWKS that cannot be fetched
/// fails the check closed, so no attestation is ever applied unverified.
struct PinnedKeyResetAttestationVerifier: KeyResetAttestationVerifying {
    func verify(
        attestation: Data,
        accountId: Data,
        newSignaturePubkey: Data,
        nowMs: Int64,
        maxAgeMs: Int64?
    ) async throws -> KeyResetClaims {
        let key: Data
        if let pinned = DeviceAttestKey.pinnedKey() {
            key = pinned
        } else {
            NSLog("[key-reset] WARN no attest key is pinned; verifying under the JWKS-served key")
            key = try await DeviceAttestKey.fetch()
        }
        let verified: VerifiedKeyResetAttestation
        if let maxAgeMs {
            verified = try verifyKeyResetAttestationFresh(
                attestation: attestation,
                attestVerifyingKey: key,
                expectedAccountId: accountId,
                expectedPubkey: newSignaturePubkey,
                nowMillis: nowMs,
                maxAgeMillis: maxAgeMs
            )
        } else {
            verified = try verifyKeyResetAttestation(
                attestation: attestation,
                attestVerifyingKey: key,
                expectedAccountId: accountId,
                expectedPubkey: newSignaturePubkey
            )
        }
        return KeyResetClaims(keyGeneration: verified.keyGeneration, signedAtMillis: verified.signedAtMillis)
    }
}

extension KeyResetClaims {
    init(keyGeneration: UInt64, signedAtMillis: Int64) {
        self.init(keyGeneration: keyGeneration, signedAtMs: signedAtMillis)
    }
}

/// What ``KeyResetAttestationVerifying/verify(attestation:accountId:newSignaturePubkey:nowMs:maxAgeMs:)``
/// threw, read for the two outcomes the flow tells apart.
enum KeyResetVerifyFailure {
    /// `FfiError.KeyResetAttestationStale`: verified, but outside the bound.
    static func isStale(_ error: Error) -> Bool {
        if case .KeyResetAttestationStale = error as? FfiError { return true }
        return error is KeyResetSignInRequired
    }
}
