// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * Local channels state (Phase 4 G4-2 Android). Mirror of
 * `apps/web/src/state/channels-state.ts` and
 * `apps/ios/Lakko/Channels/ChannelsUiState.swift`.
 *
 * StateFlow-backed so Compose's `collectAsState` picks
 * up every mutation without manual rebroadcast. Holds
 * the user's channel list, per-channel publisher keys,
 * server-cached posts + epochs. Display names +
 * descriptions are **local-only** — never sent to the
 * server.
 */
public class ChannelsUiState {

    private val _channels = MutableStateFlow<List<ChannelHandle>>(emptyList())
    /** User's local channel list, newest-first. */
    public val channels: StateFlow<List<ChannelHandle>> = _channels.asStateFlow()

    private val _postsByChannel = MutableStateFlow<Map<String, List<PostView>>>(emptyMap())
    /** Per-channel post cache. */
    public val postsByChannel: StateFlow<Map<String, List<PostView>>> =
        _postsByChannel.asStateFlow()

    private val _epochByChannel = MutableStateFlow<Map<String, UInt>>(emptyMap())
    /** Per-channel server epoch cache. */
    public val epochByChannel: StateFlow<Map<String, UInt>> =
        _epochByChannel.asStateFlow()

    private val _keysByChannel =
        MutableStateFlow<Map<String, ChannelPublisherKeys>>(emptyMap())
    /** Per-channel publisher keys (local-only). */
    public val keysByChannel: StateFlow<Map<String, ChannelPublisherKeys>> =
        _keysByChannel.asStateFlow()

    private val _counterByChannel = MutableStateFlow<Map<String, ULong>>(emptyMap())
    /** Per-channel monotone publish counter. Reset on
     *  ``setKeys``. */
    public val counterByChannel: StateFlow<Map<String, ULong>> =
        _counterByChannel.asStateFlow()

    public fun addChannel(handle: ChannelHandle) {
        _channels.update { list ->
            // Id-idempotent: re-adding refreshes the local
            // metadata rather than appending a duplicate.
            val without = list.filter { it.id != handle.id }
            listOf(handle) + without
        }
    }

    public fun removeChannel(channelId: String) {
        _channels.update { it.filter { c -> c.id != channelId } }
        _postsByChannel.update { it - channelId }
        _epochByChannel.update { it - channelId }
        _keysByChannel.update { it - channelId }
        _counterByChannel.update { it - channelId }
    }

    public fun updateLocalMeta(
        channelId: String,
        localName: String? = null,
        localDescription: String? = null,
    ) {
        _channels.update { list ->
            list.map {
                if (it.id == channelId) {
                    it.copy(
                        localName = localName ?: it.localName,
                        localDescription = localDescription ?: it.localDescription,
                    )
                } else {
                    it
                }
            }
        }
    }

    public fun setPosts(channelId: String, posts: List<PostView>) {
        _postsByChannel.update { it + (channelId to posts) }
    }

    public fun setEpoch(channelId: String, epoch: UInt) {
        _epochByChannel.update { it + (channelId to epoch) }
    }

    /** Install fresh publisher keys; resets counter to 0. */
    public fun setKeys(channelId: String, keys: ChannelPublisherKeys) {
        _keysByChannel.update { it + (channelId to keys) }
        _counterByChannel.update { it + (channelId to 0u) }
    }

    /** Atomically read-and-bump the per-channel publish
     *  counter. Returns the value the NEXT post should
     *  carry. */
    public fun nextCounter(channelId: String): ULong {
        var value: ULong = 0u
        _counterByChannel.update { map ->
            value = map[channelId] ?: 0u
            map + (channelId to (value + 1u))
        }
        return value
    }

    public fun reset() {
        _channels.value = emptyList()
        _postsByChannel.value = emptyMap()
        _epochByChannel.value = emptyMap()
        _keysByChannel.value = emptyMap()
        _counterByChannel.value = emptyMap()
    }
}

/** One channel as the local UI sees it. */
public data class ChannelHandle(
    /** 32-char lowercase hex (the wire form). */
    public val id: String,
    public val localName: String,
    public val localDescription: String,
    /** Unix milliseconds. */
    public val addedAtMs: ULong,
)
