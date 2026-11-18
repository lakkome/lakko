// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
import LakkoCore

/// The ways to donate (`docs/ux/donations.md` sections 3 and 4.1): the three
/// anonymous discovery routes the donations screen reads, mirror of the web
/// `src/donate/client.ts`.
///
/// Every method is a statement signed offline by the donations key; this
/// client verifies each one against the public key pinned into the build
/// (``DonationsPin``) and keeps only what verifies. A build with no pinned key
/// keeps no method and asks the server nothing. Nothing the server sends
/// outside a statement is shown (the list entry's id aside, which must equal
/// the signed id). Nothing here logs a method, a link, an address or a number.

/// The build-time pinned donations key: Info.plist `LakkoDonationsKey`, set
/// from the `LAKKO_DONATIONS_KEY` build setting, base64url without padding.
/// Empty by default; there is no server fallback.
enum DonationsPin {
    /// The decoded 32-byte Ed25519 public key, or nil when unset, blank,
    /// malformed or not 32 bytes.
    static var key: Data? {
        decode(Bundle.main.object(forInfoDictionaryKey: "LakkoDonationsKey") as? String)
    }

    /// `raw` as a 32-byte key, or nil.
    static func decode(_ raw: String?) -> Data? {
        guard let trimmed = raw?.trimmingCharacters(in: .whitespacesAndNewlines),
              !trimmed.isEmpty,
              let key = base64UrlDecodeNoPad(trimmed),
              key.count == 32
        else { return nil }
        return key
    }
}

/// One verified way to donate. Every field comes from the signed statement.
struct DonateMethod: Equatable, Identifiable {
    struct Crypto: Equatable {
        let coin: String
        let network: String
        let address: String
    }

    let id: String
    let kind: DonationKind
    /// The payment link, `https://` only.
    let link: URL?
    /// SHA-256 of the QR image the server serves for this method (32 bytes).
    let qrSha256: Data?
    let hasPhone: Bool
    /// For a crypto method: the coin, the network and the address.
    let crypto: Crypto?
}

/// A network or server failure of a donations request (not a refusal of a
/// statement, which is dropped silently).
enum DonationsError: Error, Equatable {
    case network
    case http(Int)
    case malformed
    case refused
}

final class DonationsClient {
    /// One HTTP exchange; production goes through the app's transport.
    typealias Transport = @Sendable (URLRequest) async throws -> (Data, URLResponse)
    /// `verifyDonationMethod` of the core.
    typealias MethodVerifier = (Data, Data) throws -> DonationMethod
    /// `verifyDonationPhone` of the core.
    typealias PhoneVerifier = (Data, Data, String) throws -> String

    static let sha256Length = 32
    /// Largest QR image accepted (the signing tool and discovery refuse larger).
    static let maxQrBytes = 128 * 1024
    /// The masked form shown before the number is revealed.
    static let maskedPhone = "+7 9•• ••• •• ••"

    let baseURL: String
    let key: Data?
    private let transport: Transport
    private let verifyMethod: MethodVerifier
    private let verifyPhone: PhoneVerifier

    /// The ordinary transport: the obfuscated tunnel when armed, else the
    /// anonymous session (no cookie, no session token).
    static let appTransport: Transport = { request in
        try await AntiCensorshipTransport.perform(
            request, session: AnonymousHTTP.session, config: AntiCensorshipConfig.current)
    }

    init(
        baseURL: String = DiscoveryClient.defaultBaseURL,
        key: Data? = DonationsPin.key,
        transport: @escaping Transport = DonationsClient.appTransport,
        verifyMethod: @escaping MethodVerifier = { try verifyDonationMethod(statement: $0, key: $1) },
        verifyPhone: @escaping PhoneVerifier = { try verifyDonationPhone(statement: $0, key: $1, id: $2) }
    ) {
        self.baseURL = baseURL
        self.key = key
        self.transport = transport
        self.verifyMethod = verifyMethod
        self.verifyPhone = verifyPhone
    }

    // MARK: - List

    /// `GET /discovery/donate`, verified. With no pinned key: no method, and
    /// no request. Throws ``DonationsError`` on a network or server failure.
    func fetchMethods() async throws -> [DonateMethod] {
        guard let key else { return [] }
        let (data, status) = try await exchange(method: "GET", path: "/discovery/donate", body: nil)
        guard status == 200 else { throw DonationsError.http(status) }
        guard (try? JSONSerialization.jsonObject(with: data)) != nil else {
            throw DonationsError.malformed
        }
        return Self.verifyMethods(json: data, key: key, verify: verifyMethod)
    }

