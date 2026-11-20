// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

// Supporter codes (ADR-0097, `docs/ux/supporters.md` section 1). A backer's
// code is exchanged, with no session, for an anonymous `beta-place` token the
// server cannot connect to the code; the device holds the token and attaches
// it to the first username bind, which then passes the beta cap. Mirror of
// the web and Android handling.

/// `403 {"error":"supporter-code-used"}` from `PUT /auth/login-handle`.
public let supporterCodeUsedCode = "supporter-code-used"

/// The first bind was refused because the held place was already spent by
/// another account (UX section 1.2, "already been used").
public struct SupporterCodeUsed: Error, Equatable {
    public init() {}
}

/// The line under the code field (UX section 1.2).
public enum SupporterCodeLine: Equatable {
    /// "This code does not work. Check it and try again."
    case unknown
    /// "Could not check the code. Try again."
    case failed
    /// "Too many tries. Try again later."
    case limited

    public var key: String {
        switch self {
        case .unknown: return "auth.signup.code.unknown"
        case .failed: return "auth.signup.code.failed"
        case .limited: return "auth.signup.code.limited"
        }
    }
}

/// The code field (UX section 1.1).
public enum SupporterCodeEntry {
    /// Letters and digits in a code.
    public static let length = 16

    /// `Continue` is enabled once the field holds 16 letters and digits;
    /// spaces and dashes do not count. Whether those are a valid code is
    /// the core's normalisation and the server's answer.
    public static func isComplete(_ text: String) -> Bool {
        text.unicodeScalars.filter { $0.isASCII && CharacterSet.alphanumerics.contains($0) }.count == length
    }
}

/// A place held on this device: the `beta-place` token header value and the
/// epoch it belongs to. A bearer token: kept in the Keychain, this device
/// only, never backed up.
public struct HeldSupporterPlace: Codable, Equatable {
    public let headerValue: String
    public let epoch: UInt64

    public init(headerValue: String, epoch: UInt64) {
        self.headerValue = headerValue
        self.epoch = epoch
    }

    /// The verifier accepts the current and the previous epoch (ADR-0097,
    /// Consequences), so a place is live through the epoch after its own.
    public func isLive(currentEpoch: UInt64) -> Bool {
        currentEpoch <= epoch &+ 1
    }
}

/// Where the held place lives.
public protocol SupporterPlaceStore: AnyObject {
    func load() -> HeldSupporterPlace?
    func save(_ place: HeldSupporterPlace)
    func clear()
}

/// The production store: one Keychain item, `AfterFirstUnlockThisDeviceOnly`
/// (not synced, not in a backup). Not tied to an account: the place exists
/// before the account does.
public final class KeychainSupporterPlaceStore: SupporterPlaceStore {
    public static let serviceTag = "me.lakko.supporter-place.v1"
    private static let account = "beta-place"
    private let service: String

    public init(service: String = KeychainSupporterPlaceStore.serviceTag) {
        self.service = service
    }

    public func load() -> HeldSupporterPlace? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: Self.account,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data else {
            if status != errSecItemNotFound {
                NSLog("[supporter] place read failed: OSStatus %d", status)
            }
            return nil
        }
        guard let place = try? JSONDecoder().decode(HeldSupporterPlace.self, from: data) else {
            clear()
            return nil
        }
        return place
    }

    public func save(_ place: HeldSupporterPlace) {
        guard let data = try? JSONEncoder().encode(place) else { return }
        let match: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: Self.account,
        ]
        var status = SecItemUpdate(match as CFDictionary, [kSecValueData: data] as CFDictionary)
        if status == errSecItemNotFound {
            var add = match
            add[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            add[kSecValueData] = data
            status = SecItemAdd(add as CFDictionary, nil)
        }
        if status != errSecSuccess {
            NSLog("[supporter] place write failed: OSStatus %d", status)
        }
    }

    public func clear() {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: Self.account,
        ]
        _ = SecItemDelete(query as CFDictionary)
    }
}

