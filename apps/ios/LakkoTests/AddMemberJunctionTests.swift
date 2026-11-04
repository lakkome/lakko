// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
import LakkoCore
@testable import Lakko

/// ADR-0073 R.13b item A - the junction of the add-member filter (R.2) with
/// an ADR-0074 key rotation.
///
/// After a PROVEN rotation an account's kept devices are still bound to its
/// old key while the directory serves the new one. Every path that adds a
/// person's devices goes through `ChatRepository.expectedPersonDevices`: the
/// identity gate for the account first, then only the devices bound to the
/// expected key or a proven predecessor of it, and never one bound to a key
/// an accepted reset retired.
@MainActor
final class AddMemberJunctionTests: XCTestCase {

    /// Serves one account's device list.
    final class DirectoryStub: URLProtocol {
        private static let lock = NSLock()
        private static var body = Data()
        private static var log: [String] = []

        static func serve(_ devices: [(deviceId: Data, keyPackage: Data)]) {
            let payload: [String: Any] = [
                "keypackages": devices.map {
                    ["device_id": $0.deviceId.base64EncodedString(),
                     "keypackage": $0.keyPackage.base64EncodedString()]
                },
            ]
            lock.lock()
            body = (try? JSONSerialization.data(withJSONObject: payload)) ?? Data()
            log = []
            lock.unlock()
        }

        static var requests: [String] {
            lock.lock()
            defer { lock.unlock() }
            return log
        }

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
        override func stopLoading() {}