    /// Verify the list route's answer against `key`. An entry is dropped when
    /// its statement does not decode or verify, when the signed id differs from
    /// the entry's id, when it repeats an id already kept, or when it is not a
    /// method this client draws. Order is the server's.
    static func verifyMethods(json: Data, key: Data, verify: MethodVerifier) -> [DonateMethod] {
        guard let object = try? JSONSerialization.jsonObject(with: json) as? [String: Any],
              let list = object["methods"] as? [Any]
        else { return [] }
        var out: [DonateMethod] = []
        var seen = Set<String>()
        for raw in list {
            guard let entry = raw as? [String: Any],
                  let id = entry["id"] as? String,
                  let statement = decodeStatement(entry["statement_b64"])
            else { continue }
            guard let verified = try? verify(statement, key) else { continue }
            guard verified.id == id, !seen.contains(id), let method = toMethod(verified) else { continue }
            seen.insert(id)
            out.append(method)
        }
        return out
    }

    // MARK: - QR

    /// The method's QR image bytes, only when their SHA-256 equals
    /// `expectedSha256`; nil on a mismatch or any failure.
    func fetchQr(id: String, expectedSha256: Data) async -> Data? {
        guard expectedSha256.count == Self.sha256Length,
              let (data, status) = try? await exchange(
                method: "GET", path: "/discovery/donate/\(Self.pathSegment(id))/qr", body: nil),
              status == 200,
              !data.isEmpty, data.count <= Self.maxQrBytes
        else { return nil }
        let digest = Data(SHA256.hash(data: data))
        return Self.sameBytes(digest, expectedSha256) ? data : nil
    }

    // MARK: - Phone

    /// `POST /discovery/donate/{id}/phone`, verified against the pinned key
    /// and the method's id. Throws when the number is not given or does not
    /// verify.
    func revealPhone(id: String) async throws -> String {
        guard let key else { throw DonationsError.refused }
        let body = Data(#"{"action":"reveal"}"#.utf8)
        let (data, status) = try await exchange(
            method: "POST", path: "/discovery/donate/\(Self.pathSegment(id))/phone", body: body)
        guard status == 200 else { throw DonationsError.http(status) }
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let statement = Self.decodeStatement(object["statement_b64"])
        else { throw DonationsError.malformed }
        let phone: String
        do {
            phone = try verifyPhone(statement, key, id)
        } catch {
            throw DonationsError.refused
        }
        guard let text = Self.text(phone) else { throw DonationsError.refused }
        return text
    }

    // MARK: - Helpers

    private func exchange(method: String, path: String, body: Data?) async throws -> (Data, Int) {
        guard let url = URL(string: baseURL + path) else { throw DonationsError.malformed }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.cachePolicy = .reloadIgnoringLocalCacheData
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let raw: (Data, URLResponse)
        do {
            raw = try await transport(request)
        } catch {
            throw DonationsError.network
        }
        guard let http = raw.1 as? HTTPURLResponse else { throw DonationsError.malformed }
        return (raw.0, http.statusCode)
    }

    private static func pathSegment(_ id: String) -> String {
        id.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? ""
    }

    private static func decodeStatement(_ value: Any?) -> Data? {
        guard let s = value as? String, !s.isEmpty, let data = Data(base64Encoded: s), !data.isEmpty
        else { return nil }
        return data
    }

    private static func text(_ value: String?) -> String? {
        guard let value, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return value
    }

    /// Read one verified statement into a method, or nil when it is not one
    /// this client draws.
    static func toMethod(_ v: DonationMethod) -> DonateMethod? {
        if v.kind == .crypto {
            guard let coin = text(v.coin), let network = text(v.network), let address = text(v.address)
            else { return nil }
            return DonateMethod(
                id: v.id, kind: .crypto, link: nil, qrSha256: nil, hasPhone: false,
                crypto: .init(coin: coin, network: network, address: address))
        }
        let link = text(v.link).flatMap { raw -> URL? in
            guard raw.hasPrefix("https://"), let url = URL(string: raw), url.scheme == "https" else { return nil }
            return url
        }
        let qr = v.qrSha256.flatMap { $0.count == sha256Length ? $0 : nil }
        guard link != nil || qr != nil || v.hasPhone else { return nil }
        return DonateMethod(
            id: v.id, kind: v.kind, link: link, qrSha256: qr, hasPhone: v.hasPhone, crypto: nil)
    }

    static func sameBytes(_ a: Data, _ b: Data) -> Bool {
        guard a.count == b.count else { return false }
        var diff: UInt8 = 0
        for (x, y) in zip(a, b) { diff |= x ^ y }
        return diff == 0
    }
}
