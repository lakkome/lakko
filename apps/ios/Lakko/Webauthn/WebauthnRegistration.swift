// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import AuthenticationServices
import CryptoKit
import Foundation
import UIKit

/// `WebauthnRegistration` — async-throws wrapper around
/// `ASAuthorizationController` for the Phase 2a2d passkey
/// registration flow.
///
/// Mirrors `apps/web/src/auth/webauthn-client.ts`'s
/// `WebauthnClient.registerPasskey` middle step
/// (`navigator.credentials.create`). The web/native split:
///
///   * The web side uses `navigator.credentials.create(...)`
///     directly inside the same `WebauthnClient` class.
///   * On iOS the platform authenticator is driven through a
///     delegate-style `ASAuthorizationController`; we bridge it
///     to an async API via a continuation so the call site
///     stays linear.
///
/// The two halves of the flow live in
/// ``WebauthnClient/startRegistration(userName:nickname:)`` and
/// ``WebauthnClient/finishRegistration(stateId:nickname:credential:)``;
/// this type only covers the platform-authenticator hop in the
/// middle.
///
/// ## Platform requirements
///
///   * iOS 16+ (`ASAuthorizationPlatformPublicKeyCredentialProvider`).
///   * Associated Domain configured in Info.plist for production —
///     `webcredentials:<rp_id>` matching `LAKKO_WEBAUTHN_RP_ID`
///     on the server. For dev (localhost) Apple's Platform Passkey
///     API rejects the request; testing uses a TestFlight build
///     against a real apple-app-site-association host.
///
/// ## PRF
///
/// With a `prfSalt` the registration also asks the new passkey for its PRF
/// evaluation (``PasskeyVault``): the platform API
/// (`ASAuthorizationPlatformPublicKeyCredentialRegistrationRequest.prf` and
/// `ASAuthorizationPlatformPublicKeyCredentialRegistration.prf`) exists from
/// iOS 18.0. The result reports whether the passkey's provider supports PRF
/// and, when the provider evaluated it at creation, the output itself. Below
/// iOS 18 the salt is ignored and both are absent. The output is returned to
/// the caller only: it is not part of the credential JSON the server
/// receives.
///
/// ## Usage
///
/// ```swift
/// let challenge = try base64UrlDecode(start.options.publicKey.challenge)
/// let userId    = try base64UrlDecode(start.options.publicKey.user.id)
/// let runner = WebauthnRegistration()
/// let registered = try await runner.run(
///     rpId: start.options.publicKey.rp.id,
///     challenge: challenge,
///     userName: start.options.publicKey.user.name,
///     userID: userId,
///     presentationAnchor: anchor // any UIWindow / scene anchor
/// )
/// try await client.finishRegistration(
///     stateId: start.state_id,
///     nickname: start.nickname,
///     credential: registered.credential
/// )
/// ```
@MainActor
public final class WebauthnRegistration: NSObject {

    public enum RegistrationError: Error, LocalizedError {
        case cancelled
        /// The passkey provider already holds a passkey for this account
        /// (an excluded credential matched): nothing was created. Not a
        /// failure of the device (UX `docs/ux/identity-recovery.md` 10.2).
        case alreadyExists
        case noCredential
        case underlying(Error)

        public var errorDescription: String? {
            switch self {
            case .cancelled:
                return "Cancelled. No passkey was created."
            case .alreadyExists:
                return "This device already has a passkey for this account."
            case .noCredential:
                return "No passkey credential was returned."
            case .underlying(let err):
                return err.localizedDescription
            }
        }
    }

    /// What one registration yields.
    public struct Result {
        /// The credential as the server's finish route takes it. Carries no
        /// extension results: the PRF output never leaves this device.
        public let credential: RegisterPublicKeyCredentialJSON
        /// The raw id of the new credential.
        public let credentialId: Data
        /// The 32-byte PRF output for the salt asked, when the provider
        /// evaluated it at creation; else `nil`.
        public let prfOutput: Data?
        /// What the platform reported about PRF for the new credential:
        /// `true` supported, `false` not supported, `nil` nothing reported
        /// (no salt asked, or a platform older than iOS 18).
        public let prfSupported: Bool?
        /// The output for the SECOND salt asked (the reset-seal salt,
        /// ADR-0101 section 10), when the provider evaluated it at creation.
        public var secondPrfOutput: Data? = nil
    }

    /// Run a single registration round. Holds onto an internal
    /// `ASAuthorizationController` + continuation; constructing
    /// fresh per call keeps the delegate lifetime crisp.
    public func run(
        rpId: String,
        challenge: Data,
        userName: String,
        userID: Data,
        prfSalt: Data? = nil,
        secondPrfSalt: Data? = nil,
        excludedCredentials: [Data] = [],
        presentationAnchor: ASPresentationAnchor
    ) async throws -> Result {
        let provider = ASAuthorizationPlatformPublicKeyCredentialProvider(
            relyingPartyIdentifier: rpId
        )
        let request = provider.createCredentialRegistrationRequest(
            challenge: challenge,
            name: userName,
            userID: userID
        )
        if let prfSalt, #available(iOS 18.0, *) {
            if let secondPrfSalt {
                request.prf = .inputValues(.init(saltInput1: prfSalt, saltInput2: secondPrfSalt))
            } else {
                request.prf = .inputValues(.saltInput1(prfSalt))
            }
        }
        // The account's existing passkeys (the server's `excludeCredentials`):
        // a provider that holds one refuses instead of creating a second, or
        // silently replacing it. Set from iOS 18 only, the first system with
        // a distinct error for the refusal
        // (`ASAuthorizationError.matchedExcludedCredential`); what an older
        // system returns for it is not known, and a generic failure there
        // would read as "cannot create passkeys".
        if !excludedCredentials.isEmpty, #available(iOS 18.0, *) {
            request.excludedCredentials = excludedCredentials.map {
                ASAuthorizationPlatformPublicKeyCredentialDescriptor(credentialID: $0)
            }
        }
        // Apple's default UV policy matches what webauthn-rs's
        // Passkey flow expects (Required). No additional knob
        // is needed for the Passkey path.
        let controller = ASAuthorizationController(authorizationRequests: [request])

        // Bridge delegate + presentation context to a single
        // helper that captures the continuation; lifetime ends
        // when the continuation resumes.
        return try await withCheckedThrowingContinuation { continuation in
            let bridge = ControllerBridge(
                anchor: presentationAnchor,
                continuation: continuation
            )
            controller.delegate = bridge
            controller.presentationContextProvider = bridge
            // Keep the bridge alive for the duration of the
            // request. The delegate's terminal callbacks clear
            // the strong reference.
            bridge.attach(controller: controller)
            controller.performRequests()
        }
    }
}

