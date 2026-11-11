// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AuthenticationServices
import LakkoCore
import SwiftUI

/// A passkey just added to the signed-in account.
struct AddedPasskey: Equatable {
    /// URL-safe base64 (no padding) of the credential id, as the server
    /// lists it.
    let credentialIdB64: String
    let nickname: String
    /// What the registration yielded for protecting keys, when it did.
    let prfOutput: Data?
    /// What creation reported for the new credential's PRF support:
    /// `false` an explicit "cannot carry keys" (also a system older than
    /// iOS 18, which has no PRF API), `nil` nothing said.
    let prfSupported: Bool?

    /// Whether the platform can protect keys with this passkey at all.
    var canCarryKeys: Bool {
        PasskeyVault.canCarryKeys(prfOutput: prfOutput != nil, prfSupported: prfSupported)
    }
}

/// Adding a passkey to the signed-in account: the one sequence behind
/// Settings > Security > Passkeys and the passkey offer at the end of
/// signup.
///
/// It registers the passkey (asking for the PRF evaluation) and turns on
/// attested device removals (G-13, ADR-0015). Storing the keys for the new
/// passkey follows with nothing to choose
/// (``PasskeyKeysEnrollment/storeAfterAdd(_:)``, UX
/// `docs/ux/identity-recovery.md` 10.2).
@MainActor
enum PasskeyAdder {

    static func add(
        client: WebauthnClient,
        registration: WebauthnRegistration,
        userName: String,
        nickname: String,
        account: (accountIdBytes: Data, accountIdB64: String)?,
        freshGrant: String,
        presentationAnchor: ASPresentationAnchor
    ) async throws -> AddedPasskey {
        let start = try await client.startRegistration(
            userName: userName, nickname: nickname, freshGrant: freshGrant)
        let challenge = try base64UrlDecode(start.options.publicKey.challenge)
        let userId = try base64UrlDecode(start.options.publicKey.user.id)
        let registered = try await registration.run(
            rpId: start.options.publicKey.rp.id,
            challenge: challenge,
            userName: start.options.publicKey.user.name,
            userID: userId,
            prfSalt: PasskeyVault.prfSalt,
            presentationAnchor: presentationAnchor
        )
        let finish = try await client.finishRegistration(
            stateId: start.state_id,
            nickname: start.nickname,
            credential: registered.credential,
            freshGrant: freshGrant
        )
        // G-13 (ADR-0015): now that the account has a passkey, enable
        // attested removals so device removals require a peer-verified
        // gesture. Non-fatal: it can be enabled on a later passkey action.
        if let account {
            do {
                try await DeviceRemoval.enableAttestedRemovalsIfNeeded(
                    accountIdBytes: account.accountIdBytes,
                    accountIdB64: account.accountIdB64
                )
            } catch {
                NSLog("failed to enable attested removals: %@", error.localizedDescription)
            }
        }
        return AddedPasskey(
            credentialIdB64: finish.credential_id_b64,
            nickname: nickname,
            prfOutput: registered.prfOutput,
            prfSupported: PasskeyVault.reportedPrfSupport(
                registrationReported: registered.prfSupported,
                prfApiAvailable: PasskeysViewModel.prfApiAvailable)
        )
    }
}

/// How storing the keys for a passkey ended.
enum KeysStoreResult: Equatable {
    /// The keys are stored for the passkey.
    case stored
    /// The passkey gave no secret to protect keys with.
    case noPrf
    /// The server does not store keys for passkeys.
    case unavailable
}

/// The ports storing the keys for a passkey drives, so the sequence can be
/// tested without a system sheet or a server.
struct PasskeyKeysEnrollment {
    /// The phrase stored on this device for the signed-in account, or `nil`.
    var devicePhrase: () -> String?
    /// The 64-byte identity seed this device holds for the account.
    var identitySeed: () -> Data?
    /// One more passkey prompt, limited to the credential: its id and PRF
    /// output.
    var evaluatePrf: (_ credentialIdB64: String) async throws -> (credentialId: Data, prfOutput: Data?)
    /// `PUT /vault/passkey`. `false` when the server has no vault key.
    var store: (_ credentialId: Data, _ sealed: Data) async throws -> Bool
    /// Keep a typed phrase that proved to be the account's on this device.
    var rememberPhrase: (_ phrase: String) -> Void
    /// Remember on this device that the passkey cannot carry keys, so its
    /// row stops offering the repair.
    var rememberNoPrf: (_ credentialIdB64: String) -> Void = { _ in }

