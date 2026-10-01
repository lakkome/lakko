// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

/**
 * Readable surfacing of the retired v1 PublisherGroup derivation.
 *
 * Kotlin mirror of `apps/web/src/channels/legacy-v1.ts` and
 * `apps/ios/Lakko/Channels/LegacyChannelV1.swift` — the copy is written once and
 * ported verbatim to all three clients.
 *
 * `crates/lakko-mls/src/publisher_group.rs` narrowed the PublisherGroup MLS
 * group-id prefix from `b"lakko/publisher-group/v1/"` (41 bytes total) to
 * `b"lakko/pubgrp/v2/"` (32 bytes) so the id fits the Delivery Service's 32-byte
 * pseudonym width. An MLS `group_id` is bound into `GroupContext` and every
 * epoch secret derived from it, so v1 state **cannot** be re-keyed in place —
 * `PublisherGroup::load` / `::create` refuse loudly instead, and already-minted
 * `/g/` co-publisher links carry a v1 id that no longer resolves to a channel.
 *
 * Both refusals are correct and neither is repairable. This object turns them
 * from raw Rust error strings into copy a channel owner can act on, and — the
 * load-bearing part — does so **narrowly**, so an unrelated failure (network,
 * server rejection, a bad channel-id hex) still reads as itself.
 *
 * ## What each detection keys on
 *
 * - **The dead invite link** keys on the *bytes*: a 41-byte `group_id` carrying
 *   the retired v1 prefix. Structural, and it cannot drift — the v1 prefix is a
 *   frozen historical constant (nothing new is minted under it) and a v2 id is a
 *   different length. See [isLegacyV1PublisherGroupId].
 *
 * - **The unopenable channel** keys on a substring of the Rust refusal message.
 *   That is genuinely fragile: `lakko_mls::Error::Group(String)` is stringly
 *   typed and UniFFI carries the payload through as `FfiException.Group` with no
 *   further discriminant, so there is nothing else to match on. An edit to the
 *   message in `refuse_if_legacy_v1_present` would silently fall back to the
 *   generic error text with no compile-time signal from this side. The durable
 *   fix is a typed error variant in the core; that is a `crates/` change and
 *   deliberately not made here. See [isLegacyRefusalText].
 */
public object LegacyChannelV1 {

    /**
     * The retired v1 group-id prefix, as ASCII bytes. Frozen: nothing mints
     * under this any more. Mirrors `PUBLISHER_GROUP_ID_PREFIX_V1` in
     * `crates/lakko-mls/src/publisher_group.rs`.
     */
    internal val V1_PREFIX: ByteArray = "lakko/publisher-group/v1/".toByteArray(Charsets.US_ASCII)

    /** Total byte length of a v1 PublisherGroup id: 25-byte prefix + 16-byte channel id. */
    internal const val V1_GROUP_ID_LEN: Int = 25 + 16

    /**
     * Distinctive substring of the Rust-side refusal raised by
     * `refuse_if_legacy_v1_present`. Matched case-sensitively; kept as narrow as
     * possible while staying unique to that one call site.
     */
    internal const val RUST_REFUSAL_MARKER: String = "legacy v1 PublisherGroup state"

    /** User-facing copy for a channel whose MLS state predates the v2 derivation. */
    public const val CHANNEL_MESSAGE: String =
        "This channel was created by an older version of Lakko and cannot be opened in this " +
            "build. Posts you already published stay readable for your subscribers. " +
            "To publish again, create a new channel — this one cannot be repaired."

    /** User-facing copy for a `/g/` co-publisher link minted under the v1 derivation. */
    public const val CO_PUBLISHER_LINK_MESSAGE: String =
        "This co-publisher invite link was created by an older version of Lakko and no longer " +
            "works. Ask the channel owner to send you a new invite link."

    /**
     * True when [groupId] carries the retired 41-byte v1 PublisherGroup shape.
     *
     * Structural: length plus the frozen ASCII prefix. A v2 id is 32 bytes, and
     * a chat / DeviceGroup id is 32 bytes of non-matching content, so this never
     * fires on anything current.
     */
    public fun isLegacyV1PublisherGroupId(groupId: ByteArray): Boolean {
        if (groupId.size != V1_GROUP_ID_LEN) return false
        for (i in V1_PREFIX.indices) {
            if (groupId[i] != V1_PREFIX[i]) return false
        }
        return true
    }

