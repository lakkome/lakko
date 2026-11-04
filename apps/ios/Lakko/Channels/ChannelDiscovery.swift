// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// `@channel` discovery (ADR-0087 sections 1-3 and 6).
///
/// A channel's on-the-wire identity is a 16-byte `channel_id` pseudonym
/// (hex-encoded in every `services/channels` REST route). A channel's
/// human-readable `@name` lives in the one namespace people and groups
/// share: a handle claimed through ``ConversationHandles`` under the
/// PublisherGroup root's handle key, public, its sealed record carrying the
/// channel id and its read key. The channel-name routes of ADR-0010 and the
/// blind channel lookup are gone; this enum only bridges the hex
/// `channel_id` the channels UI speaks to the handle layer.
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
        channelIdHex: String,
        ownerIdentitySeed: Data,
        name: String,
        description: String
    ) async throws -> Bool {
        // ADR-0087 sections 1-3 and 6 — one namespace: a channel's `@name` is
        // a handle claimed under the PublisherGroup root's handle key, public,
        // its record carrying the channel id and its read key. The blind
        // channel lookup and the channel-name register routes are gone.
        let channelId = try ChannelsComposer.channelIdFromHex(channelIdHex)
        let owner = try Identity.fromSeed(seed: ownerIdentitySeed)
        let (journal, facts) = try ConversationHandles.channelFacts(
            ownerIdentity: owner, channelId: channelId, name: name, description: description)
        let written = try await ConversationHandles.claim(
            discovery: DiscoveryClient(), accountSeed: owner.ed25519Seed(), journalId: journal,
            handle: handle, isPublic: true, facts: facts)
        if let accountIdB64 = AcceptedContactsStore.boundAccountIdB64 {
            HeldHandleStore.put(HeldHandleStore.Entry(
                kind: "channel", handle: written.handle, isPublic: true,
                journalIdHex: AttachmentBridge.hex(journal), channelIdHex: channelIdHex.lowercased(),
                name: name, description: description, leaseUntilMs: written.leaseUntilMs), accountIdB64)
        }
        return true
    }

    /// Resolve a typed `@handle` to its channel id hex, or `nil` when no
    /// public channel holds that name (a person or a group holding it
    /// resolves to `nil` here too).
    public static func resolveChannelHandle(
        _ handle: String
    ) async throws -> String? {
        guard let found = try await ConversationHandles.lookupExact(
            discovery: DiscoveryClient(), handle: handle),
              found.kind == .channel, found.id.count == 16 else { return nil }
        return AttachmentBridge.hex(found.id)
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