    enum Step: Equatable {
        /// The phrase is not on this device: ask for the 12 words.
        case needsPhrase
        case done(KeysStoreResult)
        case refused(String)
    }

    /// Store the keys for this passkey. Every refusal leaves the passkey
    /// "Logs in only".
    ///
    ///   1. The phrase: stored on this device, else typed and checked
    ///      against the identity this device holds (the account's own keys).
    ///   2. The passkey's secret: from the registration, else one more
    ///      passkey prompt for this credential only. It never leaves the
    ///      device.
    ///   3. Seal and store.
    ///
    /// `.refused` carries a catalog key.
    func turnOn(
        credentialIdB64: String,
        registrationPrfOutput: Data?,
        typedPhrase: String?
    ) async throws -> Step {
        let mnemonic: RecoveryMnemonic
        if let stored = devicePhrase(), let parsed = try? RecoveryMnemonic.fromPhrase(phrase: stored) {
            mnemonic = parsed
        } else {
            guard let typedPhrase else { return .needsPhrase }
            guard let parsed = try? RecoveryMnemonic.fromPhrase(
                phrase: typedPhrase.trimmingCharacters(in: .whitespacesAndNewlines)) else {
                return .refused("restore_identity.error.invalid_phrase")
            }
            var seed = parsed.toSeed()
            let same = seed == identitySeed()
            seed.resetBytes(in: 0..<seed.count)
            guard same else {
                return .refused("settings.passkeys.keys_choice.error.phrase_mismatch")
            }
            mnemonic = parsed
            rememberPhrase(parsed.phrase())
        }

        var prfOutput = PasskeyVault.usablePrfOutput(registrationPrfOutput)
        if prfOutput == nil {
            let extras = try await evaluatePrf(credentialIdB64)
            guard base64UrlEncodeNoPad(extras.credentialId) == credentialIdB64 else {
                return .refused("settings.passkeys.keys_choice.error.other_passkey")
            }
            prfOutput = PasskeyVault.usablePrfOutput(extras.prfOutput)
        }
        guard let prfOutput else {
            rememberNoPrf(credentialIdB64)
            return .done(.noPrf)
        }
        guard let credentialId = base64UrlDecodeNoPad(credentialIdB64) else {
            return .refused("settings.passkeys.error.generic")
        }

        let sealed = try passkeyVaultSeal(mnemonic: mnemonic, prfOutput: prfOutput)
        return .done(try await store(credentialId, sealed) ? .stored : .unavailable)
    }

    /// Store the keys for a passkey just added, with nothing to choose (UX
    /// 10.2 steps 2 to 4). Never throws: every failure is an outcome, and
    /// the passkey itself is added whatever it is.
    func storeAfterAdd(_ added: AddedPasskey) async -> PasskeyKeysAddOutcome {
        guard added.canCarryKeys else {
            rememberNoPrf(added.credentialIdB64)
            return .noPrf
        }
        do {
            let step = try await turnOn(
                credentialIdB64: added.credentialIdB64,
                registrationPrfOutput: added.prfOutput,
                typedPhrase: nil
            )
            switch step {
            case .needsPhrase: return .phraseMissing
            case .done(.stored): return .stored
            case .done(.noPrf): return .noPrf
            case .done(.unavailable): return .unavailable
            case .refused("settings.passkeys.keys_choice.error.other_passkey"): return .otherPasskey
            case .refused: return .keysFailed
            }
        } catch {
            return PasskeyKeysAddOutcome.failure(error)
        }
    }
}

/// The line of "Restore keys with this passkey" for a refusal of
/// ``PasskeyKeysEnrollment/turnOn(credentialIdB64:registrationPrfOutput:typedPhrase:)``:
/// a different passkey answering reads "choose this passkey" on a row, where
/// the add flow says "the passkey you just added".
enum PasskeyKeysRepair {
    static func refusalKey(_ key: String) -> String {
        key == "settings.passkeys.keys_choice.error.other_passkey"
            ? "settings.passkeys.keys.error.other_passkey"
            : key
    }
}

