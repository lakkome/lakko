// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0087 sections 6, 7, 13a and 15 — the channels routes a channel as a
/// conversation needs: the count, the public seed, the invite keys and the
/// private flag, the paged subscriber list with its tickets, a publisher
/// removing a subscriber, and the reactions and comments under a post.
extension ChannelsClient {

    // MARK: - Section 15: the count

    /// `GET /v1/channels/:id/subscribers/count` (publisher proof).
    public func subscriberCount(channelIdHex: String, accountIdentity: Identity) async throws -> UInt64 {
        struct Resp: Decodable { let count: UInt64 }
        let resp: Resp = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "GET", path: "/v1/channels/\(channelIdHex)/subscribers/count",
            query: "", body: Data(), contentType: nil)
        return resp.count
    }

    // MARK: - Section 7: private flag, invite keys, the paged list

    /// `PUT /v1/channels/:id/access` (owner proof) — set the private flag.
    public func setPrivate(channelIdHex: String, isPrivate: Bool, accountIdentity: Identity) async throws {
        let body = try encoder.encode(["private": isPrivate])
        let _: EmptyResponse = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "PUT", path: "/v1/channels/\(channelIdHex)/access",
            query: "", body: body, contentType: "application/json")
    }

    /// `PUT /v1/channels/:id/invite-keys/:pk` (publisher proof) — register a
    /// link's invite key. `maxUses`: `0` one use, `UInt32.max` unlimited.
    public func putInviteKey(
        channelIdHex: String, invitePublicKey: Data, expiryMs: UInt64, maxUses: UInt32,
        accountIdentity: Identity
    ) async throws {
        struct Req: Encodable { let expiry_ms: UInt64; let max_uses: UInt32 }
        let body = try encoder.encode(Req(expiry_ms: expiryMs, max_uses: maxUses))
        let _: EmptyResponse = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "PUT",
            path: "/v1/channels/\(channelIdHex)/invite-keys/\(ChannelsComposer.bytesToHex(invitePublicKey))",
            query: "", body: body, contentType: "application/json")
    }

    /// `DELETE /v1/channels/:id/invite-keys/:pk` (publisher proof).
    public func deleteInviteKey(channelIdHex: String, invitePublicKey: Data, accountIdentity: Identity) async throws {
        let _: EmptyResponse = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "DELETE",
            path: "/v1/channels/\(channelIdHex)/invite-keys/\(ChannelsComposer.bytesToHex(invitePublicKey))",
            query: "", body: Data(), contentType: nil)
    }

    /// `GET /v1/channels/:id/invite-keys` (publisher proof) — the live invite
    /// public keys.
    public func inviteKeys(channelIdHex: String, accountIdentity: Identity) async throws -> [Data] {
        struct Resp: Decodable {
            struct Key: Decodable { let invite_pk_hex: String }
            let invite_keys: [Key]
        }
        let resp: Resp = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "GET", path: "/v1/channels/\(channelIdHex)/invite-keys",
            query: "", body: Data(), contentType: nil)
        return resp.invite_keys.compactMap { ChannelsClient.hexToData($0.invite_pk_hex) }
    }

    /// One row of the paged subscriber list (section 7).
    public struct SubscriberRow: Equatable, Sendable, Identifiable {
        public var id: String { pubkeyHex }
        public let seq: UInt64
        public let pubkeyHex: String
        public let sealedTicket: Data?
    }

    /// `GET /v1/channels/:id/subscribers?after=&limit=` (publisher proof).
    public func subscriberPage(
        channelIdHex: String, after: UInt64, accountIdentity: Identity
    ) async throws -> (rows: [SubscriberRow], nextAfter: UInt64) {
        struct Resp: Decodable {
            struct Entry: Decodable {
                let seq: UInt64
                let subscriber_id: String
                let sealed_ticket_b64: String?
            }
            let entries: [Entry]?
            let subscribers: [String]
            let next_after: UInt64?
        }
        let query = "after=\(after)"
        let resp: Resp = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "GET", path: "/v1/channels/\(channelIdHex)/subscribers",
            query: query, body: Data(), contentType: nil)
        let rows: [SubscriberRow]
        if let entries = resp.entries {
            rows = entries.map {
                SubscriberRow(
                    seq: $0.seq, pubkeyHex: $0.subscriber_id,
                    sealedTicket: $0.sealed_ticket_b64.flatMap(base64UrlDecodeNoPad))
            }
        } else {
            rows = resp.subscribers.enumerated().map {
                SubscriberRow(seq: after + UInt64($0.offset) + 1, pubkeyHex: $0.element, sealedTicket: nil)
            }
        }
        return (rows, resp.next_after ?? rows.last?.seq ?? after)
    }

    /// `DELETE /v1/channels/:id/subscribers/:pk` with a publisher proof
    /// (section 7.3): remove a subscriber of a private channel. A rotation
    /// follows, by the caller.
    public func removeSubscriber(channelIdHex: String, pubkeyHex: String, accountIdentity: Identity) async throws {
        do {
            let _: EmptyResponse = try await sendAsPublisher(
                signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
                method: "DELETE", path: "/v1/channels/\(channelIdHex)/subscribers/\(pubkeyHex)",
                query: "", body: Data(), contentType: nil)
        } catch let error as ChannelsApiError where error.status == 404 {
            return
        }
    }

    /// `POST /v1/channels/:id/subscribers` on a private channel: one
    /// `channel-subscribe` token, the invite proof and the sealed ticket.
    @discardableResult
    public func subscribePrivate(
        channelIdHex: String, subscriberPubkeyHex: String, inviteProof: String, sealedTicket: Data
    ) async throws -> String {
        struct Req: Encodable { let subscriber_pubkey_hex: String; let sealed_ticket_b64: String }
        struct Resp: Decodable { let subscriber_id: String }
        let url = try buildURL(endpoint: "/v1/channels/\(channelIdHex)/subscribers")
        let body = try encoder.encode(Req(
            subscriber_pubkey_hex: subscriberPubkeyHex,
            sealed_ticket_b64: base64UrlEncodeNoPad(sealedTicket)))
        let anonymousSession = self.anonymousSession
        let (data, http) = try await AnonTokenRoute.send(context: .channelSubscribe, tokens: tokens) { tokenHeader in
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue(tokenHeader, forHTTPHeaderField: AnonTokenRoute.headerName)
            request.setValue(inviteProof, forHTTPHeaderField: channelInviteProofHeaderName())
            request.httpBody = body
            let (data, response) = try await channelsPerform(
                request, session: anonymousSession, config: .current)
            guard let http = response as? HTTPURLResponse else {
                throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
            }
            return (data, http)
        }
        let resp: Resp = try decode(data: data, http: http)
        return resp.subscriber_id
    }

    // MARK: - Section 6: the public seed

    /// `PUT /v1/channels/:id/public-seed/:epoch` (publisher proof).
    public func putPublicSeed(
        channelIdHex: String, epoch: UInt32, envelope: Data, accountIdentity: Identity
    ) async throws {
        let _: EmptyResponse = try await sendAsPublisher(
            signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
            method: "PUT", path: "/v1/channels/\(channelIdHex)/public-seed/\(epoch)",
            query: "", body: envelope, contentType: "application/octet-stream")
    }

    /// `GET /v1/channels/:id/public-seed/:epoch` — open, immutable. `nil`
    /// when none is stored for the epoch.
    public func fetchPublicSeed(channelIdHex: String, epoch: UInt32) async throws -> Data? {
        var request = URLRequest(url: try buildURL(endpoint: "/v1/channels/\(channelIdHex)/public-seed/\(epoch)"))
        request.httpMethod = "GET"
        let (data, response) = try await channelsPerform(
            request, session: anonymousSession, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        if http.statusCode == 404 { return nil }
        guard (200..<300).contains(http.statusCode) else {
            let _: EmptyResponse = try decode(data: data, http: http)
            return nil
        }
        return data
    }

    // MARK: - Section 13a: reactions

    /// One post's label counts.
    public struct PostLabelCounts: Equatable, Sendable {
        public let cursor: UInt64
        public let labels: [FfiReactionLabelCount]
        /// The post's comment count (ADR-0087 phase B, the service's own
        /// count); `nil` from a service that does not send it.
        public var comments: UInt64? = nil
    }

    /// `GET /v1/channels/:id/reactions?posts=` — up to 50 posts a call, no
    /// session, no token.
    public func reactionCounts(channelIdHex: String, posts: [UInt64]) async throws -> [PostLabelCounts] {
        struct Resp: Decodable {
            struct Post: Decodable {
                struct Label: Decodable { let label_hex: String; let count: UInt64 }
                let cursor: UInt64
                let labels: [Label]
                let comments: UInt64?
            }
            let posts: [Post]
        }
        guard !posts.isEmpty else { return [] }
        var request = URLRequest(url: try buildURL(
            endpoint: "/v1/channels/\(channelIdHex)/reactions?posts="
                + posts.prefix(50).map(String.init).joined(separator: ",")))
        request.httpMethod = "GET"
        let (data, response) = try await channelsPerform(
            request, session: anonymousSession, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        let resp: Resp = try decode(data: data, http: http)
        return resp.posts.map { post in
            PostLabelCounts(
                cursor: post.cursor,
                labels: post.labels.compactMap { label in
                    ChannelsClient.hexToData(label.label_hex).map {
                        FfiReactionLabelCount(label: $0, count: label.count)
                    }
                },
                comments: post.comments)
        }
    }

    // MARK: - Section 13a (phase B): reactions on comments

    /// `GET .../posts/:post/comment-reactions?comments=` — up to 50 comments.
    public func commentReactionCounts(
        channelIdHex: String, postCursor: UInt64, comments: [UInt64]
    ) async throws -> [PostLabelCounts] {
        struct Resp: Decodable {
            struct Comment: Decodable {
                struct Label: Decodable { let label_hex: String; let count: UInt64 }
                let cursor: UInt64
                let labels: [Label]
            }
            let comments: [Comment]
        }
        guard !comments.isEmpty else { return [] }
        var request = URLRequest(url: try buildURL(
            endpoint: "/v1/channels/\(channelIdHex)/posts/\(postCursor)/comment-reactions?comments="
                + comments.prefix(50).map(String.init).joined(separator: ",")))
        request.httpMethod = "GET"
        let (data, response) = try await channelsPerform(
            request, session: anonymousSession, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        let resp: Resp = try decode(data: data, http: http)
        return resp.comments.map { comment in
            PostLabelCounts(cursor: comment.cursor, labels: comment.labels.compactMap { label in
                ChannelsClient.hexToData(label.label_hex).map { FfiReactionLabelCount(label: $0, count: label.count) }
            })
        }
    }

    /// `POST .../posts/:post/comments/:comment/reactions` — one
    /// `channel-react` token and the write proof of the reaction's own key.
    public func addCommentReaction(
        channelIdHex: String, postCursor: UInt64, commentCursor: UInt64,
        writeKey: ChannelCommentReactionWriteKey, label: Data
    ) async throws {
        struct Req: Encodable { let writer_key_hex: String; let label_hex: String }
        let body = try encoder.encode(Req(
            writer_key_hex: ChannelsComposer.bytesToHex(writeKey.publicKey()),
            label_hex: ChannelsComposer.bytesToHex(label)))
        let url = try buildURL(
            endpoint: "/v1/channels/\(channelIdHex)/posts/\(postCursor)/comments/\(commentCursor)/reactions")
        let anonymousSession = self.anonymousSession
        let (data, http) = try await AnonTokenRoute.send(context: .channelReact, tokens: tokens) { header in
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue(header, forHTTPHeaderField: AnonTokenRoute.headerName)
            request.setValue(writeKey.signAdd(), forHTTPHeaderField: channelWriteProofHeaderName())
            request.httpBody = body
            let (data, response) = try await channelsPerform(
                request, session: anonymousSession, config: .current)
            guard let http = response as? HTTPURLResponse else {
                throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
            }
            return (data, http)
        }
        let _: EmptyResponse = try decode(data: data, http: http)
    }

    /// `DELETE .../posts/:post/comments/:comment/reactions/:writer/:label` — no token.
    public func deleteCommentReaction(
        channelIdHex: String, postCursor: UInt64, commentCursor: UInt64,
        writeKey: ChannelCommentReactionWriteKey, label: Data
    ) async throws {
        let path = "/v1/channels/\(channelIdHex)/posts/\(postCursor)/comments/\(commentCursor)/reactions/"
            + "\(ChannelsComposer.bytesToHex(writeKey.publicKey()))/\(ChannelsComposer.bytesToHex(label))"
        var request = URLRequest(url: try buildURL(endpoint: path))
        request.httpMethod = "DELETE"
        request.setValue(writeKey.signDelete(), forHTTPHeaderField: channelWriteProofHeaderName())
        let (data, response) = try await channelsPerform(
            request, session: anonymousSession, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        let _: EmptyResponse = try decode(data: data, http: http)
    }

    /// `POST .../posts/:cursor/reactions` — one `channel-react` token and the
    /// write proof of the reaction's own key.
    public func addReaction(
        channelIdHex: String, postCursor: UInt64, writeKey: ChannelReactionWriteKey, label: Data
    ) async throws {
        struct Req: Encodable { let writer_key_hex: String; let label_hex: String }
        let body = try encoder.encode(Req(
            writer_key_hex: ChannelsComposer.bytesToHex(writeKey.publicKey()),
            label_hex: ChannelsComposer.bytesToHex(label)))
        let url = try buildURL(endpoint: "/v1/channels/\(channelIdHex)/posts/\(postCursor)/reactions")
        let anonymousSession = self.anonymousSession
        let (data, http) = try await AnonTokenRoute.send(context: .channelReact, tokens: tokens) { header in
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue(header, forHTTPHeaderField: AnonTokenRoute.headerName)
            // Each attempt signs anew: the nonce is single use.
            request.setValue(writeKey.signAdd(), forHTTPHeaderField: channelWriteProofHeaderName())
            request.httpBody = body
            let (data, response) = try await channelsPerform(
                request, session: anonymousSession, config: .current)
            guard let http = response as? HTTPURLResponse else {
                throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
            }
            return (data, http)
        }
        let _: EmptyResponse = try decode(data: data, http: http)
    }

    /// `DELETE .../posts/:cursor/reactions/:writer/:label` — no token.
    public func deleteReaction(
        channelIdHex: String, postCursor: UInt64, writeKey: ChannelReactionWriteKey, label: Data
    ) async throws {
        let path = "/v1/channels/\(channelIdHex)/posts/\(postCursor)/reactions/"
            + "\(ChannelsComposer.bytesToHex(writeKey.publicKey()))/\(ChannelsComposer.bytesToHex(label))"
        var request = URLRequest(url: try buildURL(endpoint: path))
        request.httpMethod = "DELETE"
        request.setValue(writeKey.signDelete(), forHTTPHeaderField: channelWriteProofHeaderName())
        let (data, response) = try await channelsPerform(
            request, session: anonymousSession, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        let _: EmptyResponse = try decode(data: data, http: http)
    }

    // MARK: - Section 13a: comments

    /// One stored comment, still sealed.
    public struct CommentRow: Equatable, Sendable {
        public let cursor: UInt64
        public let writerKey: Data
        public let sealed: Data
        public let serverTsMs: UInt64
    }

    /// `GET .../posts/:cursor/comments?after=&limit=` — a page of at most 20.
    public func listComments(
        channelIdHex: String, postCursor: UInt64, after: UInt64, limit: Int = 20
    ) async throws -> (rows: [CommentRow], nextAfter: UInt64) {
        struct Resp: Decodable {
            struct Row: Decodable {
                let cursor: UInt64
                let writer_key_hex: String
                let sealed_b64: String
                let server_ts_ms: UInt64
            }
            let comments: [Row]
            let next_after: UInt64
        }
        var request = URLRequest(url: try buildURL(
            endpoint: "/v1/channels/\(channelIdHex)/posts/\(postCursor)/comments?after=\(after)&limit=\(limit)"))
        request.httpMethod = "GET"
        let (data, response) = try await channelsPerform(
            request, session: anonymousSession, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        let resp: Resp = try decode(data: data, http: http)
        let rows = resp.comments.compactMap { row -> CommentRow? in
            guard let key = ChannelsClient.hexToData(row.writer_key_hex),
                  let sealed = base64UrlDecodeNoPad(row.sealed_b64) else { return nil }
            return CommentRow(cursor: row.cursor, writerKey: key, sealed: sealed, serverTsMs: row.server_ts_ms)
        }
        return (rows, resp.next_after)
    }

    /// `POST .../posts/:cursor/comments` — one `channel-comment` token.
    /// Returns the comment cursor.
    public func addComment(
        channelIdHex: String, postCursor: UInt64, writeKey: ChannelCommentWriteKey, sealed: Data
    ) async throws -> UInt64 {
        struct Req: Encodable { let writer_key_hex: String; let sealed_b64: String }
        struct Resp: Decodable { let cursor: UInt64 }
        let body = try encoder.encode(Req(
            writer_key_hex: ChannelsComposer.bytesToHex(writeKey.publicKey()),
            sealed_b64: base64UrlEncodeNoPad(sealed)))
        let url = try buildURL(endpoint: "/v1/channels/\(channelIdHex)/posts/\(postCursor)/comments")
        let anonymousSession = self.anonymousSession
        let (data, http) = try await AnonTokenRoute.send(context: .channelComment, tokens: tokens) { header in
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue(header, forHTTPHeaderField: AnonTokenRoute.headerName)
            request.setValue(writeKey.signAdd(sealed: sealed), forHTTPHeaderField: channelWriteProofHeaderName())
            request.httpBody = body
            let (data, response) = try await channelsPerform(
                request, session: anonymousSession, config: .current)
            guard let http = response as? HTTPURLResponse else {
                throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
            }
            return (data, http)
        }
        let resp: Resp = try decode(data: data, http: http)
        return resp.cursor
    }

    /// `DELETE .../comments/:comment` by its author's write key.
    public func deleteOwnComment(
        channelIdHex: String, postCursor: UInt64, commentCursor: UInt64, writeKey: ChannelCommentWriteKey
    ) async throws {
        var request = URLRequest(url: try buildURL(
            endpoint: "/v1/channels/\(channelIdHex)/posts/\(postCursor)/comments/\(commentCursor)"))
        request.httpMethod = "DELETE"
        request.setValue(writeKey.signDelete(commentCursor: commentCursor), forHTTPHeaderField: channelWriteProofHeaderName())
        let (data, response) = try await channelsPerform(
            request, session: anonymousSession, config: .current)
        guard let http = response as? HTTPURLResponse else {
            throw ChannelsApiError(status: -1, code: "no-http-response", detail: "non-HTTP response")
        }
        if http.statusCode == 404 { return }
        let _: EmptyResponse = try decode(data: data, http: http)
    }

    /// `DELETE .../comments/:comment` by a publisher (owner or co-publisher
    /// proof).
    public func deleteCommentAsPublisher(
        channelIdHex: String, postCursor: UInt64, commentCursor: UInt64, accountIdentity: Identity
    ) async throws {
        do {
            let _: EmptyResponse = try await sendAsPublisher(
                signing: IdentityPublisherSigning(accountIdentity: accountIdentity, channelIdHex: channelIdHex),
                method: "DELETE",
                path: "/v1/channels/\(channelIdHex)/posts/\(postCursor)/comments/\(commentCursor)",
                query: "", body: Data(), contentType: nil)
        } catch let error as ChannelsApiError where error.status == 404 {
            return
        }
    }
}