        override func startLoading() {
            Self.lock.lock()
            Self.log.append(request.url?.path ?? "")
            let body = Self.body
            Self.lock.unlock()
            guard let url = request.url,
                  let response = HTTPURLResponse(
                    url: url, statusCode: 200, httpVersion: "HTTP/1.1",
                    headerFields: ["Content-Type": "application/json"])
            else { return }
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: body)
            client?.urlProtocolDidFinishLoading(self)
        }
    }

    private struct Device {
        let deviceId: Data
        let keyPackage: Data
        /// base64 of the account key the KeyPackage is bound to.
        let boundKeyB64: String
    }

    /// A device bound to the account whose seed is `accountSeed`.
    private func device(_ tag: UInt8, accountSeed: Data) throws -> Device {
        let identity = try Identity.fromSeedBound(
            seed: Data(repeating: tag, count: 32), accountSeed: accountSeed, issuedAtUnixSecs: 1_700_000_000)
        let keyPackage = try identity.createKeyPackage()
        let bound = try XCTUnwrap(AddMemberKey.boundKeyB64(keyPackage: keyPackage))
        return Device(deviceId: identity.signaturePublicKey(), keyPackage: keyPackage, boundKeyB64: bound)
    }

    /// The account key the devices of `accountSeed` are bound to, as their
    /// own signed binding names it.
    private func accountKeyB64(_ accountSeed: Data) throws -> String {
        try device(0x7F, accountSeed: accountSeed).boundKeyB64
    }

    private func makeRepository() -> ChatRepository {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [DirectoryStub.self]
        let repository = ChatRepository(
            chatClient: Offline.chatClient(),
            dsClient: Offline.ds(),
            discovery: DiscoveryClient(
                baseURL: "http://directory.junction.test",
                session: URLSession(configuration: configuration),
                tokens: OfflineAnonTokens()))
        return repository
    }

    private let accountId = Data(repeating: 0x77, count: 16)
    private let oldSeed = Data(repeating: 0x01, count: 32)
    private let newSeed = Data(repeating: 0x02, count: 32)
    private let strangerSeed = Data(repeating: 0x03, count: 32)

    // MARK: - The pure rule

    func testTheBoundKeyOfADeviceIsItsAccountKey() throws {
        // Two devices of one account name the same key; another account's
        // device names another; and the key is not the device's own.
        let first = try device(0x10, accountSeed: oldSeed)
        let second = try device(0x11, accountSeed: oldSeed)
        let other = try device(0x12, accountSeed: newSeed)
        XCTAssertEqual(first.boundKeyB64, second.boundKeyB64)
        XCTAssertNotEqual(first.boundKeyB64, other.boundKeyB64)
        XCTAssertNotEqual(first.boundKeyB64, base64Encode(first.deviceId))
    }

    // MARK: - A rotated non-contact is added

    func testARotatedNonContactIsAdded() async throws {
        let oldKey = try accountKeyB64(oldSeed)
        let newKey = try accountKeyB64(newSeed)
        // The person rotated: a device kept across the rotation is still
        // bound to the OLD key, a device paired later to the NEW one. A
        // stranger's device sits in the same list (the directory is not
        // trusted to say whose devices it returned).
        let kept = try device(0x10, accountSeed: oldSeed)
        let pairedLater = try device(0x11, accountSeed: newSeed)
        let stranger = try device(0x12, accountSeed: strangerSeed)
        DirectoryStub.serve([kept, pairedLater, stranger].map { ($0.deviceId, $0.keyPackage) })

        let repository = makeRepository()
        // What this device knows before the add: nothing proven. The person
        // is not a contact; this device holds a verified binding to the old
        // key (it saw them as a group member).
        var proven = ProvenPredecessors()
        repository.provenPredecessorsProvider = { proven }
        var order: [String] = []
        repository.addIdentityGate = { gated in
            XCTAssertEqual(gated, self.accountId)
            order.append("gate")
            XCTAssertTrue(DirectoryStub.requests.isEmpty, "the gate runs before the devices are fetched")
            // What the fail-closed gate does for a binding whose key changed
            // by a verified supersession proof (`ContactsClient
            // .verifyBundleAccountKey`): the binding becomes a pin and the
            // old key the account's proven predecessor.
            proven.record(oldKeyB64: oldKey, newKeyB64: newKey, evidence: .verifiedProof)
        }

        // The caller still names the key it knows the person by: the old one.
        let added = try await repository.expectedPersonDevices(
            accountId: accountId, expectedKey: try XCTUnwrap(base64Decode(oldKey)))
        order.append("fetched")

        XCTAssertEqual(order, ["gate", "fetched"])
        XCTAssertEqual(DirectoryStub.requests.count, 1)
        XCTAssertEqual(
            Set(added.map { base64Encode($0.deviceId) }),
            Set([kept, pairedLater].map { base64Encode($0.deviceId) }),
            "both the kept device (old key) and the one paired later (new key) are the same person")
        XCTAssertFalse(added.contains { $0.deviceId == stranger.deviceId })
    }

    func testTheSamePersonIsAddedWhenTheCallerNamesTheNewKey() async throws {
        let oldKey = try accountKeyB64(oldSeed)
        let newKey = try accountKeyB64(newSeed)
        let kept = try device(0x10, accountSeed: oldSeed)
        let pairedLater = try device(0x11, accountSeed: newSeed)
        DirectoryStub.serve([kept, pairedLater].map { ($0.deviceId, $0.keyPackage) })
        let repository = makeRepository()
        var proven = ProvenPredecessors()
        repository.provenPredecessorsProvider = { proven }
        repository.addIdentityGate = { _ in
            proven.record(oldKeyB64: oldKey, newKeyB64: newKey, evidence: .verifiedProof)
        }
        let added = try await repository.expectedPersonDevices(
            accountId: accountId, expectedKey: try XCTUnwrap(base64Decode(newKey)))
        XCTAssertEqual(added.count, 2)
    }

    func testWithoutAProvenRotationOnlyTheExpectedKeysDevicesAreAdded() async throws {
        let oldKey = try accountKeyB64(oldSeed)
        let kept = try device(0x10, accountSeed: oldSeed)
        let other = try device(0x11, accountSeed: newSeed)
        DirectoryStub.serve([kept, other].map { ($0.deviceId, $0.keyPackage) })
        let repository = makeRepository()
        repository.provenPredecessorsProvider = { ProvenPredecessors() }
        repository.addIdentityGate = { _ in }
        let added = try await repository.expectedPersonDevices(
            accountId: accountId, expectedKey: try XCTUnwrap(base64Decode(oldKey)))
        XCTAssertEqual(added.map(\.deviceId), [kept.deviceId])
    }

    // MARK: - A reset-retired key is refused

    func testADeviceBoundToAResetRetiredKeyIsRefused() async throws {
        let retiredKey = try accountKeyB64(oldSeed)
        let newKey = try accountKeyB64(newSeed)
        // An accepted RESET: the old key is retired, not a predecessor. The
        // reset forgets the chain, so no proven edge leads from it.
        let leftBehind = try device(0x10, accountSeed: oldSeed)
        let afterReset = try device(0x11, accountSeed: newSeed)
        DirectoryStub.serve([leftBehind, afterReset].map { ($0.deviceId, $0.keyPackage) })
        let repository = makeRepository()
        repository.provenPredecessorsProvider = { ProvenPredecessors() }
        repository.addIdentityGate = { _ in }

        let added = try await repository.expectedPersonDevices(
            accountId: accountId, expectedKey: try XCTUnwrap(base64Decode(newKey)))
        XCTAssertEqual(added.map(\.deviceId), [afterReset.deviceId])
        XCTAssertFalse(
            AddMemberKey.acceptableBoundKeys(expectedKeyB64: newKey, proven: ProvenPredecessors())
                .contains(retiredKey))
    }

    func testAnUnprovenChangeIsNeverAProvenPredecessor() throws {
        let retiredKey = try accountKeyB64(oldSeed)
        let newKey = try accountKeyB64(newSeed)
        var proven = ProvenPredecessors()
        // An accepted reset is not a verified proof: nothing is recorded, so
        // the retired key is not accepted through the proven-chain row.
        XCTAssertFalse(proven.record(oldKeyB64: retiredKey, newKeyB64: newKey, evidence: .attestationOnly))
        XCTAssertEqual(AddMemberKey.acceptableBoundKeys(expectedKeyB64: newKey, proven: proven), [newKey])
    }

    func testARefusedIdentityAddsNothingAndFetchesNothing() async throws {
        struct Refused: Error {}
        let kept = try device(0x10, accountSeed: oldSeed)
        DirectoryStub.serve([(kept.deviceId, kept.keyPackage)])
        let repository = makeRepository()
        repository.addIdentityGate = { _ in throw Refused() }
        do {
            _ = try await repository.expectedPersonDevices(
                accountId: accountId, expectedKey: try XCTUnwrap(base64Decode(try accountKeyB64(oldSeed))))
            XCTFail("a change that is neither proven nor an attested reset refuses the add")
        } catch is Refused {
        }
        XCTAssertTrue(DirectoryStub.requests.isEmpty)
    }

    func testACallerThatJustRanTheGateDoesNotRunItAgain() async throws {
        let kept = try device(0x10, accountSeed: oldSeed)
        DirectoryStub.serve([(kept.deviceId, kept.keyPackage)])
        let repository = makeRepository()
        repository.provenPredecessorsProvider = { ProvenPredecessors() }
        var gated = 0
        repository.addIdentityGate = { _ in gated += 1 }
        let added = try await repository.expectedPersonDevices(
            accountId: accountId, expectedKey: try XCTUnwrap(base64Decode(kept.boundKeyB64)),
            gateAlreadyRan: true)
        XCTAssertEqual(gated, 0)
        XCTAssertEqual(added.count, 1)
    }

    // MARK: - Every add path goes through the same filter

    /// The repository's sources, read from the checkout the test was built
    /// from (the simulator shares the host's file system).
    private func chatSource(_ name: String, file: StaticString = #filePath) throws -> String {
        let tests = URL(fileURLWithPath: "\(file)").deletingLastPathComponent()
        let url = tests.deletingLastPathComponent().appendingPathComponent("Lakko/Chat/\(name)")
        return try String(contentsOf: url, encoding: .utf8)
    }

    /// The body of the function whose declaration contains `signature`.
    private func body(of signature: String, in source: String) throws -> String {
        let start = try XCTUnwrap(source.range(of: signature), "no \(signature)")
        let rest = source[start.lowerBound...]
        let end = rest.dropFirst(signature.count).range(of: "\n    }\n")?.lowerBound ?? rest.endIndex
        return String(rest[..<end])
    }

    func testEveryAddPathGoesThroughTheSameFilter() throws {
        let repository = try chatSource("ChatRepository.swift")
        let continuity = try chatSource("ChatRepository+Continuity.swift")
        let lineage = try chatSource("ChatRepository+Lineage.swift")

        // Another person's device list is fetched in ONE place: the funnel.
        // The four other fetches read THIS account's own list (the founder's
        // other devices, the pairing fan-out, the re-admission arm's own
        // row, an own-device re-request).
        let funnel = try body(of: "func expectedPersonDevices(", in: repository)
        XCTAssertTrue(funnel.contains("discovery.fetchKeyPackages(accountId: accountId)"))
        XCTAssertTrue(funnel.contains("devicesOfExpectedPerson(listed, expectedKey: expectedKey)"))
        XCTAssertTrue(funnel.contains("checkContactIdentity(accountId: accountId)"))
        let gate = try XCTUnwrap(funnel.range(of: "if !gateAlreadyRan"))
        let fetch = try XCTUnwrap(funnel.range(of: "discovery.fetchKeyPackages("))
        XCTAssertTrue(gate.lowerBound < fetch.lowerBound, "the gate runs before the fetch")

        let ownAccountFetchers = [
            "func ownOtherDevices(",
            "private func awaitDeviceKeyPackage(",
            "func readmitDevicesIfDue(",
            "private func performReRequest(",
        ]
        var expected = 1
        for signature in ownAccountFetchers {
            let text = try body(of: signature, in: repository)
            XCTAssertEqual(
                text.components(separatedBy: "discovery.fetchKeyPackages(").count - 1, 1,
                "\(signature) reads this account's own list once")
            expected += 1
        }
        XCTAssertEqual(
            repository.components(separatedBy: "fetchKeyPackages(").count - 1, expected,
            "a new fetch of a device list must go through expectedPersonDevices")
        XCTAssertFalse(continuity.contains("fetchKeyPackages("))
        XCTAssertFalse(lineage.contains("fetchKeyPackages("))

        // ...and every path that adds another person calls it.
        for signature in ["private func foundGroup(", "public func addMemberToGroup(", "func repairResetMember("] {
            XCTAssertTrue(
                try body(of: signature, in: repository).contains("expectedPersonDevices("),
                "\(signature) adds devices that did not pass the filter")
        }
        XCTAssertTrue(try body(of: "func readmitDevicesIfDue(", in: repository).contains("expectedPersonDevices("))
        XCTAssertTrue(try body(of: "func prepareSuccessor(", in: continuity).contains("expectedPersonDevices("))
        // The ADR-0050 membership re-request of an older add rides the
        // user-facing add.
        XCTAssertTrue(try body(of: "private func performReRequest(", in: repository).contains("addMemberToGroup("))
    }
}
