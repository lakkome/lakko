// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AuthenticationServices
import CryptoKit
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
///
/// ## PRF
///
/// With a `prfSalt` the assertion also asks the passkey for its PRF
/// evaluation (``PasskeyVault``). The platform API for it
/// (`ASAuthorizationPlatformPublicKeyCredentialAssertionRequest.prf` and
/// `ASAuthorizationPlatformPublicKeyCredentialAssertion.prf`) exists from
/// iOS 18.0; below that the salt is ignored and ``Result/prfOutput`` is
/// `nil`. The output is returned to the caller only: it is not part of the
/// credential JSON the server receives.
@MainActor
public final class WebauthnAssertion: NSObject {

    public enum AssertionError: Error, LocalizedError {
        case cancelled
        case noCredential
        case underlying(Error)

        public var errorDescription: String? {
            switch self {
            case .cancelled:
                return Localization.text("auth.passkey.error.cancelled")
            case .noCredential:
                return "No passkey credential was returned."
            case .underlying(let err):
                return err.localizedDescription
            }
        }
    }

    /// What one assertion yields.
    public struct Result {
        /// The credential as the server's finish route takes it. Carries no
        /// extension results: the PRF output never leaves this device.
        public let credential: AuthPublicKeyCredentialJSON
        /// The raw id of the credential that answered.
        public let credentialId: Data
        /// The 32-byte PRF output for the salt asked, or `nil` when none was
        /// asked, the platform is older than iOS 18, or the passkey's
        /// provider evaluated nothing.
        public let prfOutput: Data?
        /// The output for the second salt asked, or `nil`.
        public var secondPrfOutput: Data? = nil
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
        prfSalt: Data? = nil,
        secondPrfSalt: Data? = nil,
        presentationAnchor: ASPresentationAnchor
    ) async throws -> Result {
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
        if let prfSalt, #available(iOS 18.0, *) {
            if let secondPrfSalt {
                request.prf = .inputValues(.init(saltInput1: prfSalt, saltInput2: secondPrfSalt))
            } else {
                request.prf = .inputValues(.saltInput1(prfSalt))
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
        CheckedContinuation<WebauthnAssertion.Result, Error>?
    private var retainedController: ASAuthorizationController?

    init(
        anchor: ASPresentationAnchor,
        continuation: CheckedContinuation<WebauthnAssertion.Result, Error>
    ) {
        self.anchor = anchor
        self.continuation = continuation
    }

    func attach(controller: ASAuthorizationController) {
        self.retainedController = controller
        Self.keepAlive(self)
    }

    /// The bridges whose sheet is still open. `ASAuthorizationController`
    /// holds its delegate and presentation context provider weakly, and
    /// nothing else holds the bridge once `run` has called
    /// `performRequests`, so without this it was released at once and
    /// the sheet's answer resumed nothing ("Adding..." forever, a leaked
    /// continuation; seen on an iPhone 2026-11-11).
    private static let liveLock = NSLock()
    nonisolated(unsafe) private static var live: [ObjectIdentifier: ControllerBridge] = [:]

    private static func keepAlive(_ bridge: ControllerBridge) {
        liveLock.lock()
        live[ObjectIdentifier(bridge)] = bridge
        liveLock.unlock()
    }

    private static func letGo(_ bridge: ControllerBridge) {
        liveLock.lock()
        live[ObjectIdentifier(bridge)] = nil
        liveLock.unlock()
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
            Self.letGo(self)
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
        var prfOutput: Data?
        var secondPrfOutput: Data?
        if #available(iOS 18.0, *), let prf = assertion.prf {
            prfOutput = PasskeyVault.usablePrfOutput(prf.first.withUnsafeBytes { Data($0) })
            secondPrfOutput = PasskeyVault.usablePrfOutput(prf.second.map { key in
                key.withUnsafeBytes { Data($0) }
            })
        }
        self.continuation?.resume(returning: WebauthnAssertion.Result(
            credential: json,
            credentialId: assertion.credentialID,
            prfOutput: prfOutput,
            secondPrfOutput: secondPrfOutput
        ))
    }

    func authorizationController(
        controller: ASAuthorizationController,
        didCompleteWithError error: Error
    ) {
        defer {
            self.continuation = nil
            self.retainedController = nil
            Self.letGo(self)
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
