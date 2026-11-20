// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreImage
import CryptoKit
import SwiftUI
import XCTest
import LakkoCore
@testable import Lakko

/// The donations screen keeps only what verifies against the pinned key
/// (`docs/ux/donations.md` sections 3 and 4.1). The statements below are the
/// signed once by the offline tool with a test key made for these tests; only
/// its public half is here. They run through the real UniFFI `verifyDonationMethod` /
/// `verifyDonationPhone`.
final class DonationsTests: XCTestCase {

    /// Public half of the test donations key (base64url, no padding). The
    /// statements below are version 2 (with the signed `icon` byte), signed by
    /// `tools/donations` with a key made for these tests and then deleted.
    static let keyB64Url = "6txH_uV_JuyMcJ-W72dTNPRX6P2UdAKsW4_eKGBFJug"
    /// sbp: link `https://example.org/pay/sbp`, a QR (hash of ``qrPngB64``), a phone.
    static let sbp =
        "AgAAAaHuXt3BA3NicAEAABtodHRwczovL2V4YW1wbGUub3JnL3BheS9zYnAAAAAAAenLQhER6abcLRcEiEZG9EBEzzh0xg8JqGwyCHOa87KmAbBY9z2Yx1goxBMQkoFr6YDeK8e7OvYYMh+zAEO/CrA2mFnnvmoe5UGdmbLLdOqY+VXvNkShE4MNneNJKHLKZwI="
    /// bmc: link `https://example.org/bmc` only.
    static let bmc =
        "AgAAAaHuXt3BA2JtYwMAABdodHRwczovL2V4YW1wbGUub3JnL2JtYwAAAAAAADzJT5y2xrfLUbc/BmwBXNAKvEqIMWONjQQLS/e1RBHBvlHXWQ0JF94z0dgiJy62SVwLTcn+w9+CPK7tzJjlzw4="
    /// usdt: crypto, USDT on TRON, address `TTestAddress123`, no coin mark.
    static let usdt =
        "AgAAAaHuXt3BBHVzZHQEAAAABFVTRFQEVFJPTgAPVFRlc3RBZGRyZXNzMTIzAACEoDGGbPUcTtNvtQSiGdgK5X0swt19bMWGQtHozZ/gp9wxW1tr/YfP8PrcisjbBo+JZKZpnqLo8tkSaQBOfYoM"
    /// btc: crypto, Bitcoin on Bitcoin, the Bitcoin mark.
    static let btc =
        "AgAAAaHuXt3BA2J0YwQBAAAHQml0Y29pbgdCaXRjb2luAChiYzFxdGVzdGFkZHJlc3MwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwAAALGV8kaQvJoCnzOTHDCVlvL+BjEkwCm6ZJB2vTwQ8PfUwI0S73qfi8fjKRWRwBhqDUNKgC9PSrHNH0Jt1iw6AI"
    static let btcAddress = "bc1qtestaddress0000000000000000000000000"
    /// revolut: link and a QR of 434 KiB (``bigQr``), the size of the real
    /// Revolut image, above the 128 KiB the screen used to accept.
    static let revolut =
        "AgAAAaHuXt3BB3Jldm9sdXQCAAAbaHR0cHM6Ly9leGFtcGxlLm9yZy9yZXZvbHV0AAAAAAE+/0mCWcdYBXieYaAcyWb9tsh5694CPkYH/9t2YbTklgDT6W9/33RhwV04Ig42e0aaMlQrwoo1JvNTjsuiQ4musKFfLmtMt8irclzgY3EeM+stAE0Q2lxwrljrZt5zeYEP"
    /// The QR `revolut`'s statement hashes: the PNG signature, then zeros.
    static var bigQr: Data {
        Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) + Data(count: 434 * 1024 - 8)
    }
    /// The phone statement of `sbp`: `+7 900 000-00-00`.
    static let sbpPhone =
        "AQNzYnAQKzcgOTAwIDAwMC0wMC0wMKOAoXanYIsRhX9A2q/v93DhwEUlJvgT00oTWBf1TGvl7pWm0ivMuHDsxkMuInBEl0pjjAh4fKx1hLPQx9N15QE="
    /// The QR image `sbp`'s statement hashes: a real 2x2 PNG.
    static let qrPngB64 =
        "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEElEQVR4nGNgYGD4D8UQBgAd9AP9yOH2qAAAAABJRU5ErkJggg=="

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
        XCTAssertEqual(
            methods[2].crypto, .init(coin: "USDT", network: "TRON", address: "TTestAddress123", icon: .none))
        XCTAssertNil(methods[2].link)
    }

    func testACryptoMethodCarriesItsSignedCoinMark() {
        let methods = DonationsClient.verifyMethods(
            json: Self.list([["id": "btc", "statement_b64": Self.btc]]), key: key, verify: Self.verify)
        XCTAssertEqual(
            methods.first?.crypto,
            .init(coin: "Bitcoin", network: "Bitcoin", address: Self.btcAddress, icon: .btc))
    }

    /// The icon byte is signed: changing it (byte 14, after version, the
    /// signing time, the id `btc` and the kind) drops the method.
    func testTheCoinMarkByteIsSigned() {
        let raw = [UInt8](Data(base64Encoded: Self.btc)!)
        XCTAssertEqual(raw[0], 2)
        XCTAssertEqual(raw[13], 4, "kind byte: crypto")
        XCTAssertEqual(raw[14], 1, "icon byte: btc")
        var changed = raw
        changed[14] = 2
        let methods = DonationsClient.verifyMethods(
            json: Self.list([["id": "btc", "statement_b64": Data(changed).base64EncodedString()]]),
            key: key, verify: Self.verify)
        XCTAssertEqual(methods, [])
    }

    func testTheCardsAreGroupedPaymentFirstThenCryptoInTheServersOrder() {
        let methods = DonationsClient.verifyMethods(
            json: Self.list([
                ["id": "usdt", "statement_b64": Self.usdt],
                ["id": "sbp", "statement_b64": Self.sbp],
                ["id": "btc", "statement_b64": Self.btc],
                ["id": "bmc", "statement_b64": Self.bmc],
            ]),
            key: key, verify: Self.verify)
        let groups = DonationsClient.groups(methods)
        XCTAssertEqual(groups.payment.map(\.id), ["sbp", "bmc"])
        XCTAssertEqual(groups.crypto.map(\.id), ["usdt", "btc"])
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

    /// The real Revolut image is 434 KiB; the first native build capped the
    /// download at 128 KiB and so never showed it.
    func testAQrImageOf434KiBIsAccepted() async throws {
        XCTAssertEqual(DonationsClient.maxQrBytes, 512 * 1024)
        let methods = DonationsClient.verifyMethods(
            json: Self.list([["id": "revolut", "statement_b64": Self.revolut]]), key: key, verify: Self.verify)
        let hash = try XCTUnwrap(methods.first?.qrSha256)
        let big = Self.bigQr
        let recorder = Recorder { _ in (200, big) }
        let data = await client(key: key, recorder).fetchQr(id: "revolut", expectedSha256: hash)
        XCTAssertEqual(data?.count, 434 * 1024)
    }

    /// Loading the screen loads every payment QR and decodes it: the first
    /// native build attached the QR request to a view that drew nothing until
    /// the image arrived, so the request never ran and no QR ever appeared.
    @MainActor
    func testLoadingTheScreenShowsEveryVerifiedPaymentQr() async throws {
        let png = try XCTUnwrap(Data(base64Encoded: Self.qrPngB64))
        let recorder = Recorder { request in
            switch request.url?.path {
            case "/discovery/discovery/donate":
                return (200, Self.list([
                    ["id": "sbp", "statement_b64": Self.sbp],
                    ["id": "bmc", "statement_b64": Self.bmc],
                    ["id": "btc", "statement_b64": Self.btc],
                ]))
            case "/discovery/discovery/donate/sbp/qr":
                return (200, png)
            default:
                return (404, Data())
            }
        }
        let model = DonationsModel(client: client(key: key, recorder))
        await model.load()
        guard case .loaded(let methods) = model.phase else {
            return XCTFail("expected methods, got \(model.phase)")
        }
        XCTAssertEqual(methods.map(\.id), ["sbp", "bmc", "btc"])
        let image = try XCTUnwrap(model.qrImages["sbp"], "the sbp QR was not loaded")
        XCTAssertEqual(image.size.width * image.scale, 2)
        XCTAssertEqual(Set(model.qrImages.keys), ["sbp"])
        XCTAssertTrue(recorder.requests.contains { $0.url?.path == "/discovery/discovery/donate/sbp/qr" })
    }

    /// A QR whose bytes hash right but do not decode as an image is not shown.
    @MainActor
    func testAQrThatDoesNotDecodeIsNotShown() async {
        let recorder = Recorder { request in
            if request.url?.path == "/discovery/discovery/donate/revolut/qr" { return (200, Self.bigQr) }
            return (200, Self.list([["id": "revolut", "statement_b64": Self.revolut]]))
        }
        let model = DonationsModel(client: client(key: key, recorder))
        await model.load()
        XCTAssertNil(model.qrImages["revolut"])
        XCTAssertTrue(recorder.requests.contains { $0.url?.path == "/discovery/discovery/donate/revolut/qr" })
    }

    // MARK: - The crypto QR code

    func testEveryCoinMarkIsInTheAssetCatalog() {
        XCTAssertNil(DonationQr.markAsset(.none))
        for icon in [DonationIcon.btc, .eth, .ton] {
            let name = DonationQr.markAsset(icon)
            XCTAssertNotNil(name)
            XCTAssertNotNil(UIImage(named: name ?? ""), "asset \(name ?? "")")
        }
    }

    func testAPlainCodeClearsNothingAndAMarkClearsOnlyTheCentre() throws {
        let plain = try XCTUnwrap(DonationQr.plan(address: Self.btcAddress, icon: .none))
        XCTAssertNil(plain.markAsset)
        XCTAssertFalse(plain.cleared.contains(true))

        for icon in [DonationIcon.btc, .eth, .ton] {
            let marked = try XCTUnwrap(DonationQr.plan(address: Self.btcAddress, icon: icon))
            XCTAssertEqual(marked.size, plain.size)
            let n = marked.size
            let cleared = marked.cleared.filter { $0 }.count
            XCTAssertGreaterThan(cleared, 0)
            // Level H recovers 30% of the codewords; the mark stays far below.
            XCTAssertLessThan(Double(cleared) / Double(n * n), 0.08)
            for y in 0..<n {
                for x in 0..<n {
                    let finder = (x < 8 && y < 8) || (x >= n - 8 && y < 8) || (x < 8 && y >= n - 8)
                    if finder {
                        XCTAssertFalse(marked.cleared[y * n + x], "finder module \(x),\(y)")
                        XCTAssertEqual(marked.dark[y * n + x], plain.dark[y * n + x])
                    }
                    if marked.cleared[y * n + x] { XCTAssertFalse(marked.dark[y * n + x]) }
                }
            }
        }
    }

    /// The drawn code, mark included, scans back to the address.
    @MainActor
    func testTheDrawnCodeWithAMarkScansToTheAddress() throws {
        for icon in [DonationIcon.none, .btc, .eth, .ton] {
            let renderer = ImageRenderer(
                content: DonationQrView(address: Self.btcAddress, icon: icon).frame(width: 400, height: 400))
            renderer.scale = 2
            let image = try XCTUnwrap(renderer.cgImage)
            let detector = try XCTUnwrap(CIDetector(
                ofType: CIDetectorTypeQRCode, context: nil,
                options: [CIDetectorAccuracy: CIDetectorAccuracyHigh]))
            let found = detector.features(in: CIImage(cgImage: image))
                .compactMap { ($0 as? CIQRCodeFeature)?.messageString }
            XCTAssertEqual(found, [Self.btcAddress], "icon \(icon)")
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