/// For tests and previews.
public final class InMemorySupporterPlaceStore: SupporterPlaceStore {
    public var place: HeldSupporterPlace?
    public init(_ place: HeldSupporterPlace? = nil) { self.place = place }
    public func load() -> HeldSupporterPlace? { place }
    public func save(_ place: HeldSupporterPlace) { self.place = place }
    public func clear() { place = nil }
}

/// The held place with its lifetime applied: an expired place is dropped.
public final class SupporterPlaceHolder {
    private let store: SupporterPlaceStore
    private let currentEpoch: () -> UInt64

    public init(
        store: SupporterPlaceStore = KeychainSupporterPlaceStore(),
        currentEpoch: @escaping () -> UInt64 = {
            supporterCodeEpochAt(unixSecs: UInt64(max(0, Date().timeIntervalSince1970)))
        }
    ) {
        self.store = store
        self.currentEpoch = currentEpoch
    }

    /// The place, while it is live; `nil` (and the item dropped) after.
    public func live() -> HeldSupporterPlace? {
        guard let place = store.load() else { return nil }
        guard place.isLive(currentEpoch: currentEpoch()) else {
            store.clear()
            return nil
        }
        return place
    }

    public func hold(_ place: HeldSupporterPlace) { store.save(place) }

    /// The bind said the place was spent by another account.
    public func drop() { store.clear() }
}

// MARK: - Redeeming a code

/// One `beta-place` token request (the FFI ``SupporterCodeRequest``); a seam
/// so the exchange can be driven by a test.
public protocol SupporterTokenRequesting {
    func epoch() -> UInt64
    func redeemBodyJson() -> String
    func finalize(evaluationB64url: String, proofB64url: String, publicKeyB64url: String) throws -> String
}

extension SupporterCodeRequest: SupporterTokenRequesting {}

/// The two network calls of the exchange.
public protocol SupporterCodeTransport {
    /// `POST /auth/supporter-code/redeem`; `nil` when no answer came.
    func redeem(bodyJson: String) async -> (status: Int, body: Data)?
    /// `GET /auth/tokens/key/beta-place/{epoch}`.
    func betaPlaceKey(epoch: UInt64) async throws -> String
}

/// What the redeem route answered.
enum SupporterRedeemAnswer: Equatable {
    case evaluated(epoch: UInt64, evaluation: String, proof: String)
    case epochNotIssuable(currentEpoch: UInt64)
    case line(SupporterCodeLine)

    /// Read an answer. `context` is the name the evaluation must carry.
    static func classify(status: Int, body: Data, context: String) -> SupporterRedeemAnswer {
        let object = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
        let code = object?["error"] as? String
        switch status {
        case 200..<300:
            guard let object,
                  object["context"] as? String == context,
                  let epoch = (object["epoch"] as? NSNumber)?.uint64Value,
                  let evaluation = object["evaluation"] as? String,
                  let proof = object["proof"] as? String
            else { return .line(.failed) }
            return .evaluated(epoch: epoch, evaluation: evaluation, proof: proof)
        case 400 where code == "epoch-not-issuable":
            guard let current = (object?["current_epoch"] as? NSNumber)?.uint64Value else {
                return .line(.failed)
            }
            return .epochNotIssuable(currentEpoch: current)
        case 400 where code == "supporter-code-malformed",
             404 where code == "supporter-code-unknown",
             409 where code == "supporter-code-mismatch",
             410 where code == "supporter-code-expired":
            return .line(.unknown)
        case 429:
            return .line(.limited)
        default:
            return .line(.failed)
        }
    }
}

/// How redeeming ended.
public enum SupporterRedeemOutcome: Equatable {
    case held(HeldSupporterPlace)
    case line(SupporterCodeLine)
}

/// Turns a typed code into a held place.
public protocol SupporterCodeRedeeming {
    func redeem(code: String) async -> SupporterRedeemOutcome
}

