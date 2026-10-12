// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0065 "Phase 2 revised: silent continuity", R.2 - the acceptance rule
// for a contact's account-key RESET, as one pure decision.
//
// iOS mirror of `apps/web/src/contacts/reset-verdict.ts`; the table is the
// same and so are the cases in `ResetVerdictTests`.
//
// A reset is a key change with NO supersession proof (with one, ADR-0016's
// path runs and this file is never asked: a proven rotation is not a reset).
// It is accepted, silently, when the directory serves an auth-signed
// key-reset attestation for exactly this account and this key, at a
// generation strictly above the one this device last accepted, and the key
// log (when one is configured) agrees. Anything negative is the existing
// fail-closed refusal; anything not yet known is no verdict at all.
//
// Free of the core, storage and the network: `ContactsClient` gathers the
// inputs and acts on the outcome.

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

/// Which refusal copy the action shows (`contacts.identity_changed.*`).
public enum ResetRefusal: Equatable {
    /// The key log shows the new key and does NOT record a reset.
    case unexplained
    /// Everything else that is negative.
    case unconfirmed
}

public enum ResetVerdict: Equatable {
    case accepted(keyGeneration: UInt64, signedAtMs: Int64)
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

    public init(
        heldKeyB64: String,
        acceptedGeneration: UInt64,
        fetchedKeyB64: String,
        bundleGeneration: UInt64,
        attest: AttestCheck,
        log: LogCheck
    ) {
        self.heldKeyB64 = heldKeyB64
        self.acceptedGeneration = acceptedGeneration
        self.fetchedKeyB64 = fetchedKeyB64
        self.bundleGeneration = bundleGeneration
        self.attest = attest
        self.log = log
    }
}

/// The refusal copy for a negative verdict: `unexplained` only when the log
/// itself, read to the end, shows the key without a reset.
private func refusal(for log: LogCheck) -> ResetVerdict {
    .refused(log.status == .confirmed && log.isReset == false ? .unexplained : .unconfirmed)
}

/// R.2, in the order the checks decide:
///
/// | # | attestation | generation                  | key log                        | verdict                 |
/// |---|-------------|-----------------------------|--------------------------------|-------------------------|
/// | 1 | absent      | -                           | any                            | refused                 |
/// | 2 | invalid     | -                           | any                            | refused                 |
/// | 3 | no-key      | -                           | any                            | no-verdict              |
/// | 4 | verified    | != bundle's, or <= accepted | any                            | refused                 |
/// | 5 | verified    | ok                          | confirmed, `is_reset` false    | refused (`unexplained`) |
/// | 6 | verified    | ok                          | mismatch / rollback            | refused                 |
/// | 7 | verified    | ok                          | unreachable / absent           | no-verdict              |
/// | 8 | verified    | ok                          | confirmed + `is_reset`, or off | accepted                |
///
/// A refusal's copy is `unexplained` exactly when the log is `confirmed`
/// without `is_reset`, `unconfirmed` otherwise. Row 8's `off` arm is decision
/// D3 (``logOffAcceptsReset()``).
///
/// Callers must not ask for an unchanged key; one that does is answered
/// `noVerdict` (nothing to accept, nothing to refuse).
public func resetVerdict(_ input: ResetVerdictInput) -> ResetVerdict {
    if input.fetchedKeyB64 == input.heldKeyB64 { return .noVerdict }
    let keyGeneration: UInt64
    let signedAtMs: Int64
    switch input.attest {
    case .absent, .invalid:
        return refusal(for: input.log)
    case .noKey:
        return .noVerdict
    case .verified(let generation, let signed):
        keyGeneration = generation
        signedAtMs = signed
    }
    if keyGeneration != input.bundleGeneration || keyGeneration <= input.acceptedGeneration {
        return refusal(for: input.log)
    }
    let accepted = ResetVerdict.accepted(keyGeneration: keyGeneration, signedAtMs: signedAtMs)
    switch input.log.status {
    case .confirmed:
        return input.log.isReset == true ? accepted : refusal(for: input.log)
    case .off:
        return logOffAcceptsReset() ? accepted : .refused(.unconfirmed)
    case .mismatch, .rollback:
        return refusal(for: input.log)
    case .unreachable, .absent:
        return .noVerdict
    }
}

/// Decision D3 (R.10): with no key log configured in the build, a reset is
/// accepted on the attestation alone. `false` would refuse every reset until
/// the log is deployed.
public func logOffAcceptsReset() -> Bool {
    true
}

/// The rotation marker of the re-pin (R.2 "The re-pin"): never backwards, in
/// ADR-0016's marker space (seconds).
public func resetRepinMarker(heldMarker: Int64, nowMs: Int64) -> Int64 {
    max(heldMarker, nowMs / 1000)
}