    /**
     * True when [text] carries the Rust-side refusal marker.
     *
     * The single string-matching site on Android — see the object doc for why no
     * better handle exists and why this is fragile. Split out as a pure function
     * over `String` so the unit-test suite can drive it directly.
     */
    public fun isLegacyRefusalText(text: String?): Boolean =
        text != null && text.contains(RUST_REFUSAL_MARKER)

    /** True when [error] is the Rust-side refusal to open a v1 PublisherGroup. */
    public fun isLegacyPublisherGroupError(error: Throwable?): Boolean {
        if (error == null) return false
        if (error is LegacyChannelException) return error.kind == LegacyChannelKind.CHANNEL
        return isLegacyRefusalText(error.message)
    }

    /**
     * Map [error] to a [LegacyChannelException] when it is the retired-v1
     * refusal; otherwise return [error] untouched so unrelated failures keep
     * their own text.
     */
    public fun mapOpenError(error: Throwable): Throwable =
        if (isLegacyPublisherGroupError(error)) {
            LegacyChannelException(LegacyChannelKind.CHANNEL)
        } else {
            error
        }

    /**
     * Message to show for [error]: the retired-v1 copy when it applies,
     * otherwise [fallback] so a network / server / codec failure still reads as
     * itself.
     */
    public fun describe(error: Throwable, fallback: (Throwable) -> String): String = when {
        error is LegacyChannelException -> error.message
        isLegacyPublisherGroupError(error) -> CHANNEL_MESSAGE
        else -> fallback(error)
    }

    /**
     * [describe] as a catalog message (ADR-0061): `channels.legacy.*` when the
     * retired-v1 copy applies, otherwise [fallback].
     */
    public fun describeText(
        error: Throwable,
        fallback: (Throwable) -> me.lakko.app.l10n.UiText,
    ): me.lakko.app.l10n.UiText = when {
        error is LegacyChannelException -> error.text
        isLegacyPublisherGroupError(error) -> me.lakko.app.l10n.UiText(CHANNEL_KEY)
        else -> fallback(error)
    }

    /** Catalog key of [CHANNEL_MESSAGE]. */
    public const val CHANNEL_KEY: String = "channels.legacy.channel"

    /** Catalog key of [CO_PUBLISHER_LINK_MESSAGE]. */
    public const val CO_PUBLISHER_LINK_KEY: String = "channels.legacy.copublisher_link"
}

/** Which of the two retired-v1 surfaces a [LegacyChannelException] describes. */
public enum class LegacyChannelKind {
    /** A channel whose persisted MLS state predates the v2 derivation. */
    CHANNEL,

    /** A `/g/` co-publisher link minted under the v1 derivation. */
    INVITE_LINK,
}

/**
 * A retired-v1 refusal, already carrying its user-facing message.
 *
 * Thrown at the boundaries where the condition is actually detectable, so every
 * UI call site discriminates on the *type* rather than re-doing a string match.
 */
public class LegacyChannelException(
    public val kind: LegacyChannelKind,
) : Exception(
    when (kind) {
        LegacyChannelKind.CHANNEL -> LegacyChannelV1.CHANNEL_MESSAGE
        LegacyChannelKind.INVITE_LINK -> LegacyChannelV1.CO_PUBLISHER_LINK_MESSAGE
    },
) {
    /** What the screen shows for this refusal, in the interface language (ADR-0061). */
    public val text: me.lakko.app.l10n.UiText
        get() = me.lakko.app.l10n.UiText(
            when (kind) {
                LegacyChannelKind.CHANNEL -> LegacyChannelV1.CHANNEL_KEY
                LegacyChannelKind.INVITE_LINK -> LegacyChannelV1.CO_PUBLISHER_LINK_KEY
            },
        )

    /** The English copy for this refusal (logs) — never null, unlike [Throwable.message]. */
    override val message: String
        get() = when (kind) {
            LegacyChannelKind.CHANNEL -> LegacyChannelV1.CHANNEL_MESSAGE
            LegacyChannelKind.INVITE_LINK -> LegacyChannelV1.CO_PUBLISHER_LINK_MESSAGE
        }
}
