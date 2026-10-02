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
    /// Whether the keys choice follows, or the passkey signs in only.
    let outcome: PasskeyVault.AddOutcome
}

/// Adding a passkey to the signed-in account: the one sequence behind
/// Settings > Security > Passkeys and the passkey offer at the end of
/// signup. Mirror of `apps/web/src/auth/passkey-add.ts`.
///
/// It registers the passkey (asking for the PRF evaluation), turns on
/// attested device removals (G-13, ADR-0015), and says whether the keys
/// choice can follow. Nothing is stored for the passkey here: letting it
/// restore keys is the person's own choice (``PasskeyKeysChoiceView``).
@MainActor
enum PasskeyAdder {

    static func add(
        client: WebauthnClient,
        registration: WebauthnRegistration,
        userName: String,
        nickname: String,
        account: (accountIdBytes: Data, accountIdB64: String)?,
        presentationAnchor: ASPresentationAnchor
    ) async throws -> AddedPasskey {
        let start = try await client.startRegistration(userName: userName, nickname: nickname)
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
            credential: registered.credential
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
            outcome: PasskeyVault.addOutcome(
                prfOutput: registered.prfOutput != nil,
                prfSupported: registered.prfSupported
            )
        )
    }
}

/// How the keys choice ended.
enum KeysChoiceResult: Equatable {
    /// The keys are stored for the passkey.
    case stored
    /// The person chose sign-in only; nothing was stored.
    case signInOnly
    /// The passkey gave no secret to protect keys with.
    case noPrf
    /// The server does not store keys for passkeys.
    case unavailable

    /// The catalog key of the result line, or `nil` when there is nothing
    /// to say (the person chose sign-in only).
    var messageKey: String? {
        switch self {
        case .stored: return "settings.passkeys.keys_choice.done"
        case .signInOnly: return nil
        case .noPrf: return "settings.passkeys.keys_choice.no_prf"
        case .unavailable: return "settings.passkeys.keys_choice.unavailable"
        }
    }
}

/// The ports "Turn on" drives, so the sequence can be tested without a
/// system sheet or a server.
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

    enum Step: Equatable {
        /// The phrase is not on this device: ask for the 12 words.
        case needsPhrase
        case done(KeysChoiceResult)
        case refused(String)
    }

    /// "Turn on": store the keys for this passkey. Every refusal leaves the
    /// passkey "Signs in only".
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
        guard let prfOutput else { return .done(.noPrf) }
        guard let credentialId = base64UrlDecodeNoPad(credentialIdB64) else {
            return .refused("settings.passkeys.error.generic")
        }

        let sealed = try passkeyVaultSeal(mnemonic: mnemonic, prfOutput: prfOutput)
        return .done(try await store(credentialId, sealed) ? .stored : .unavailable)
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
                DevicePhrase.store(phrase, accountIdB64: account.accountIdB64, context: "passkey-keys-choice")
            }
        )
    }
}

/// "Let this passkey restore your keys?": the choice shown after a passkey
/// is added, and from a passkey's row later. Used by Settings > Security >
/// Passkeys and by the passkey offer at the end of signup.
///
/// Storing the keys for a passkey is OPT-IN, per passkey, and informed: the
/// safety of the keys then also rests on whoever syncs the passkey. Nothing
/// is stored unless the person presses "Turn on"; the two buttons carry the
/// same weight and neither is the default.
struct PasskeyKeysChoiceView: View {
    let credentialIdB64: String
    let nickname: String
    /// What the registration yielded, when it yielded it.
    let prfOutput: Data?
    let enrollment: PasskeyKeysEnrollment
    let onDone: (KeysChoiceResult) -> Void

    @State private var askingPhrase = false
    @State private var typedPhrase = ""
    @State private var busy = false
    @State private var error: UiText?
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(l10n.t("settings.passkeys.keys_choice.title"))
                .font(.headline)
                .foregroundStyle(Color.lakkoText)
            if !nickname.isEmpty {
                Text(nickname)
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            Text(l10n.t("settings.passkeys.keys_choice.body_on"))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
            Text(l10n.t("settings.passkeys.keys_choice.body_off"))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
            Text(l10n.t("settings.passkeys.keys_choice.later"))
                .font(.footnote)
                .foregroundStyle(Color.lakkoTextMuted)

            if askingPhrase {
                Text(l10n.t("settings.passkeys.keys_choice.phrase_needed"))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoText)
                PhraseEntryGrid(phrase: $typedPhrase, disabled: busy)
            }
            if let error {
                AuthHint(text: error.text, isError: true)
            }

            // Same weight, no default: the person must pick one.
            HStack(spacing: 12) {
                AuthSecondaryButton(
                    title: l10n.t(busy ? "common.checking" : "settings.passkeys.keys_choice.turn_on"),
                    disabled: busy,
                    action: turnOn
                )
                AuthSecondaryButton(
                    title: l10n.t("settings.passkeys.keys_choice.signin_only"),
                    disabled: busy,
                    action: { onDone(.signInOnly) }
                )
            }
        }
        .padding(16)
        .background(Color.lakkoSurface)
        .clipShape(RoundedRectangle(cornerRadius: LakkoRadius.card))
        .overlay(
            RoundedRectangle(cornerRadius: LakkoRadius.card)
                .stroke(Color.lakkoBorder, lineWidth: 1)
        )
        .accessibilityElement(children: .contain)
    }

    private func turnOn() {
        guard !busy else { return }
        busy = true
        error = nil
        Task {
            defer { busy = false }
            do {
                let step = try await enrollment.turnOn(
                    credentialIdB64: credentialIdB64,
                    registrationPrfOutput: prfOutput,
                    typedPhrase: askingPhrase ? typedPhrase : nil
                )
                switch step {
                case .needsPhrase:
                    askingPhrase = true
                case .refused(let key):
                    error = .key(key)
                case .done(let result):
                    onDone(result)
                }
            } catch {
                if AuthErrorCopy.isPasskeyCancelled(error) {
                    // Cancelled in the system sheet: the passkey stays
                    // sign-in only.
                    self.error = .key("settings.passkeys.keys_choice.cancelled")
                } else {
                    NSLog("[passkeys] WARN turning on restoring keys failed")
                    self.error = .key("settings.passkeys.error.generic")
                }
            }
        }
    }
}
