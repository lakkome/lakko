// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AuthenticationServices
import Foundation
import UIKit

/// `WebauthnAssertion` — async-throws wrapper around
/// `ASAuthorizationController` for the Phase 2a2c2-ios passkey
/// **authentication** flow.
///
/// Mirror of ``WebauthnRegistration`` for the assertion half:
///
///   * ``WebauthnRegistration.run`` drives a
///     `createCredentialRegistrationRequest` (passkey enrolment).
///   * ``WebauthnAssertion.run`` drives a
///     `createCredentialAssertionRequest` (passkey login).
///
/// Both bridge the delegate-style `ASAuthorizationController` API
/// to a single async call site via a continuation. Both require
/// iOS 16+ (`ASAuthorizationPlatformPublicKeyCredentialProvider`)
/// and an `apple-app-site-association` `webcredentials:<rp_id>`
/// binding in production.
@MainActor
public final class WebauthnAssertion: NSObject {

    public enum AssertionError: Error, LocalizedError {
        case cancelled
        case noCredential
        case underlying(Error)

        public var errorDescription: String? {
            switch self {
            case .cancelled:
                return "Cancelled. No passkey sign-in happened."
            case .noCredential:
                return "No passkey credential was returned."
            case .underlying(let err):
                return err.localizedDescription
            }
        }
    }

    /// Drive a single passkey assertion round.
    ///
    /// `allowedCredentials` is the list of credential ids the
    /// server's `/webauthn/auth/start` returned in
    /// `options.publicKey.allowCredentials`. When omitted (the
    /// discoverable-credential flow), the authenticator picks
    /// any passkey it has for the relying party.
    public func run(
        rpId: String,
        challenge: Data,
        allowedCredentials: [Data],
        presentationAnchor: ASPresentationAnchor
    ) async throws -> AuthPublicKeyCredentialJSON {
        let provider = ASAuthorizationPlatformPublicKeyCredentialProvider(
            relyingPartyIdentifier: rpId
        )
        let request = provider.createCredentialAssertionRequest(
            challenge: challenge
        )
        if !allowedCredentials.isEmpty {
            request.allowedCredentials = allowedCredentials.map {
                ASAuthorizationPlatformPublicKeyCredentialDescriptor(credentialID: $0)
            }
        }
        let controller = ASAuthorizationController(authorizationRequests: [request])

        return try await withCheckedThrowingContinuation { continuation in
            let bridge = ControllerBridge(
                anchor: presentationAnchor,
                continuation: continuation
            )
            controller.delegate = bridge
            controller.presentationContextProvider = bridge
            bridge.attach(controller: controller)
            controller.performRequests()
        }
    }
}

private final class ControllerBridge: NSObject,
    ASAuthorizationControllerDelegate,
    ASAuthorizationControllerPresentationContextProviding
{
    private let anchor: ASPresentationAnchor
    private var continuation:
        CheckedContinuation<AuthPublicKeyCredentialJSON, Error>?
    private var retainedController: ASAuthorizationController?

    init(
        anchor: ASPresentationAnchor,
        continuation: CheckedContinuation<AuthPublicKeyCredentialJSON, Error>
    ) {
        self.anchor = anchor
        self.continuation = continuation
    }

    func attach(controller: ASAuthorizationController) {
        self.retainedController = controller
    }

    func presentationAnchor(
        for _: ASAuthorizationController
    ) -> ASPresentationAnchor {
        anchor
    }

    func authorizationController(
        controller: ASAuthorizationController,
        didCompleteWithAuthorization authorization: ASAuthorization
    ) {
        defer {
            self.continuation = nil
            self.retainedController = nil
        }
        guard
            let assertion = authorization.credential
                as? ASAuthorizationPlatformPublicKeyCredentialAssertion
        else {
            self.continuation?.resume(
                throwing: WebauthnAssertion.AssertionError.noCredential
            )
            return
        }
        let credentialIdB64 = base64UrlEncode(assertion.credentialID)
        let userHandleB64: String? = assertion.userID
            .map(base64UrlEncode)
        let json = AuthPublicKeyCredentialJSON(
            id: credentialIdB64,
            rawId: credentialIdB64,
            response: AuthPublicKeyCredentialJSON.Response(
                clientDataJSON: base64UrlEncode(assertion.rawClientDataJSON),
                authenticatorData: base64UrlEncode(assertion.rawAuthenticatorData),
                signature: base64UrlEncode(assertion.signature),
                userHandle: userHandleB64
            ),
            extensions: nil
        )
        self.continuation?.resume(returning: json)
    }

    func authorizationController(
        controller: ASAuthorizationController,
        didCompleteWithError error: Error
    ) {
        defer {
            self.continuation = nil
            self.retainedController = nil
        }
        if let asError = error as? ASAuthorizationError {
            switch asError.code {
            case .canceled:
                self.continuation?.resume(
                    throwing: WebauthnAssertion.AssertionError.cancelled
                )
                return
            default:
                break
            }
        }
        self.continuation?.resume(
            throwing: WebauthnAssertion.AssertionError.underlying(error)
        )
    }
}