/// The exchange (ADR-0097 section 1.3): normalise, build the deterministic
/// request for the current epoch, ask the server, rebuild once at the epoch
/// the server names, check the proof against the key route's key (never one
/// in the response) and unblind.
public struct SupporterCodeRedeemer: SupporterCodeRedeeming {
    let transport: SupporterCodeTransport
    let normalize: (String) throws -> String
    let currentEpoch: () -> UInt64
    let makeRequest: (String, UInt64) throws -> SupporterTokenRequesting
    let contextName: String

    public init(
        transport: SupporterCodeTransport = AuthSupporterCodeTransport(),
        normalize: @escaping (String) throws -> String = { try supporterCodeNormalize(text: $0) },
        currentEpoch: @escaping () -> UInt64 = {
            supporterCodeEpochAt(unixSecs: UInt64(max(0, Date().timeIntervalSince1970)))
        },
        makeRequest: @escaping (String, UInt64) throws -> SupporterTokenRequesting = {
            try SupporterCodeRequest(code: $0, epoch: $1)
        },
        contextName: String = betaPlaceContextName()
    ) {
        self.transport = transport
        self.normalize = normalize
        self.currentEpoch = currentEpoch
        self.makeRequest = makeRequest
        self.contextName = contextName
    }

    public func redeem(code: String) async -> SupporterRedeemOutcome {
        let normalized: String
        do {
            normalized = try normalize(code)
        } catch {
            return .line(.unknown)
        }
        var epoch = currentEpoch()
        var rebuilt = false
        while true {
            let request: SupporterTokenRequesting
            do {
                request = try makeRequest(normalized, epoch)
            } catch {
                NSLog("[supporter] request build failed")
                return .line(.failed)
            }
            guard let answer = await transport.redeem(bodyJson: request.redeemBodyJson()) else {
                return .line(.failed)
            }
            switch SupporterRedeemAnswer.classify(
                status: answer.status, body: answer.body, context: contextName) {
            case .line(let line):
                return .line(line)
            case .epochNotIssuable(let current):
                guard !rebuilt, current != epoch else { return .line(.failed) }
                rebuilt = true
                epoch = current
                continue
            case .evaluated(let answeredEpoch, let evaluation, let proof):
                guard answeredEpoch == request.epoch() else { return .line(.failed) }
                do {
                    let key = try await transport.betaPlaceKey(epoch: answeredEpoch)
                    let header = try request.finalize(
                        evaluationB64url: evaluation, proofB64url: proof, publicKeyB64url: key)
                    return .held(HeldSupporterPlace(headerValue: header, epoch: answeredEpoch))
                } catch {
                    NSLog("[supporter] place not held: proof or key check failed")
                    return .line(.failed)
                }
            }
        }
    }
}

/// The production transport: auth's redeem route on the anonymous session
/// (no session token, no cookies), through the anti-censorship transport
/// when armed; the key through the anonymous-token key route.
public struct AuthSupporterCodeTransport: SupporterCodeTransport {
    let baseURL: String
    let session: URLSession

    public init(baseURL: String = AuthClient.defaultBaseURL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = AnonymousHTTP.session(pairedWith: session)
    }

    public func redeem(bodyJson: String) async -> (status: Int, body: Data)? {
        guard let url = URL(string: baseURL + "/auth/supporter-code/redeem") else { return nil }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = Data(bodyJson.utf8)
        do {
            let (data, response) = try await AntiCensorshipTransport.perform(
                request, session: session, config: .current)
            guard let http = response as? HTTPURLResponse else { return nil }
            return (http.statusCode, data)
        } catch {
            NSLog("[supporter] redeem request failed: %@", String(describing: type(of: error)))
            return nil
        }
    }

    public func betaPlaceKey(epoch: UInt64) async throws -> String {
        try await AuthAnonTokenIssuer(baseURL: baseURL, session: session)
            .publicKey(contextName: betaPlaceContextName(), epoch: epoch)
    }
}
