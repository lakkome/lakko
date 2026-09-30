// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0051 §4 — the channel's shared name and description as the newest
/// metadata record carried them: a bounded, domain-separated record posted
/// as the plaintext of an ordinary channel post, so its authenticity is the
/// post signature (only a publisher can post) and its confidentiality the
/// epoch seed. Readers keep the newest per channel by `updatedAtMs` and
/// title the channel from it; a record is never shown as a post.
public struct SharedChannelMeta: Equatable, Sendable {
    public let name: String
    public let description: String
    /// Publisher-clock UNIX milliseconds. A re-issue (after a rotation)
    /// carries the stored value verbatim, never a fresh clock read, so it can
    /// never out-order a later edit.
    public let updatedAtMs: UInt64
    /// ADR-0059 metadata v2 — the topic id (0 = Other), the ISO 639
    /// language (empty = not set) and whether the owner listed the channel in
    /// Discover. A rename keeps what the channel already said.
    public let topic: UInt8
    public let language: String
    public let listed: Bool

    public init(
        name: String, description: String, updatedAtMs: UInt64,
        topic: UInt8 = 0, language: String = "", listed: Bool = false
    ) {
        self.name = name
        self.description = description
        self.updatedAtMs = updatedAtMs
        self.topic = topic
        self.language = language
        self.listed = listed
    }
}

/// What one decrypted channel post plaintext is (ADR-0051 §4): text to
/// render, a metadata record to keep, or a domain match that failed a bound
/// and is dropped — never rendered as text, because a plaintext that opens
/// with the record's domain was meant as a record.
public enum ChannelPostContent: Equatable {
    case text(String)
    case meta(SharedChannelMeta)
    case malformed
}

/// ADR-0051 §4 — the reader-side sniff and the publisher-side post, over the
/// core's `classifyChannelPostPlaintext` / `composeChannelMeta` so the three
/// clients do not each hand-roll the discriminator.
public enum ChannelMetaRecord {

    /// UX spec §2 — a name is 1 to 64 characters, a description 0 to 300.
    public static let nameMaxChars = 64
    public static let descriptionMaxChars = 300

    /// Classify one decrypted plaintext. A text post whose bytes are not
    /// UTF-8 is reported as `.malformed` too (there is nothing to render).
    public static func classify(_ plaintext: Data) -> ChannelPostContent {
        let content = classifyChannelPostPlaintext(plaintext: plaintext)
        switch content.kind {
        case .text:
            guard let bytes = content.text, let text = String(data: bytes, encoding: .utf8) else {
                return .malformed
            }
            return .text(text)
        case .meta:
            guard let meta = content.meta else { return .malformed }
            return .meta(SharedChannelMeta(
                name: meta.name, description: meta.description, updatedAtMs: meta.updatedAtMs,
                topic: meta.topic, language: meta.language, listed: meta.listed))
        case .malformed:
            return .malformed
        }
    }

    /// Post `meta` into the channel as an ordinary post: compose the record
    /// through the core (which refuses exactly what a reader refuses), consume
    /// a publish counter, seal under the epoch seed, sign, publish. On
    /// success the publisher's own copy is recorded locally through the same
    /// newest-wins rule a reader applies.
    ///
    /// Throws the core's refusal (an empty name, a bound violation) before
    /// any counter is consumed, or the publish error.
    @MainActor
    public static func post(
        state: ChannelsUiState,
        client: ChannelsClient,
        channelId: String,
        meta: SharedChannelMeta
    ) async throws {
        guard let keys = state.keysByChannel[channelId] else {
            throw ChannelMetaError.notAPublisher
        }
        let plaintext = try composeChannelMeta(
            meta: FfiChannelMeta(
                name: meta.name, description: meta.description, updatedAtMs: meta.updatedAtMs,
                topic: meta.topic, language: meta.language, listed: meta.listed))
        let epoch = state.epochByChannel[channelId] ?? 0
        let counter = state.nextCounter(channelId: channelId)
        let wire = try ChannelsComposer.compose(
            keys: keys,
            channelIdHex: channelId,
            epochId: epoch,
            messageCounter: counter,
            plaintext: plaintext
        )
        _ = try await client.publishPost(channelIdHex: channelId, payload: wire)
        state.recordSharedMeta(channelId: channelId, meta: meta)
    }

    /// A fresh record for an edit or the creation: the trimmed fields with a
    /// new stamp. `nil` when the name is empty after trimming (the UX copy for
    /// that is "A channel needs a name").
    ///
    /// ADR-0059 — `keeping` carries the Discover fields over (topic,
    /// language, `listed`) so a rename of a listed channel stays listed.
    public static func fresh(
        name: String, description: String, nowMs: UInt64, keeping: SharedChannelMeta? = nil
    ) -> SharedChannelMeta? {
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else { return nil }
        return SharedChannelMeta(
            name: trimmedName,
            description: description.trimmingCharacters(in: .whitespacesAndNewlines),
            updatedAtMs: nowMs,
            topic: keeping?.topic ?? 0,
            language: keeping?.language ?? "",
            listed: keeping?.listed ?? false
        )
    }
}

public enum ChannelMetaError: Error, Equatable {
    /// This device holds no publisher keys for the channel.
    case notAPublisher
}
