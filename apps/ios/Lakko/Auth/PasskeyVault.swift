// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// A passkey that restores keys: the pure rules. Mirror of
/// `apps/web/src/recovery/passkey-vault.ts`.
///
/// A passkey signs the person in; with the WebAuthn PRF extension it can
/// also carry the account's keys. The PRF output for a fixed salt is a
/// stable 32-byte secret per credential that follows a synced passkey to
/// every device the passkey syncs to. The client seals the phrase entropy
/// under a key derived from it (`passkeyVaultSeal` in the core) and stores
/// the blob server-side per credential; a passkey sign-in on a new device
/// asks for the same PRF evaluation, fetches the blob, opens it into the
/// full 12-word phrase and hands it to the ordinary phrase path, which
/// checks it against the account's published identity (fail-closed).
///
/// Storing the blob is OPT-IN per passkey: key safety then also rests on
/// whoever syncs the passkey (Apple, Google, a password manager), so the
/// person decides on the informed-trust screen. Nothing here enrolls on its
/// own. The PRF output never leaves this device: it is read from the
/// platform's result and is not part of the credential JSON the server
/// receives.
///
/// Platform support is uneven. No PRF output means the passkey signs in
/// only, and the UI says so.
enum PasskeyVault {

    /// The PRF salt every Lakko client evaluates. Fixed by design: the
    /// output is already per-credential, and a per-account salt would have
    /// to be fetched before the login that needs it.
    static let prfSalt = Data("lakko/passkey-vault/prf/v1".utf8)

    /// The only PRF output length accepted.
    static let prfOutputLength = 32

    /// The sealed blob the server stores per credential is exactly this
    /// long.
    static let sealedEntropyLength = 44

    /// `output` when it is a usable PRF output (32 bytes), else `nil`.
    static func usablePrfOutput(_ output: Data?) -> Data? {
        guard let output, output.count == prfOutputLength else { return nil }
        return output
    }

    /// What a passkey sign-in on a device without the account's keys found,
    /// which decides the first line of the restore prompt.
    enum KeysOutcome: Equatable {
        /// The blob opened and the phrase matched the published identity.
        case restored
        /// No PRF output, or no blob for this passkey: it signs in only.
        case noKeys
        /// A blob exists but did not open, or the phrase it gave is not the
        /// account's: nothing was changed.
        case unusable
    }

    /// The outcome for what the ceremony and the vault returned. `opened` is
    /// whether the blob opened into a phrase; `matched` is whether that
    /// phrase passed the published-identity check.
    static func keysOutcome(prfOutput: Bool, blobStored: Bool, opened: Bool, matched: Bool) -> KeysOutcome {
        if !prfOutput || !blobStored { return .noKeys }
        return opened && matched ? .restored : .unusable
    }

    /// What follows creating a passkey.
    enum AddOutcome: Equatable {
        /// The platform can protect keys with this passkey: show the choice.
        case offerKeysChoice
        /// It cannot: the passkey signs in only, and the result line says so.
        case signInOnly
    }

    /// Whether the choice screen follows a registration. `prfOutput` is
    /// whether the registration already yielded the PRF output;
    /// `prfSupported` is what the platform reported for the new credential
    /// (`true`: supported, the output comes when the passkey is next used;
    /// `false` or `nil`: not supported, or the platform said nothing).
    static func addOutcome(prfOutput: Bool, prfSupported: Bool?) -> AddOutcome {
        prfOutput || prfSupported == true ? .offerKeysChoice : .signInOnly
    }

    /// What a passkey row says in Settings.
    enum KeysLabel: Equatable {
        case restoresKeys
        case signInOnly

        var catalogKey: String {
            switch self {
            case .restoresKeys: return "settings.passkeys.label.restores_keys"
            case .signInOnly: return "settings.passkeys.label.signin_only"
            }
        }
    }

    /// The label for one passkey: it restores keys exactly when the server
    /// holds a blob for its credential id. `withKeys` is `GET
    /// /vault/passkeys`; `nil` (not loaded, or the server has no vault key)
    /// reads as sign-in only, which is what such a passkey can do.
    static func keysLabel(credentialIdB64: String, withKeys: [String]?) -> KeysLabel {
        withKeys?.contains(credentialIdB64) == true ? .restoresKeys : .signInOnly
    }

    /// Whether the restore prompt lists the passkey row: passkeys are
    /// supported on this device, and the account has a passkey that carries
    /// keys (`passkeysWithKeys` is the count from `GET /vault/passkeys`;
    /// `nil` while it is not known, which keeps the row).
    static func restoreOffersPasskey(passkeysSupported: Bool, passkeysWithKeys: Int?) -> Bool {
        passkeysSupported && (passkeysWithKeys == nil || (passkeysWithKeys ?? 0) > 0)
    }
}
