// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0073 "Phase 2 revised: silent continuity", R.2 - the acceptance rule
// for a contact's account-key RESET, with rule 4 (amended 2026-11-26, the
// second time; ADR-0101 section 10).
//
// The table is written ONCE, in `lakko_contacts::reset_verdict`, and reached
// here through the core's `resetVerdict(input:)`. This file holds only the
// Swift shapes `ContactsClient` gathers and the mapping to and from the
// core's; the hand-written copy of the table this file carried until
// 2026-11-27 is gone.
//
// A reset is a key change with NO supersession proof (with one, ADR-0016's
// path runs and this file is never asked: a proven rotation is not a reset).
// Rules 1 to 3 are the server's word: an auth-signed key-reset attestation
// for exactly this account and this key, at a generation strictly above the
// one this device last accepted, and the key log (when one is configured)
// agreeing. Rule 4 is the account's: the reset carries a seal by a key the
// account announced under its OLD key before the reset. All four: `sealed`,
// the same person, everything moves. Rules 1 to 3 without 4: `unsealed`, a
// new account under the same username, nothing moves (ADR-0101 section 11).
// Anything negative is the fail-closed refusal; anything not yet known is no
// verdict at all.
//
// Free of storage and the network: `ContactsClient` gathers the inputs and
// acts on the outcome.

/// What the caller learned by running `verifyKeyResetAttestation` over the
/// bundle's `key_reset_attest` under the trusted attest key.
public enum AttestCheck: Equatable {
    /// The bundle carries no attestation.
    case absent
    /// It carries one and the verifier refused it (signature, account
    /// binding, key binding), or it is not decodable.
    case invalid
    /// No attest key could be obtained (no build-time pin and the JWKS fetch
    /// failed): nothing can be said about the attestation yet.
    case noKey
    /// It verified for this account id and the fetched key.
    case verified(keyGeneration: UInt64, signedAtMs: Int64)

    var ffi: FfiAttestCheck {
        switch self {
        case .absent: return .absent
        case .invalid: return .invalid
        case .noKey: return .noKey
        case .verified(let keyGeneration, let signedAtMs):
            return .verified(keyGeneration: keyGeneration, signedAtMs: signedAtMs)
        }
    }
}

/// The verified key-log answer for the fetched key (`verifyIdentityInLog`).
public struct LogCheck: Equatable {
    public let status: LogVerifyStatus
    /// The log's `is_reset` marker when a latest entry was read.
    public let isReset: Bool?

    public init(status: LogVerifyStatus, isReset: Bool?) {
        self.status = status
        self.isReset = isReset
    }
}

extension LogVerifyStatus {
    var ffi: FfiLogStatus {
        switch self {
        case .confirmed: return .confirmed
        case .absent: return .absent
        case .rollback: return .rollback
        case .mismatch: return .mismatch
        case .unreachable: return .unreachable
        case .off: return .off
        }
    }
}

/// Which refusal copy the action shows (`contacts.identity_changed.*`).
public enum ResetRefusal: Equatable, Sendable {
    /// The key log shows the new key and does NOT record a reset.
    case unexplained
    /// Everything else that is negative.
    case unconfirmed
}

/// The seal half of rule 4, as `ContactsClient` gathered it.
public struct SealCheck: Equatable {
    /// The signature half (`checkResetSeal`): the previous generation's
    /// announcement verifies under the held key and lists the key that
    /// signed the reset seal over the fetched key.
    public let valid: Bool
    /// The previous announcement's `seq` when valid: the new floor.
    public let announcementSeq: UInt64
    /// The core's refusal code when not valid, for the log line only.
    public let refusal: String?
    /// The key-log half (ignored by the rule when the build has no log).
    public let log: FfiSealLog

    public init(valid: Bool, announcementSeq: UInt64 = 0, refusal: String? = nil, log: FfiSealLog = .notProven) {
        self.valid = valid
        self.announcementSeq = announcementSeq
        self.refusal = refusal
        self.log = log
    }

    /// No seal at all: the bundle carried none, or it could not be read.
    public static let none = SealCheck(valid: false, refusal: "noSeal")
}

public enum ResetVerdict: Equatable, Sendable {
    /// Rules 1 to 4: the same person. Re-pin and move everything.
    case sealed(keyGeneration: UInt64, signedAtMs: Int64, announcementSeq: UInt64)
    /// Rules 1 to 3 without 4: a new account under the same username.
    case unsealed(keyGeneration: UInt64, signedAtMs: Int64)
    case refused(ResetRefusal)
    case noVerdict
}

/// The facts of one fetched bundle the rule reads.
public struct ResetVerdictInput: Equatable {
    /// The key this device HOLDS for the account (the pin, or a verified
    /// binding for a member it never pinned), standard base64.
    public let heldKeyB64: String
    /// The generation this device last accepted for the account (the
    /// bundle's generation at the first pin); 0 when none was recorded.
    public let acceptedGeneration: UInt64
    /// The fetched bundle's key and generation.
    public let fetchedKeyB64: String
    public let bundleGeneration: UInt64
    public let attest: AttestCheck
    public let log: LogCheck
    public let seal: SealCheck

    public init(
        heldKeyB64: String,
        acceptedGeneration: UInt64,
        fetchedKeyB64: String,
        bundleGeneration: UInt64,
        attest: AttestCheck,
        log: LogCheck,
        seal: SealCheck = .none
    ) {
        self.heldKeyB64 = heldKeyB64
        self.acceptedGeneration = acceptedGeneration
        self.fetchedKeyB64 = fetchedKeyB64
        self.bundleGeneration = bundleGeneration
        self.attest = attest
        self.log = log
        self.seal = seal
    }
}

/// R.2 with rule 4: the core's table (`lakko_contacts::reset_verdict`).
///
/// A key that is not base64 can be neither held nor fetched: no verdict
/// (nothing to accept, nothing to refuse). Callers must not ask for an
/// unchanged key; one that does is answered `noVerdict` by the core.
public func resetVerdict(_ input: ResetVerdictInput) -> ResetVerdict {
    guard let held = Data(base64Encoded: input.heldKeyB64),
          let fetched = Data(base64Encoded: input.fetchedKeyB64) else { return .noVerdict }
    let answer = LakkoCore.resetVerdict(input: FfiResetVerdictInput(
        heldKey: held,
        acceptedGeneration: input.acceptedGeneration,
        fetchedKey: fetched,
        bundleGeneration: input.bundleGeneration,
        attest: input.attest.ffi,
        logStatus: input.log.status.ffi,
        logIsReset: input.log.isReset,
        seal: FfiSealCheck(
            valid: input.seal.valid, announcementSeq: input.seal.announcementSeq, kind: nil,
            refusal: input.seal.refusal),
        sealLog: input.seal.log))
    switch answer {
    case .sealed(let keyGeneration, let signedAtMs, let announcementSeq):
        return .sealed(keyGeneration: keyGeneration, signedAtMs: signedAtMs, announcementSeq: announcementSeq)
    case .unsealed(let keyGeneration, let signedAtMs):
        return .unsealed(keyGeneration: keyGeneration, signedAtMs: signedAtMs)
    case .refused(let why):
        return .refused(why == .unexplained ? .unexplained : .unconfirmed)
    case .noVerdict:
        return .noVerdict
    }
}

/// The rotation marker of the re-pin (R.2 "The re-pin"): never backwards, in
/// ADR-0016's marker space (seconds).
public func resetRepinMarker(heldMarker: Int64, nowMs: Int64) -> Int64 {
    max(heldMarker, nowMs / 1000)
}
