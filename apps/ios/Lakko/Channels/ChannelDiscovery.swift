// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// `@channel` discovery — iOS client wiring for G4-5 / ADR-0010.
///
/// Mirror of `apps/web/src/channels/channel-discovery.ts`.
///
/// A channel's on-the-wire identity is a 16-byte `channel_id` pseudonym
/// (hex-encoded in every `services/channels` REST route). That is
/// unfriendly to type or share, so — exactly like `@username` — a channel
/// may claim a human-readable `@handle`. The mapping lives in the
/// zero-trust discovery service behind the SAME OPRF mechanism as
/// usernames (`services/discovery/src/channel_names.rs`): the server
/// stores only `OPRF(K, normalize(@handle)) -> channel_id`, never the
/// plaintext handle.
///
/// This is a thin channel-facing wrapper over
/// ``DiscoveryClient/registerChannelName(channelId:handle:)`` /
/// ``DiscoveryClient/lookupChannel(handle:)`` (which own the
/// multi-round-trip blind → evaluate (one anonymous token) → finalize →
/// register/lookup dance and reuse the UniFFI `OprfBlinder` blind/finalize
/// primitives — no OPRF is reimplemented here). It only bridges the hex
/// `channel_id` the channels UI speaks to the raw-16-byte form the
/// discovery client speaks.
public enum ChannelDiscovery {

    /// Raw 16-byte channel_id ⇒ 32 lowercase hex chars.
    private static let channelIdHexLen = 32

    /// `true` when `input` is a bare 32-char hex `channel_id` (16 bytes).
    /// Lets the subscribe entry point accept EITHER a pasted raw id OR a
    /// typed `@handle` from one text field without a mode switch — a hex
    /// id is taken verbatim, anything else is resolved as a handle.
    public static func looksLikeChannelIdHex(_ input: String) -> Bool {
        let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count == channelIdHexLen else { return false }
        return trimmed.allSatisfy(\.isHexDigit)
    }

    /// Register the `@handle → channel_id` mapping at channel-create time.
    /// Since ADR-0054 S3 an anonymous write spending a `discovery-lookup`
    /// token (the OPRF step) and a `discovery-register` token, drawn with the
    /// signed-in session by ``AnonTokenPool``; the request itself never
    /// carries the session (ownership lives at the PublisherGroup layer).
    ///
    /// - Parameters:
    ///   - handle:       User-typed handle, with or without a leading `@`
    ///                   — normalised inside `OprfBlinder.blind`.
    ///   - channelIdHex: The freshly-minted 32-char hex channel id.
    /// - Returns: The server `ok` flag.
    /// - Throws: ``DiscoveryClientError/http(status:endpoint:body:)`` with
    ///   `status == 409` when the handle is already taken
    ///   (first-come-first-served); ``AnonTokenUnavailable`` when no token
    ///   could be spent.
    @discardableResult
    public static func registerChannelHandle(
        handle: String,
        channelIdHex: String
    ) async throws -> Bool {
        let channelId = try ChannelsComposer.channelIdFromHex(channelIdHex)
        let client = DiscoveryClient()
        return try await client.registerChannelName(
            channelId: channelId,
            handle: handle
        )
    }

    /// Resolve a typed `@handle` to its channel id hex, or `nil` when no
    /// channel has claimed that handle. Unauthenticated (the lookup path
    /// is open); anonymous-safe.
    ///
    /// - Parameter handle: User-typed handle, with or without a leading `@`.
    /// - Returns: 32-char hex `channel_id`, or `nil` on an unknown handle
    ///   (the server's 404 not-found path).
    public static func resolveChannelHandle(
        _ handle: String
    ) async throws -> String? {
        let client = DiscoveryClient()
        return try await client.lookupChannel(handle: handle)
    }

    /// Convenience for the subscribe entry point: accept EITHER a pasted
    /// 32-char hex `channel_id` OR a typed `@handle`, and return the
    /// resolved channel id hex (or `nil` when a handle does not resolve).
    /// A bare hex id is returned verbatim (lowercased) with no network
    /// round-trip; anything else is OPRF-resolved.
    public static func resolveChannelIdOrHandle(
        _ input: String
    ) async throws -> String? {
        let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return nil }
        if looksLikeChannelIdHex(trimmed) {
            return trimmed.lowercased()
        }
        return try await resolveChannelHandle(trimmed)
    }
}

/// The channel id a `@handle` was claimed for, kept from the moment of the
/// claim until the channel exists: a create that fails after the claim
/// retries with the same id, which discovery answers as already this
/// channel's, and the person's handle is not burned.
///
/// Per account, in `UserDefaults`: a handle and a random id, no secret.
/// Removed by ``AccountLocalDataWipe``.
public enum PendingChannelHandleStore {

    static let keyPrefix = "lakko.channels.pending-handles.v1."

    private static func key(_ accountIdB64: String) -> String { keyPrefix + accountIdB64 }

    private static func normalized(_ handle: String) -> String {
        String(handle.trimmingCharacters(in: .whitespaces).drop(while: { $0 == "@" })).lowercased()
    }

    private static func load(_ accountIdB64: String, _ defaults: UserDefaults) -> [String: String] {
        defaults.data(forKey: key(accountIdB64))
            .flatMap { try? JSONDecoder().decode([String: String].self, from: $0) } ?? [:]
    }

    private static func save(_ held: [String: String], _ accountIdB64: String, _ defaults: UserDefaults) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        if held.isEmpty {
            defaults.removeObject(forKey: key(accountIdB64))
        } else if let data = try? JSONEncoder().encode(held) {
            defaults.set(data, forKey: key(accountIdB64))
        }
    }

    /// The id kept for `handle`, `nil` for none.
    public static func channelId(
        forHandle handle: String, accountIdB64: String, defaults: UserDefaults = .standard
    ) -> String? {
        load(accountIdB64, defaults)[normalized(handle)]
    }

    /// Keep `channelIdHex` for `handle`, before the claim goes out.
    public static func keep(
        handle: String, channelIdHex: String, accountIdB64: String, defaults: UserDefaults = .standard
    ) {
        var held = load(accountIdB64, defaults)
        held[normalized(handle)] = channelIdHex
        save(held, accountIdB64, defaults)
    }

    /// The channel exists, or the handle turned out to be someone else's.
    public static func forget(
        handle: String, accountIdB64: String, defaults: UserDefaults = .standard
    ) {
        var held = load(accountIdB64, defaults)
        guard held.removeValue(forKey: normalized(handle)) != nil else { return }
        save(held, accountIdB64, defaults)
    }

    public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64))
    }
}
