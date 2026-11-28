// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// UX `identity-recovery.md` 19.1b — setting up an authenticator app, the
// same screen on the sign-up step (19.1a) and in Settings > Security >
// Recovery. This file is the pure part: how the key is written, when a
// typed code is checked, and what each answer of the two routes
// (`/totp/enroll/start`, `/totp/enroll/finish`) leaves on the screen.
// The model and the screen are in `UI/Settings/AuthenticatorSetupScreen.swift`.

public enum AuthenticatorSetup {

    /// What the screen shows.
    public enum Phase: Equatable {
        /// No proof is live (Settings only): the 17.1 block, then the
        /// button that starts.
        case needsProof
        /// The key is being asked for: a spinner where the QR code goes.
        case starting
        /// The QR code, the key and the code field.
        case ready
        /// The six digits are being checked: the field is disabled.
        case checking
        case done
        /// `409`: the account already has an authenticator.
        case alreadyEnabled
        /// Any other failure: the line, and `Try again`, which starts over
        /// with a new key.
        case failed
    }

    /// What a failed call leaves.
    public enum Failure: Equatable {
        /// `401` from the finish: a wrong, expired or already used code.
        case wrongCode
        /// `409` from either call.
        case alreadyEnabled
        /// `403 fresh-auth-required`: the proof ran out. Nothing is wrong;
        /// the block appears and the setup starts over.
        case proofNeeded
        /// `429`, with the wait when the server named one.
        case limited(retryAfterSecs: Int?)
        case offline
        case failed
    }

    public static let wrongCodeKey = "settings.recovery.totp.error.wrong"
    public static let genericKey = "settings.recovery.totp.error.generic"
    public static let offlineKey = "common.error.offline"
    public static let alreadyEnabledKey = "settings.recovery.totp.already_enabled"
    public static let doneKey = "settings.recovery.totp.done"

    /// The digits of a code.
    public static let codeLength = 6

    /// The key in groups of four, as an authenticator app shows it.
    public static func groupedKey(_ secret: String) -> String {
        let characters = Array(secret.filter { !$0.isWhitespace })
        var groups: [String] = []
        var index = 0
        while index < characters.count {
            groups.append(String(characters[index..<min(index + 4, characters.count)]))
            index += 4
        }
        return groups.joined(separator: " ")
    }

    /// What the field keeps of what was typed or pasted: ASCII digits only,
    /// six at most (a pasted "123 456" is the code 123456).
    public static func digits(_ typed: String) -> String {
        String(typed.filter { $0.isASCII && $0.isNumber }.prefix(codeLength))
    }

    /// The code to check, once the sixth digit is in; `nil` before.
    public static func completeCode(_ typed: String) -> String? {
        let code = digits(typed)
        return code.count == codeLength ? code : nil
    }

    /// The answer of a route by its status alone (`403
    /// fresh-auth-required` is read from the error, in ``failure(_:)``).
    public static func failure(status: Int, body: String) -> Failure {
        switch status {
        case 401: return .wrongCode
        case 409: return .alreadyEnabled
        case 429:
            return .limited(
                retryAfterSecs: AuthRateLimit.refusal(status: status, body: body)?.retryAfterSecs)
        default: return .failed
        }
    }

    public static func failure(_ error: Error) -> Failure {
        if FreshAuthRefusal.isRequired(error) { return .proofNeeded }
        switch error as? AuthClientError {
        case .http(let status, _, let body): return failure(status: status, body: body)
        case .network: return .offline
        default: return .failed
        }
    }
}
