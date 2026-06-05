// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation

/// WebAuthn action binding (G-13, ADR-0014) — iOS surface.
///
/// Mirror of the `assertAction` / `actionHashFor*` additions in
/// `apps/web/src/auth/webauthn-client.ts`.
///
/// Action binding elevates a *specific* sensitive operation to
/// require a fresh passkey gesture: the server issues a challenge
/// that is a function of an opaque 32-byte `action_hash`, so the
/// authenticator's signature attests to THAT action. The ceremony
/// is exactly the passkey-assertion (login) flow against two
/// different endpoints — `/auth/action/start` and
/// `/auth/action/finish` — with an extra `action_hash_b64` field
/// in both request bodies.
///
/// The caller computes `action_hash` with one of the
/// ``actionHashForDeleteCredential(credentialIdB64:)`` /
/// ``actionHashForRemoveDevice(signaturePublicKey:)`` helpers and
/// runs the ceremony as a gate *before* performing the operation.
/// A throw (in particular a user-cancelled gesture) MUST be
/// treated as "do not perform the action" — fail-closed.

// MARK: - Domain separators

/// Domain separator bound into the `action_hash` for deleting a
/// passkey credential. Kept in lockstep with the web / Android
/// clients so the stored hash means the same thing on every
/// platform.
public let actionDomainDeleteCredential = "lakko/action/delete-credential/v1"

/// Domain separator bound into the `action_hash` for removing a
/// DeviceGroup leaf.
public let actionDomainRemoveDevice = "lakko/action/remove-device/v1"

// MARK: - Wire shapes (mirror server-side `auth/action`)

/// Body of `POST /auth/action/start`.
public struct ActionStartRequest: Encodable {
    /// URL-safe base64 of the 32-byte action hash.
    public let action_hash_b64: String
}

/// Response from `POST /auth/action/start`. `options` carries the
/// identical `RequestChallengeResponse` shape `/webauthn/auth/start`
/// returns — the assertion-request options the authenticator signs.
public struct ActionStartResponse: Decodable {
    public let state_id: String
    public let options: ServerRequestOptions
}

/// Body of `POST /auth/action/finish`.
public struct ActionFinishRequest: Encodable {
    public let state_id: String
    public let action_hash_b64: String
    public let credential: AuthPublicKeyCredentialJSON
}

/// Response from `POST /auth/action/finish`.
public struct ActionFinishResponse: Decodable {
    public let account_id_b64: String
    /// Echo of the action hash the passkey attested to.
    public let action_hash_b64: String
    public let credential_id_b64: String
}

// MARK: - Ceremony

extension WebauthnClient {

    /// Drive the action-binding ceremony (G-13, ADR-0014):
    ///
    ///   1. `POST /auth/action/start` with the opaque
    ///      `actionHashB64`. Server derives
    ///      `challenge = H(action_hash || nonce)` and returns
    ///      `(state_id, options)`.
    ///   2. `ASAuthorizationController` assertion — the user
    ///      performs the passkey gesture, signing over the
    ///      action-bound challenge.
    ///   3. `POST /auth/action/finish` with the same
    ///      `action_hash_b64` and the asserted credential.
    ///      Resolves only if the server verifies the signature
    ///      attests to this action.
    ///
    /// Bearer-auth required (the caller sets the session token via
    /// ``setSessionToken(_:)``). Throws the same error family as
    /// ``loginWithPasskey()`` — in particular
    /// ``WebauthnAssertion/AssertionError/cancelled`` when the user
    /// dismisses the sheet, so the caller MUST treat a throw as
    /// "do not perform the action".
    ///
    /// A ``WebauthnClientError/http(status:endpoint:body:)`` with
    /// `status == 404` on the `/auth/action/start` call signals the
    /// account has no passkey credential (the server's
    /// `NoCredentials` path). The device-removal gate branches on
    /// that to proceed on session-token auth alone.
    ///
    /// Must run on `@MainActor` so the embedded
    /// `ASAuthorizationController` can present the system sheet via
    /// ``currentPresentationAnchor()``.
    @MainActor
    public func assertAction(actionHashB64: String) async throws -> ActionFinishResponse {
        let start: ActionStartResponse = try await postJsonAuthed(
            endpoint: "/auth/action/start",
            body: ActionStartRequest(action_hash_b64: actionHashB64)
        )
        let publicKey = start.options.publicKey
        let challenge = try base64UrlDecode(publicKey.challenge)
        guard let rpId = publicKey.rpId else {
            throw WebauthnClientError.argument(
                "server omitted rp_id in /auth/action/start response"
            )
        }
        let allowedCredentials: [Data] = try (publicKey.allowCredentials ?? [])
            .map { try base64UrlDecode($0.id) }
        let credentialJSON = try await WebauthnAssertion().run(
            rpId: rpId,
            challenge: challenge,
            allowedCredentials: allowedCredentials,
            presentationAnchor: currentPresentationAnchor()
        )
        let finish: ActionFinishResponse = try await postJsonAuthed(
            endpoint: "/auth/action/finish",
            body: ActionFinishRequest(
                state_id: start.state_id,
                action_hash_b64: actionHashB64,
                credential: credentialJSON
            )
        )
        return finish
    }
}

// MARK: - action_hash derivation

/// `action_hash = SHA-256(domain_utf8 || identifier)`, URL-safe
/// base64 without padding. SHA-256 is native everywhere
/// (WebCrypto / CryptoKit / MessageDigest), so the same
/// convention computes identically on every client. The server
/// treats the result as opaque 32 bytes.
private func deriveActionHash(domain: String, identifier: Data) -> String {
    var input = Data(domain.utf8)
    input.append(identifier)
    let digest = SHA256.hash(data: input)
    return base64UrlEncode(Data(digest))
}

/// Action hash for deleting the passkey credential identified by
/// `credentialIdB64` (the value ``WebauthnClient/listCredentials()``
/// returns). The identifier is the credential id *bytes* (the
/// url-safe-base64 string decoded).
public func actionHashForDeleteCredential(credentialIdB64: String) throws -> String {
    let idBytes = try base64UrlDecode(credentialIdB64)
    return deriveActionHash(domain: actionDomainDeleteCredential, identifier: idBytes)
}

/// Action hash for removing the DeviceGroup leaf whose MLS
/// signature public key is `signaturePublicKey`.
public func actionHashForRemoveDevice(signaturePublicKey: Data) -> String {
    deriveActionHash(domain: actionDomainRemoveDevice, identifier: signaturePublicKey)
}
