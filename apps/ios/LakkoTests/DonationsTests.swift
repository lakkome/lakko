// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import XCTest
import LakkoCore
@testable import Lakko

/// The donations screen keeps only what verifies against the pinned key
/// (`docs/ux/donations.md` sections 3 and 4.1). The statements below are the
/// ones the web suite (`apps/web/src/donate/client.test.ts`) uses: signed once
/// by the offline tool with a test key made for those tests; only its public
/// half is here. They run through the real UniFFI `verifyDonationMethod` /
/// `verifyDonationPhone`.
final class DonationsTests: XCTestCase {

    /// Public half of the test donations key (base64url, no padding).
    static let keyB64Url = "Aa2oo1Ax_WyUSvvLwpKvosXMZ9MBmeQodQUQ9cRFhpI"
    /// sbp: link `https://example.org/pay/sbp`, a QR (hash of ``qrPngB64``), a phone.
    static let sbp =
        "AQAAAaHhyfJJA3NicAEAG2h0dHBzOi8vZXhhbXBsZS5vcmcvcGF5L3NicAAAAAABTY9om8Dn/h1VongZaStJZ2Ltw5S7xRO2zK1OTWl3j2IBRiZ/imXADYiETe7DnEgGHssNMCOfZn3ma9jQiQf+T/BF+ReJHN3VbG60MRQdurvr6+Pb9K1UuCZBgiqw/MfUDw=="
    /// bmc: link `https://example.org/bmc` only.
    static let bmc =
        "AQAAAaHhyfJJA2JtYwMAF2h0dHBzOi8vZXhhbXBsZS5vcmcvYm1jAAAAAAAA5nZKnFPzfsZLbOjeJ3ks3ABjbMWMkeK2W9lf+aExW3c+aB93oRqpdX7pHDjhVMMtq/NEbJCsTmreDG9zvo58Cw=="
    /// usdt: crypto, USDT on TRON, address `TTestAddress123`.
    static let usdt =
        "AQAAAaHhyfJJBHVzZHQEAAAEVVNEVARUUk9OAA9UVGVzdEFkZHJlc3MxMjMAAAkZA3eKg6Y+KZTc4Ctf+p17h7jEsDA4LmShIFGu4RTMZ24Mhidcu/CmzztuD6MW8WZfKBgLgu6jQVJeAzmRBww="
    /// The phone statement of `sbp`: `+7 900 000-00-00`.
    static let sbpPhone =
        "AQNzYnAQKzcgOTAwIDAwMC0wMC0wMN9JN67Dh3kDZ5H5s41EhuyEPWdloGhf46BgxODda9QSaX+mF1HOFUdjq2BrnSD0UaCBGWztppdar6NonJ74Www="
    /// The QR image `sbp`'s statement hashes.
    static let qrPngB64 = "iVBORw0KGgpmYWtlcXJpbWFnZWJ5dGVzLWZvci10ZXN0"

    static let base = "https://discovery.test/discovery"

    private var key: Data {
        guard let key = DonationsPin.decode(Self.keyB64Url) else {
            XCTFail("test key did not decode")
            return Data()
        }
        return key
    }

    private static let verify: DonationsClient.MethodVerifier = {
        try verifyDonationMethod(statement: $0, key: $1)
    }

    private static func list(_ entries: [[String: Any]]) -> Data {
        // swiftlint:disable:next force_try
        try! JSONSerialization.data(withJSONObject: ["methods": entries])
    }

    /// Flip one bit of a base64 statement, inside the signed bytes.
    private static func tampered(_ b64: String, at index: Int) -> String {
        var bytes = [UInt8](Data(base64Encoded: b64) ?? Data())
        bytes[index] ^= 0x01
        return Data(bytes).base64EncodedString()
    }

    /// Records every request and answers each with `respond`.
    final class Recorder: @unchecked Sendable {
        private let lock = NSLock()
        private var _requests: [URLRequest] = []
        let respond: @Sendable (URLRequest) throws -> (Int, Data)

        init(_ respond: @escaping @Sendable (URLRequest) throws -> (Int, Data)) {
            self.respond = respond
        }

        var requests: [URLRequest] {
            lock.lock(); defer { lock.unlock() }
            return _requests
        }

        private func record(_ request: URLRequest) {
            lock.lock(); defer { lock.unlock() }
            _requests.append(request)
        }

        var transport: DonationsClient.Transport {
            { [self] request in
                record(request)
                let (status, body) = try respond(request)
                let response = HTTPURLResponse(
                    url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: nil)!
                return (body, response)
            }
        }
    }

    private func client(key: Data?, _ recorder: Recorder) -> DonationsClient {
        DonationsClient(baseURL: Self.base, key: key, transport: recorder.transport)
    }

    // MARK: - The list