/// Delegate / presentation-context bridge. Apple's
/// `ASAuthorizationController` insists on `NSObject` delegates;
/// we keep one alive per request, capturing the continuation.
private final class ControllerBridge: NSObject,
    ASAuthorizationControllerDelegate,
    ASAuthorizationControllerPresentationContextProviding
{
    private let anchor: ASPresentationAnchor
    private var continuation:
        CheckedContinuation<WebauthnRegistration.Result, Error>?
    private var retainedController: ASAuthorizationController?

    init(
        anchor: ASPresentationAnchor,
        continuation: CheckedContinuation<WebauthnRegistration.Result, Error>
    ) {
        self.anchor = anchor
        self.continuation = continuation
    }

    /// Pin the controller so its delegate (us) stays alive until
    /// one of the terminal callbacks fires.
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
            let registration = authorization.credential
                as? ASAuthorizationPlatformPublicKeyCredentialRegistration
        else {
            self.continuation?.resume(
                throwing: WebauthnRegistration.RegistrationError.noCredential
            )
            return
        }
        guard let rawAttestation = registration.rawAttestationObject else {
            self.continuation?.resume(
                throwing: WebauthnRegistration.RegistrationError.noCredential
            )
            return
        }
        let credentialIdB64 = base64UrlEncode(registration.credentialID)
        let json = RegisterPublicKeyCredentialJSON(
            id: credentialIdB64,
            rawId: credentialIdB64,
            response: RegisterPublicKeyCredentialJSON.Response(
                clientDataJSON: base64UrlEncode(registration.rawClientDataJSON),
                attestationObject: base64UrlEncode(rawAttestation),
                // iOS Platform Passkey is always "internal"; webauthn-rs
                // tolerates omitted transports, but a single-element
                // hint smooths future cross-device matchmaking.
                transports: ["internal"]
            ),
            extensions: nil
        )
        var prfOutput: Data?
        var secondPrfOutput: Data?
        var prfSupported: Bool?
        if #available(iOS 18.0, *), let prf = registration.prf {
            prfSupported = prf.isSupported
            prfOutput = PasskeyVault.usablePrfOutput(prf.first.map { key in
                key.withUnsafeBytes { Data($0) }
            })
            secondPrfOutput = PasskeyVault.usablePrfOutput(prf.second.map { key in
                key.withUnsafeBytes { Data($0) }
            })
        }
        self.continuation?.resume(returning: WebauthnRegistration.Result(
            credential: json,
            credentialId: registration.credentialID,
            prfOutput: prfOutput,
            prfSupported: prfSupported,
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
        self.continuation?.resume(throwing: WebauthnRegistration.registrationError(for: error))
    }
}

extension WebauthnRegistration {
    /// The ``RegistrationError`` for what the system sheet failed with.
    nonisolated static func registrationError(for error: Error) -> RegistrationError {
        if let asError = error as? ASAuthorizationError {
            if asError.code == .canceled { return .cancelled }
            if #available(iOS 18.0, *), asError.code == .matchedExcludedCredential {
                return .alreadyExists
            }
        }
        return .underlying(error)
    }
}

// MARK: - base64url helpers

/// Decode URL-safe base64 without padding. Matches the format the
/// server emits in every `challenge` / `user.id` /
/// `allowCredentials[].id` slot.
public func base64UrlDecode(_ s: String) throws -> Data {
    var b64 = s.replacingOccurrences(of: "-", with: "+")
        .replacingOccurrences(of: "_", with: "/")
    let pad = (4 - (b64.count % 4)) % 4
    b64 += String(repeating: "=", count: pad)
    guard let data = Data(base64Encoded: b64) else {
        throw WebauthnClientError.argument("invalid base64url payload")
    }
    return data
}

/// Encode raw bytes as URL-safe base64 without padding.
public func base64UrlEncode(_ data: Data) -> String {
    data.base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "")
}

// MARK: - Presentation anchor helper

/// Resolve a sensible presentation anchor for SwiftUI callers.
/// The `UIWindowScene` reachability check is the standard
/// pattern; fall back to a fresh window if nothing is on screen
/// (defensive — won't happen in normal app flow).
@MainActor
public func currentPresentationAnchor() -> ASPresentationAnchor {
    for scene in UIApplication.shared.connectedScenes {
        if let windowScene = scene as? UIWindowScene,
           let window = windowScene.windows.first(where: { $0.isKeyWindow })
        {
            return window
        }
    }
    return UIWindow()
}