/// What adding a passkey ended with, for its result line (UX 10.2).
enum PasskeyKeysAddOutcome: Equatable {
    /// The keys are stored: the row is "Logs in and restores keys".
    case stored
    /// The platform or the passkey provider gave no secret to protect keys
    /// with: "Logs in only", and no repair is offered.
    case noPrf
    /// This device does not hold the recovery phrase: "Logs in only", and
    /// the row offers "Restore keys with this passkey".
    case phraseMissing
    /// The second passkey sheet was dismissed.
    case cancelled
    /// A different passkey answered the second sheet.
    case otherPasskey
    /// `503`: the server does not store keys for passkeys.
    case unavailable
    /// Offline, or another failure after the passkey was created: "Logs in
    /// only for now", and the row offers the repair.
    case keysFailed

    /// The catalog key of the result line.
    var messageKey: String {
        switch self {
        case .stored: return "settings.passkeys.added_restores"
        case .noPrf: return "settings.passkeys.added_no_keys"
        case .phraseMissing: return "settings.passkeys.added"
        case .cancelled: return "settings.passkeys.keys_choice.cancelled"
        case .otherPasskey: return "settings.passkeys.keys_choice.error.other_passkey"
        case .unavailable: return "settings.passkeys.keys_choice.unavailable"
        case .keysFailed: return "settings.passkeys.added_keys_failed"
        }
    }

    /// Whether the line reads as a failure. The passkey is added in every
    /// case; only storing the keys did not happen.
    var isError: Bool {
        switch self {
        case .stored, .noPrf, .phraseMissing: return false
        case .cancelled, .otherPasskey, .unavailable, .keysFailed: return true
        }
    }

    /// The line itself (`settings.passkeys.added` names the passkey).
    func message(nickname: String) -> UiText {
        self == .phraseMissing
            ? .key(messageKey, texts: ["name": nickname])
            : .key(messageKey)
    }

    /// The outcome of a failure thrown while storing.
    static func failure(_ error: Error) -> PasskeyKeysAddOutcome {
        if AuthErrorCopy.isPasskeyCancelled(error) { return .cancelled }
        if !isOffline(error) {
            NSLog("[passkeys] WARN storing the keys for a new passkey failed")
        }
        return .keysFailed
    }

    /// Whether the failure is the network's: the passkey service or the
    /// vault could not be reached.
    static func isOffline(_ error: Error) -> Bool {
        if case .network = error as? WebauthnClientError { return true }
        if case .network = error as? AuthClientError { return true }
        return false
    }
}

extension PasskeyKeysEnrollment {
    /// The enrollment against the live session and server.
    @MainActor
    static func live(authRepository: AuthRepository, client: WebauthnClient) -> PasskeyKeysEnrollment {
        PasskeyKeysEnrollment(
            devicePhrase: { authRepository.devicePhrase() },
            identitySeed: { authRepository.identitySecrets?.accountIdentitySeed },
            evaluatePrf: { credentialIdB64 in
                client.setSessionToken(authRepository.identitySecrets?.sessionToken)
                return try await client.evaluatePrf(
                    credentialIdB64: credentialIdB64, prfSalt: PasskeyVault.prfSalt)
            },
            store: { credentialId, sealed in
                guard let token = authRepository.identitySecrets?.sessionToken else {
                    throw SessionNotReady()
                }
                do {
                    try await AuthClient().passkeyVaultPut(
                        sessionToken: token, credentialId: credentialId, sealedEntropy: sealed)
                    return true
                } catch AuthClientError.http(let status, _, _) where status == 503 {
                    return false
                }
            },
            rememberPhrase: { phrase in
                guard case .session(let account) = authRepository.state else { return }
                DevicePhrase.store(phrase, accountIdB64: account.accountIdB64, context: "passkey-keys", since: nil)
            },
            rememberNoPrf: { credentialIdB64 in
                guard case .session(let account) = authRepository.state else { return }
                PasskeyNoPrfStore().insert(credentialIdB64, accountIdB64: account.accountIdB64)
            }
        )
    }
}