    func testVerifiedMethodsKeepTheServersOrderAndTheSignedFields() {
        let methods = DonationsClient.verifyMethods(
            json: Self.list([
                ["id": "sbp", "statement_b64": Self.sbp],
                ["id": "bmc", "statement_b64": Self.bmc],
                ["id": "usdt", "statement_b64": Self.usdt],
            ]),
            key: key, verify: Self.verify)
        XCTAssertEqual(methods.map(\.id), ["sbp", "bmc", "usdt"])
        XCTAssertEqual(methods[0].kind, .sbp)
        XCTAssertEqual(methods[0].link?.absoluteString, "https://example.org/pay/sbp")
        XCTAssertTrue(methods[0].hasPhone)
        XCTAssertEqual(methods[0].qrSha256?.count, 32)
        XCTAssertEqual(methods[1].kind, .bmc)
        XCTAssertEqual(methods[1].link?.absoluteString, "https://example.org/bmc")
        XCTAssertNil(methods[1].qrSha256)
        XCTAssertFalse(methods[1].hasPhone)
        XCTAssertEqual(methods[2].kind, .crypto)
        XCTAssertEqual(methods[2].crypto, .init(coin: "USDT", network: "TRON", address: "TTestAddress123"))
        XCTAssertNil(methods[2].link)
    }

    func testAStatementChangedInAnySignedByteIsDroppedAndTheRestKept() {
        let len = Data(base64Encoded: Self.bmc)!.count
        for index in [1, 12, 20, len - 1] {
            let methods = DonationsClient.verifyMethods(
                json: Self.list([
                    ["id": "bmc", "statement_b64": Self.tampered(Self.bmc, at: index)],
                    ["id": "usdt", "statement_b64": Self.usdt],
                ]),
                key: key, verify: Self.verify)
            XCTAssertEqual(methods.map(\.id), ["usdt"], "byte \(index)")
        }
    }

    func testAnEntryWhoseIdDiffersFromTheSignedIdIsDropped() {
        let methods = DonationsClient.verifyMethods(
            json: Self.list([
                ["id": "bmc", "statement_b64": Self.sbp],
                ["id": "usdt", "statement_b64": Self.usdt],
            ]),
            key: key, verify: Self.verify)
        XCTAssertEqual(methods.map(\.id), ["usdt"])
    }

    func testUnsignedFieldsAreIgnoredAndAMethodIsKeptOnce() {
        let methods = DonationsClient.verifyMethods(
            json: Self.list([
                ["id": "bmc", "statement_b64": Self.bmc, "link": "https://attacker.example/"],
                ["id": "bmc", "statement_b64": Self.bmc],
            ]),
            key: key, verify: Self.verify)
        XCTAssertEqual(methods.count, 1)
        XCTAssertEqual(methods.first?.link?.absoluteString, "https://example.org/bmc")
    }

