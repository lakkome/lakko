// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
@testable import Lakko

// ADR-0073 — the session pair in the tests that predate it: an access token
// alone (no refresh credential), which the provider never refreshes and the
// restore accepts while the token is live.

extension PersistedSessionToken {
    init(sessionToken: String, sessionExpiresAtMs: Int64) {
        self.init(pair: .accessOnly(sessionToken: sessionToken, sessionExpiresAtMs: sessionExpiresAtMs))
    }
}

extension IdentitySecrets {
    init(accountId: Data, exportKey: Data, accountIdentitySeed: Data, sessionToken: String, sessionExpiresAtMs: Int64) {
        self.init(
            accountId: accountId, exportKey: exportKey, accountIdentitySeed: accountIdentitySeed,
            session: .accessOnly(sessionToken: sessionToken, sessionExpiresAtMs: sessionExpiresAtMs))
    }
}

extension SignupResult {
    init(accountId: Data, exportKey: Data, recoveryPhrase: String? = nil, sessionToken: String, sessionExpiresAtMs: Int64) {
        self.init(
            accountId: accountId, exportKey: exportKey, recoveryPhrase: recoveryPhrase,
            session: .accessOnly(sessionToken: sessionToken, sessionExpiresAtMs: sessionExpiresAtMs))
    }
}

extension LoginResult {
    init(accountId: Data, sessionKey: Data, exportKey: Data, sessionToken: String, sessionExpiresAtMs: Int64) {
        self.init(
            accountId: accountId, sessionKey: sessionKey, exportKey: exportKey,
            session: .accessOnly(sessionToken: sessionToken, sessionExpiresAtMs: sessionExpiresAtMs))
    }
}

extension PasskeyLoginResult {
    init(accountIdB64: String, sessionToken: String, sessionExpiresAtMs: Int64, credentialId: Data = Data(), prfOutput: Data? = nil) {
        self.init(
            accountIdB64: accountIdB64,
            session: .accessOnly(sessionToken: sessionToken, sessionExpiresAtMs: sessionExpiresAtMs),
            credentialId: credentialId, prfOutput: prfOutput)
    }
}

extension SessionPair {
    /// A full pair for the tests of ADR-0073 itself.
    static func fixture(
        access: String = "tok-access",
        accessExpiresAtMs: Int64 = 2_000_000,
        refresh: String = "r1.refresh",
        refreshExpiresAtMs: Int64 = 9_000_000_000,
        generation: UInt64 = 1
    ) -> SessionPair {
        SessionPair(
            sessionToken: access, sessionExpiresAtMs: accessExpiresAtMs,
            refreshToken: refresh, refreshExpiresAtMs: refreshExpiresAtMs,
            keyGeneration: generation)
    }
}
