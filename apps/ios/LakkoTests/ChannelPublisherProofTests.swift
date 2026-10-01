// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import XCTest
@testable import Lakko
import LakkoCore

// ADR-0059 section 9 (G-43): the five publisher writes and the subscriber
// list carry a publisher proof over the service path, the query without `?`
// and the exact body bytes sent, and the client learns its role per
// (account, channel) once.

private let channelId = "0123456789abcdef0123456789abcdef"
private let refusalBody = Data(#"{"code":"owner-proof-required","detail":"this route needs a valid channel owner proof"}"#.utf8)

/// A signing seam that records what it was asked to sign and hands out a
/// fresh proof value every time.
private final class RecordingSigning: ChannelPublisherSigning {
    struct Signed { let role: ChannelPublisherRole; let method: String; let path: String; let query: String; let body: Data }
    var signed: [Signed] = []
    let key: String
    init(key: String = "role-key") { self.key = key }

    func roleKey() throws -> String { key }

    func proof(
        role: ChannelPublisherRole, method: String, path: String, query: String, body: Data
    ) throws -> (name: String, value: String) {
        signed.append(Signed(role: role, method: method, path: path, query: query, body: body))
        let name = role == .owner ? "x-test-owner-proof" : "x-test-copub-proof"
        return (name, "proof-\(signed.count)")
    }
}

/// A transport that records every request and answers from a queue of
/// `(status, body)` replies.
private final class RecordingTransport: @unchecked Sendable {
    var requests: [URLRequest] = []
    var replies: [(Int, Data)]
    init(_ replies: [(Int, Data)]) { self.replies = replies }

    func perform(_ request: URLRequest) async throws -> (Data, URLResponse) {
        requests.append(request)
        let (status, body) = replies.isEmpty ? (500, Data()) : replies.removeFirst()
        let response = HTTPURLResponse(
            url: request.url!, statusCode: status, httpVersion: nil,
            headerFields: ["Content-Type": "application/json"])!
        return (body, response)
    }
}

private func ok(_ json: String = "{}") -> (Int, Data) { (200, Data(json.utf8)) }
private let refused: (Int, Data) = (401, refusalBody)

private func client(
    base: String = "https://api.example/channels",
    roles: ChannelPublisherRoleCache = ChannelPublisherRoleCache(),
    transport: RecordingTransport
) -> ChannelsClient {
    ChannelsClient(baseURL: base, roles: roles, transport: { try await transport.perform($0) })
}

final class ChannelPublisherProofTests: XCTestCase {

    // MARK: - What is signed is what is sent

    func testSignedBytesAreTheBytesSent() async throws {
        let transport = RecordingTransport([ok(#"{"epoch":3}"#)])
        let signing = RecordingSigning()
        let c = client(transport: transport)
        let body = Data(#"{"target_epoch":3}"#.utf8)
        struct View: Decodable { let epoch: UInt32 }
        let view: View = try await c.sendAsPublisher(
            signing: signing, method: "POST", path: "/v1/channels/\(channelId)/rotate/commit",
            query: "", body: body, contentType: "application/json")
        XCTAssertEqual(view.epoch, 3)
        XCTAssertEqual(signing.signed.count, 1)
        XCTAssertEqual(transport.requests.count, 1)
        let request = transport.requests[0]
        XCTAssertEqual(request.httpBody, signing.signed[0].body)
        XCTAssertEqual(request.httpBody, body)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(signing.signed[0].method, "POST")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
        XCTAssertEqual(request.value(forHTTPHeaderField: "x-test-owner-proof"), "proof-1")
    }

    func testRealPublishSendsThePayloadUnderTheOwnerHeader() async throws {
        let transport = RecordingTransport([ok(#"{"cursor":1,"server_ts_ms":2,"payload_b64":"AA"}"#)])
        let c = client(transport: transport)
        let identity = try Identity.fromSeed(seed: Data(repeating: 7, count: 32))
        let payload = Data([1, 2, 3, 4, 5])
        _ = try await c.publishPost(channelIdHex: channelId, payload: payload, accountIdentity: identity)
        let request = try XCTUnwrap(transport.requests.first)
        XCTAssertEqual(request.httpBody, payload)
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/octet-stream")
        XCTAssertNotNil(request.value(forHTTPHeaderField: channelOwnerProofHeaderName()))
        XCTAssertNil(request.value(forHTTPHeaderField: channelCopublisherProofHeaderName()))
    }

    func testRealStageEncodesTheJsonOnceAndSendsThoseBytes() async throws {
        let transport = RecordingTransport([ok(#"{"target_epoch":2,"staged":1}"#)])
        let c = client(transport: transport)
        let identity = try Identity.fromSeed(seed: Data(repeating: 9, count: 32))
        let staged = try await c.stageRotation(
            channelIdHex: channelId, targetEpoch: 2,
            batch: [RotateBatchEntry(subscriberPubkeyHex: "aa", envelopeB64: "bb")],
            accountIdentity: identity)
        XCTAssertEqual(staged, 1)
        let body = try XCTUnwrap(transport.requests.first?.httpBody)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
        XCTAssertEqual(json["target_epoch"] as? Int, 2)
    }

    // MARK: - Path and query

    func testSignedPathHasNoProxyPrefixWhileTheUrlDoes() async throws {
        let transport = RecordingTransport([ok(#"{"subscribers":[]}"#)])
        let signing = RecordingSigning()
        let c = client(base: "https://api.example/channels", transport: transport)
        struct Resp: Decodable { let subscribers: [String] }
        let _: Resp = try await c.sendAsPublisher(
            signing: signing, method: "GET", path: "/v1/channels/\(channelId)/subscribers",
            query: "", body: Data(), contentType: nil)
        XCTAssertEqual(signing.signed[0].path, "/v1/channels/\(channelId)/subscribers")
        XCTAssertEqual(
            transport.requests[0].url?.absoluteString,
            "https://api.example/channels/v1/channels/\(channelId)/subscribers")
        XCTAssertNil(transport.requests[0].httpBody)
        XCTAssertEqual(signing.signed[0].body, Data())
    }

    func testDeliverSignsTheQueryWithoutQuestionMark() async throws {
        let transport = RecordingTransport([(204, Data())])
        let signing = RecordingSigning()
        let c = client(transport: transport)
        let envelope = Data(repeating: 0xab, count: 12)
        let _: EmptyResponse = try await c.sendAsPublisher(
            signing: signing, method: "POST",
            path: "/v1/channels/\(channelId)/subscribers/cafe/envelope",
            query: "epoch=7", body: envelope, contentType: "application/octet-stream")
        XCTAssertEqual(signing.signed[0].query, "epoch=7")
        XCTAssertEqual(transport.requests[0].url?.query, "epoch=7")
        XCTAssertTrue(transport.requests[0].url!.absoluteString.hasSuffix("/envelope?epoch=7"))
        XCTAssertEqual(transport.requests[0].httpBody, envelope)
    }

    // MARK: - The role cache

    func testUnknownRoleTriesOwnerThenCopublisherAndRemembersIt() async throws {
        let roles = ChannelPublisherRoleCache()
        let transport = RecordingTransport([refused, ok(), ok()])
        let signing = RecordingSigning()
        let c = client(roles: roles, transport: transport)
        let _: EmptyResponse = try await c.sendAsPublisher(
            signing: signing, method: "POST", path: "/v1/channels/\(channelId)/posts",
            query: "", body: Data([1]), contentType: "application/octet-stream")
        XCTAssertEqual(signing.signed.map(\.role), [.owner, .copublisher])
        // A fresh proof per attempt: the server's nonces are single use.
        XCTAssertEqual(transport.requests[0].value(forHTTPHeaderField: "x-test-owner-proof"), "proof-1")
        XCTAssertEqual(transport.requests[1].value(forHTTPHeaderField: "x-test-copub-proof"), "proof-2")
        XCTAssertEqual(roles.role(forKey: signing.key), .copublisher)

        // Known now: one request, signed once, as the co-publisher.
        let _: EmptyResponse = try await c.sendAsPublisher(
            signing: signing, method: "POST", path: "/v1/channels/\(channelId)/posts",
            query: "", body: Data([2]), contentType: "application/octet-stream")
        XCTAssertEqual(transport.requests.count, 3)
        XCTAssertEqual(signing.signed.last?.role, .copublisher)
    }

    func testUnknownRoleAcceptedAsOwnerIsRemembered() async throws {
        let roles = ChannelPublisherRoleCache()
        let transport = RecordingTransport([ok()])
        let signing = RecordingSigning()
        let c = client(roles: roles, transport: transport)
        let _: EmptyResponse = try await c.sendAsPublisher(
            signing: signing, method: "POST", path: "/v1/channels/\(channelId)/rotate",
            query: "", body: Data("{}".utf8), contentType: "application/json")
        XCTAssertEqual(transport.requests.count, 1)
        XCTAssertEqual(roles.role(forKey: signing.key), .owner)
    }

    func testKnownRoleRefusedClearsTheCacheAndSurfacesWithoutRetry() async throws {
        let roles = ChannelPublisherRoleCache()
        roles.set(.owner, forKey: "role-key")
        let transport = RecordingTransport([refused])
        let signing = RecordingSigning()
        let c = client(roles: roles, transport: transport)
        do {
            let _: EmptyResponse = try await c.sendAsPublisher(
                signing: signing, method: "POST", path: "/v1/channels/\(channelId)/posts",
                query: "", body: Data([1]), contentType: "application/octet-stream")
            XCTFail("expected the refusal")
        } catch let err as ChannelsApiError {
            XCTAssertEqual(err.status, 401)
            XCTAssertEqual(err.code, ChannelsApiError.publisherRefusedCode)
            XCTAssertTrue(ChannelsApiError.isPublisherRefusal(err))
            XCTAssertEqual(ChannelsErrorCopy.key(forCode: err.code), "channels.error.publisher_refused")
        }
        XCTAssertEqual(transport.requests.count, 1)
        XCTAssertNil(roles.role(forKey: "role-key"))
    }

    func testOwnerFailureOtherThan401IsNotRetriedAsCopublisher() async throws {
        let roles = ChannelPublisherRoleCache()
        let transport = RecordingTransport([(409, Data(#"{"code":"stale-target-epoch","detail":"x"}"#.utf8))])
        let signing = RecordingSigning()
        let c = client(roles: roles, transport: transport)
        do {
            let _: EmptyResponse = try await c.sendAsPublisher(
                signing: signing, method: "POST", path: "/v1/channels/\(channelId)/rotate/commit",
                query: "", body: Data("{}".utf8), contentType: "application/json")
            XCTFail("expected the conflict")
        } catch let err as ChannelsApiError {
            XCTAssertEqual(err.code, "stale-target-epoch")
            XCTAssertFalse(ChannelsApiError.isPublisherRefusal(err))
        }
        XCTAssertEqual(transport.requests.count, 1)
        XCTAssertNil(roles.role(forKey: signing.key))
    }

    func testBothProofsRefusedLeavesTheRoleUnknown() async throws {
        let roles = ChannelPublisherRoleCache()
        let transport = RecordingTransport([refused, refused])
        let signing = RecordingSigning()
        let c = client(roles: roles, transport: transport)
        do {
            let _: EmptyResponse = try await c.sendAsPublisher(
                signing: signing, method: "POST", path: "/v1/channels/\(channelId)/posts",
                query: "", body: Data([1]), contentType: "application/octet-stream")
            XCTFail("expected the refusal")
        } catch let err as ChannelsApiError {
            XCTAssertEqual(err.code, ChannelsApiError.publisherRefusedCode)
        }
        XCTAssertEqual(transport.requests.count, 2)
        XCTAssertNil(roles.role(forKey: signing.key))
    }

    func testCreatingTheChannelRecordsTheOwnerRoleUnderTheSigningKey() async throws {
        let roles = ChannelPublisherRoleCache()
        let transport = RecordingTransport([ok(#"{"channel_id":"\#(channelId)"}"#)])
        let c = client(roles: roles, transport: transport)
        let identity = try Identity.fromSeed(seed: Data(repeating: 3, count: 32))
        _ = try await c.createChannel(channelIdHex: channelId, accountIdentity: identity)
        let key = try IdentityPublisherSigning(accountIdentity: identity, channelIdHex: channelId).roleKey()
        XCTAssertEqual(roles.role(forKey: key), .owner)
    }

    func testRoleKeyIsPerAccount() throws {
        let a = try Identity.fromSeed(seed: Data(repeating: 1, count: 32))
        let b = try Identity.fromSeed(seed: Data(repeating: 2, count: 32))
        let keyA = try IdentityPublisherSigning(accountIdentity: a, channelIdHex: channelId).roleKey()
        let keyB = try IdentityPublisherSigning(accountIdentity: b, channelIdHex: channelId).roleKey()
        XCTAssertNotEqual(keyA, keyB)
        XCTAssertEqual(keyA.count, 64)
    }

    func testRealProofsUseTheRoleHeaderAndAreFreshEachTime() throws {
        let identity = try Identity.fromSeed(seed: Data(repeating: 5, count: 32))
        let signing = IdentityPublisherSigning(accountIdentity: identity, channelIdHex: channelId)
        let path = "/v1/channels/\(channelId)/posts"
        let first = try signing.proof(role: .owner, method: "POST", path: path, query: "", body: Data([1]))
        let second = try signing.proof(role: .owner, method: "POST", path: path, query: "", body: Data([1]))
        XCTAssertEqual(first.name, channelOwnerProofHeaderName())
        XCTAssertNotEqual(first.value, second.value)
        let copub = try signing.proof(role: .copublisher, method: "POST", path: path, query: "", body: Data([1]))
        XCTAssertEqual(copub.name, channelCopublisherProofHeaderName())
    }
}