    func testNothingVerifiesUnderAnotherKeyOrForAMalformedAnswer() {
        let other = Data(repeating: 7, count: 32)
        XCTAssertEqual(
            DonationsClient.verifyMethods(
                json: Self.list([["id": "bmc", "statement_b64": Self.bmc]]), key: other, verify: Self.verify),
            [])
        XCTAssertEqual(DonationsClient.verifyMethods(json: Data("null".utf8), key: key, verify: Self.verify), [])
        XCTAssertEqual(
            DonationsClient.verifyMethods(json: Data(#"{"methods":"no"}"#.utf8), key: key, verify: Self.verify), [])
        XCTAssertEqual(
            DonationsClient.verifyMethods(
                json: Self.list([["id": "bmc", "statement_b64": "not base64!"]]), key: key, verify: Self.verify),
            [])
    }

    // MARK: - The pinned key

    func testThePinReadsNoKeyWhenUnsetBlankOrNot32Bytes() {
        XCTAssertNil(DonationsPin.decode(nil))
        XCTAssertNil(DonationsPin.decode(""))
        XCTAssertNil(DonationsPin.decode("   "))
        XCTAssertNil(DonationsPin.decode("AAAA"))
        XCTAssertEqual(DonationsPin.decode(" \(Self.keyB64Url)\n")?.count, 32)
    }

    func testWithNoPinnedKeyNothingIsShownAndTheServerIsNotAsked() async throws {
        let recorder = Recorder { _ in (200, Self.list([["id": "bmc", "statement_b64": Self.bmc]])) }
        let methods = try await client(key: nil, recorder).fetchMethods()
        XCTAssertEqual(methods, [])
        XCTAssertTrue(recorder.requests.isEmpty)
    }

    func testWithThePinnedKeyTheListIsFetchedOnceAndVerified() async throws {
        let recorder = Recorder { _ in
            (200, Self.list([
                ["id": "bmc", "statement_b64": Self.bmc],
                ["id": "sbp", "statement_b64": Self.tampered(Self.sbp, at: 5)],
            ]))
        }
        let methods = try await client(key: key, recorder).fetchMethods()
        XCTAssertEqual(methods.map(\.id), ["bmc"])
        XCTAssertEqual(recorder.requests.map { $0.url?.absoluteString }, ["\(Self.base)/discovery/donate"])
        XCTAssertEqual(recorder.requests.first?.httpMethod, "GET")
        XCTAssertNil(recorder.requests.first?.value(forHTTPHeaderField: "Authorization"))
    }

    func testANetworkOrServerFailureIsAnErrorNotAnEmptyList() async {
        let offline = Recorder { _ in throw URLError(.notConnectedToInternet) }
        do {
            _ = try await client(key: key, offline).fetchMethods()
            XCTFail("expected a failure")
        } catch {
            XCTAssertEqual(error as? DonationsError, .network)
        }
        let broken = Recorder { _ in (502, Data()) }
        do {
            _ = try await client(key: key, broken).fetchMethods()
            XCTFail("expected a failure")
        } catch {
            XCTAssertEqual(error as? DonationsError, .http(502))
        }
    }

    @MainActor
    func testTheScreenModelShowsNoneErrorOrTheMethods() async {
        let none = DonationsModel(client: client(key: nil, Recorder { _ in (200, Data()) }))
        await none.load()
        XCTAssertEqual(none.phase, .none)

        let failing = DonationsModel(client: client(key: key, Recorder { _ in (500, Data()) }))
        await failing.load()
        XCTAssertEqual(failing.phase, .error)

        let refused = DonationsModel(client: client(key: Data(repeating: 7, count: 32), Recorder { _ in
            (200, Self.list([["id": "bmc", "statement_b64": Self.bmc]]))
        }))
        await refused.load()
        XCTAssertEqual(refused.phase, .none)

        let loaded = DonationsModel(client: client(key: key, Recorder { _ in
            (200, Self.list([["id": "usdt", "statement_b64": Self.usdt]]))
        }))
        await loaded.load()
        guard case .loaded(let methods) = loaded.phase else {
            return XCTFail("expected methods, got \(loaded.phase)")
        }
        XCTAssertEqual(methods.map(\.id), ["usdt"])
    }

    // MARK: - The QR image

    private func sbpHash() -> Data {
        let methods = DonationsClient.verifyMethods(
            json: Self.list([["id": "sbp", "statement_b64": Self.sbp]]), key: key, verify: Self.verify)
        return methods.first?.qrSha256 ?? Data()
    }

    func testTheQrIsReturnedWhenItsHashEqualsTheSignedHash() async {
        let png = Data(base64Encoded: Self.qrPngB64)!
        XCTAssertEqual(Data(SHA256.hash(data: png)), sbpHash())
        let recorder = Recorder { _ in (200, png) }
        let data = await client(key: key, recorder).fetchQr(id: "sbp", expectedSha256: sbpHash())
        XCTAssertEqual(data, png)
        XCTAssertEqual(recorder.requests.first?.url?.absoluteString, "\(Self.base)/discovery/donate/sbp/qr")
    }

    /// A mismatch hides the QR only: the method stays with its link and phone.
    func testAQrHashMismatchHidesOnlyTheQr() async {
        let methods = DonationsClient.verifyMethods(
            json: Self.list([["id": "sbp", "statement_b64": Self.sbp]]), key: key, verify: Self.verify)
        XCTAssertEqual(methods.count, 1)
        XCTAssertNotNil(methods.first?.link)
        XCTAssertEqual(methods.first?.hasPhone, true)
        let hash = sbpHash()
        for respond in [
            Recorder { _ in (200, Data([1, 2, 3])) },
            Recorder { _ in (404, Data()) },
            Recorder { _ in (200, Data(count: DonationsClient.maxQrBytes + 1)) },
            Recorder { _ in throw URLError(.timedOut) },
        ] {
            let data = await client(key: key, respond).fetchQr(id: "sbp", expectedSha256: hash)
            XCTAssertNil(data)
        }
    }

    // MARK: - The phone number

    func testAVerifiedPhoneStatementGivesTheNumber() async throws {
        let recorder = Recorder { _ in
            (200, try JSONSerialization.data(withJSONObject: ["statement_b64": Self.sbpPhone]))
        }
        let phone = try await client(key: key, recorder).revealPhone(id: "sbp")
        XCTAssertEqual(phone, "+7 900 000-00-00")
        let request = recorder.requests.first
        XCTAssertEqual(request?.httpMethod, "POST")
        XCTAssertEqual(request?.url?.absoluteString, "\(Self.base)/discovery/donate/sbp/phone")
        XCTAssertEqual(request?.httpBody, Data(#"{"action":"reveal"}"#.utf8))
    }

    func testAPhoneStatementThatDoesNotVerifyIsRefused() async {
        let cases: [(String, Data?, [String: Any])] = [
            ("sbp", key, ["statement_b64": Self.tampered(Self.sbpPhone, at: 8)]),
            ("bmc", key, ["statement_b64": Self.sbpPhone]),
            ("sbp", key, ["phone": "+7 999 999-99-99"]),
            ("sbp", nil, ["statement_b64": Self.sbpPhone]),
        ]
        for (id, key, body) in cases {
            let recorder = Recorder { _ in (200, try JSONSerialization.data(withJSONObject: body)) }
            do {
                _ = try await client(key: key, recorder).revealPhone(id: id)
                XCTFail("expected a refusal for \(id)")
            } catch {}
        }
    }
}
