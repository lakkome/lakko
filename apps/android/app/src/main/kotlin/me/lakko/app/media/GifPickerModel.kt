// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * UX spec §5 — the GIF tab's state, as a plain class over an injected page
 * fetcher so every state in the spec's table is reachable from a host test.
 *
 *   * Empty query shows Trending; typing is debounced ([SearchDebouncer]) so
 *     the server sees one query per pause, not one per keystroke.
 *   * A response for a query the user has already typed past is dropped (a
 *     generation counter), so a slow answer can never overwrite a newer one.
 *   * `not configured` HIDES the tab for the rest of this picker session;
 *     every other failure is a visible, retryable state.
 *   * Infinite scroll asks for `nextOffset`; a failure while loading more
 *     keeps the items already shown.
 */
public class GifPickerModel(
    private val scope: CoroutineScope,
    private val fetch: suspend (query: String, offset: Int) -> GifResult<GifPage>,
    debounceMs: Long = GIF_SEARCH_DEBOUNCE_MS,
) {
    public enum class Phase {
        /** First page on its way — placeholder tiles. */
        LOADING,
        READY,
        /** `No GIFs found`. */
        EMPTY,
        /** `GIFs are unavailable right now` + Retry. */
        UNAVAILABLE,
        /** `Too many searches — try again in a moment`. */
        RATE_LIMITED,
        /** The proxy is not configured on this server: the tab is hidden. */
        HIDDEN,
    }

    public data class State(
        val query: String = "",
        val items: List<GifItem> = emptyList(),
        val phase: Phase = Phase.LOADING,
        val loadingMore: Boolean = false,
        val nextOffset: Int? = null,
        /** Whether a first page was ever requested. */
        val started: Boolean = false,
    )

    private val mutable = MutableStateFlow(State())
    public val state: StateFlow<State> = mutable.asStateFlow()

    private val debouncer = SearchDebouncer(scope, debounceMs)
    private var generation: Long = 0

    /** The tab became visible. Loads Trending once. */
    public fun open() {
        val s = mutable.value
        if (s.started || s.phase == Phase.HIDDEN) return
        reload(s.query)
    }

    public fun onQueryChange(query: String) {
        if (mutable.value.phase == Phase.HIDDEN) return
        mutable.update { it.copy(query = query) }
        debouncer.schedule { reload(query) }
    }

    /** The keyboard's search action: do not wait out the debounce. */
    public fun submit() {
        debouncer.flush()
    }

    public fun retry() {
        val s = mutable.value
        if (s.phase == Phase.HIDDEN) return
        if (s.items.isNotEmpty() && s.nextOffset != null) {
            loadMore()
        } else {
            reload(s.query)
        }
    }

    public fun loadMore() {
        val s = mutable.value
        val offset = s.nextOffset ?: return
        if (s.loadingMore || s.phase != Phase.READY) return
        val gen = generation
        mutable.update { it.copy(loadingMore = true) }
        scope.launch {
            val result = fetch(s.query, offset)
            if (gen != generation) return@launch
            mutable.update { current ->
                when (result) {
                    is GifResult.Ok -> current.copy(
                        items = (current.items + result.value.items).distinctBy { it.id },
                        nextOffset = result.value.nextOffset,
                        loadingMore = false,
                    )
                    GifResult.NotConfigured -> current.copy(phase = Phase.HIDDEN, loadingMore = false)
                    // Keep what is on screen; stop paging until the user retries.
                    else -> current.copy(loadingMore = false, nextOffset = null)
                }
            }
        }
    }

    /** The picker closed: drop a pending search. */
    public fun close() {
        debouncer.cancel()
        generation += 1
    }

    private fun reload(query: String) {
        generation += 1
        val gen = generation
        mutable.update {
            it.copy(
                query = query,
                items = emptyList(),
                phase = Phase.LOADING,
                loadingMore = false,
                nextOffset = null,
                started = true,
            )
        }
        scope.launch {
            val result = fetch(query, 0)
            if (gen != generation) return@launch
            mutable.update { current ->
                when (result) {
                    is GifResult.Ok -> current.copy(
                        items = result.value.items,
                        nextOffset = result.value.nextOffset,
                        phase = if (result.value.items.isEmpty()) Phase.EMPTY else Phase.READY,
                    )
                    GifResult.NotConfigured -> current.copy(phase = Phase.HIDDEN)
                    is GifResult.RateLimited -> current.copy(phase = Phase.RATE_LIMITED)
                    GifResult.Unauthorized, GifResult.Unavailable ->
                        current.copy(phase = Phase.UNAVAILABLE)
                }
            }
        }
    }
}
