// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import me.lakko.app.l10n.UiText

/**
 * Discover (docs/ux/feed.md section 6, ADR-0059): the Discover chip of the
 * Feed and the source of suggestions saved feeds mix in. Mirror of the model
 * half of `apps/web/src/discover/Discover.tsx`.
 *
 * Holds the state (per account, sealed on this device, [DiscoverStore]) and
 * the network: the once-a-day shortlist refresh in rounds of eight buckets
 * padded with the month's decoys ([planBucketRounds]), the explicit search
 * (one `directory-search` token; the UI says it is sent), the listing fetch of
 * a group-shared card when it is opened, and the handle check before
 * subscribing ([verifyDecision]).
 *
 * Every network and FFI touchpoint is behind [DiscoverBackend], so the host
 * JVM suite drives the whole model with a fake.
 */
public interface DiscoverBackend {
    public suspend fun fetchBucket(topic: Int, language: String): List<DirectoryItemWire>
    public suspend fun fetchListing(channelIdHex: String): DirectoryItemWire?
    public suspend fun search(query: String): List<DirectoryItemWire>
    /** The OPRF lookup of a handle: the channel its `@name` record names,
     *  with its read key and name (ADR-0087 sections 4 and 6), or `null`. */
    public suspend fun resolveHandle(handle: String): me.lakko.app.channels.PublicChannelFound?
    public fun decodeListing(listingB64: String): DecodedListing?
    /** The fixed topic ids (names render through [topicName], ADR-0061). */
    public fun topics(): List<Int>
    public fun loadPrefs(accountIdB64: String): DiscoverPrefs
    public fun savePrefs(accountIdB64: String, prefs: DiscoverPrefs)
    /** The UX copy for a failed search / subscribe check (quota, 429, network). */
    public fun searchErrorText(error: Throwable): UiText
    public fun lookupErrorText(error: Throwable): UiText
}

/** The search box's state. */
public sealed interface SearchState {
    public data object Idle : SearchState
    public data object Busy : SearchState
    public data class Done(val query: String, val cards: List<DirectoryCard>) : SearchState
    public data class Failed(val message: UiText) : SearchState
}

public class DiscoverModel(
    private val backend: DiscoverBackend,
    private val account: () -> String?,
    private val scope: CoroutineScope,
    private val appLanguageCode: String,
    private val nowMs: () -> Long = System::currentTimeMillis,
    private val random: () -> Double = { kotlin.random.Random.nextDouble() },
) {
    private val _prefs = MutableStateFlow(DiscoverPrefs())
    public val prefs: StateFlow<DiscoverPrefs> = _prefs.asStateFlow()
    private val _loaded = MutableStateFlow(false)
    public val loaded: StateFlow<Boolean> = _loaded.asStateFlow()
    private val _refreshing = MutableStateFlow(false)
    public val refreshing: StateFlow<Boolean> = _refreshing.asStateFlow()
    private val _unavailable = MutableStateFlow(false)
    public val unavailable: StateFlow<Boolean> = _unavailable.asStateFlow()
    private val _search = MutableStateFlow<SearchState>(SearchState.Idle)
    public val search: StateFlow<SearchState> = _search.asStateFlow()
    private val _gone = MutableStateFlow<Set<String>>(emptySet())
    public val gone: StateFlow<Set<String>> = _gone.asStateFlow()
    private val _details = MutableStateFlow<Map<String, DirectoryItemWire>>(emptyMap())
    /** Listings fetched for group-shared cards opened this session. */
    public val details: StateFlow<Map<String, DirectoryItemWire>> = _details.asStateFlow()
    private val _notice = MutableStateFlow<UiText?>(null)
    public val notice: StateFlow<UiText?> = _notice.asStateFlow()

    private val refreshLock = Mutex()
    private var loadedFor: String? = null

    public val topics: List<Int> by lazy {
        try {
            backend.topics()
        } catch (_: Throwable) {
            emptyList()
        }
    }

    public fun topicName(id: Int): UiText = me.lakko.app.discover.topicName(id)

    /** The language suggestions are fetched in. */
    public fun language(): String = _prefs.value.language ?: appLanguageCode

    /** Load the prefs of the signed-in account (once per account). */
    public suspend fun load() {
        val acct = account() ?: return
        if (loadedFor == acct) return
        _prefs.value = try {
            backend.loadPrefs(acct)
        } catch (_: Throwable) {
            DiscoverPrefs()
        }
        loadedFor = acct
        _loaded.value = true
    }

    private fun persist(next: DiscoverPrefs) {
        _prefs.value = next
        val acct = account() ?: return
        try {
            backend.savePrefs(acct, next)
        } catch (_: Throwable) {
            // Best-effort; the in-memory state stands for this session.
        }
    }

    /** The daily refresh of the picked buckets (or at once when forced or
     *  when the picked set changed). Decoy answers are discarded. */
    public suspend fun refresh(force: Boolean = false) {
        if (!refreshLock.tryLock()) return
        try {
            val p = _prefs.value
            if (p.topics.isEmpty()) return
            val lang = language()
            val now = nowMs()
            val key = pickedKey(p.topics, lang)
            if (!force && !refreshDue(p.lastRefreshMs, p.lastFetchedKey, key, now)) return
            _refreshing.value = true
            val draw = currentDecoys(p.decoys, topics, lang, now, random).draw
            val rounds = planBucketRounds(p.topics, draw, p.mixRandom, random)
            val picked = p.topics.toSet()
            val buckets = HashMap<String, CachedBucket>()
            var failures = 0
            var asked = 0
            for (round in rounds) {
                val results = kotlinx.coroutines.coroutineScope {
                    round.map { t ->
                        async {
                            t to try {
                                Result.success(backend.fetchBucket(t, lang))
                            } catch (e: CancellationException) {
                                throw e
                            } catch (e: Throwable) {
                                Result.failure(e)
                            }
                        }
                    }.awaitAll()
                }
                for ((topic, r) in results) {
                    asked += 1
                    val items = r.getOrNull()
                    if (items == null) {
                        failures += 1
                        continue
                    }
                    if (topic in picked) buckets[bucketKey(topic, lang)] = CachedBucket(now, items)
                }
            }
            if (asked > 0 && failures == asked) {
                _unavailable.value = true
                persist(_prefs.value.copy(decoys = draw))
                return
            }
            _unavailable.value = false
            for (t in picked) {
                val k = bucketKey(t, lang)
                val old = p.buckets[k]
                if (buckets[k] == null && old != null) buckets[k] = old
            }
            persist(_prefs.value.copy(decoys = draw, buckets = buckets, lastRefreshMs = now, lastFetchedKey = key))
        } finally {
            _refreshing.value = false
            refreshLock.unlock()
        }
    }

    /** The cards from the cached picked buckets. */
    public fun bucketCards(): List<DirectoryCard> {
        val p = _prefs.value
        val lang = language()
        return p.topics.flatMap { t ->
            p.buckets[bucketKey(t, lang)]?.items?.let { cardsFromItems(it, CardSource.BUCKET, backend::decodeListing) }
                ?: emptyList()
        }
    }

    /** The scorer's view of the person. */
    public fun context(following: Set<String>): CardContext {
        val p = _prefs.value
        return CardContext(
            pickedTopics = p.topics.toSet(),
            language = language(),
            notInterested = p.notInterested.toSet(),
            hiddenTopics = p.hiddenTopics.toSet(),
            following = following,
            gone = _gone.value,
        )
    }

    /** Group-shared channels as cards; a card opened this session carries its
     *  fetched listing. */
    public fun groupCards(
        shared: List<GroupShared>,
        groupTitle: (String) -> String,
        contactName: (String) -> String?,
    ): List<DirectoryCard> {
        val extra = _details.value
        return shared.map { s ->
            val best = s.byGroup.entries.maxByOrNull { it.value.size }
            val listing = extra[s.channelIdHex]
            val decoded = listing?.let { backend.decodeListing(it.listing_b64) }
            DirectoryCard(
                channelIdHex = s.channelIdHex,
                handle = s.handle,
                name = s.name,
                description = decoded?.description,
                topic = decoded?.topic,
                language = decoded?.language,
                band = listing?.subscribers?.takeIf { it in SUBSCRIBER_BANDS },
                source = CardSource.GROUP,
                serverRank = 1000,
                followers = best?.let { (g, keys) ->
                    GroupFollowers(g, groupTitle(g), keys.size, keys.mapNotNull(contactName))
                },
            )
        }
    }

    public fun runSearch(query: String) {
        val q = query.trim()
        if (q.length < 2) {
            _search.value = SearchState.Failed(DiscoverCopy.SEARCH_TOO_SHORT)
            return
        }
        _search.value = SearchState.Busy
        scope.launch {
            try {
                val items = backend.search(q.take(64))
                val found = rankCards(
                    cardsFromItems(items, CardSource.SEARCH, backend::decodeListing),
                    context(emptySet()).copy(pickedTopics = emptySet()),
                )
                _search.value = SearchState.Done(q, found)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Throwable) {
                _search.value = SearchState.Failed(backend.searchErrorText(e))
            }
        }
    }

    public fun clearSearch() {
        _search.value = SearchState.Idle
    }

    /** Fetch the public listing of a group-shared card (description, topic). */
    public fun openCard(card: DirectoryCard) {
        if (card.description != null) return
        scope.launch {
            try {
                val item = backend.fetchListing(card.channelIdHex)
                if (item == null) {
                    _gone.value = _gone.value + card.channelIdHex
                    _notice.value = DiscoverCopy.GONE
                } else {
                    _details.value = _details.value + (card.channelIdHex to item)
                }
            } catch (e: CancellationException) {
                throw e
            } catch (_: Throwable) {
                _notice.value = DiscoverCopy.UNAVAILABLE
            }
        }
    }

    /** Verify the handle, then hand the channel its record names to
     *  [onVerified] (the one public-channel Subscribe,
     *  [me.lakko.app.channels.subscribePublicChannel]): its read key and its
     *  name, so it reads at once like a search result. A handle that does not
     *  lead to this channel removes the card. */
    public suspend fun subscribe(
        card: DirectoryCard,
        onVerified: (me.lakko.app.channels.PublicChannelFound) -> Unit,
    ) {
        _notice.value = null
        val resolved = try {
            backend.resolveHandle(card.handle)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Throwable) {
            _notice.value = backend.lookupErrorText(e)
            return
        }
        if (resolved == null || verifyDecision(card.channelIdHex, resolved.idHex) == VerifyDecision.GONE) {
            _gone.value = _gone.value + card.channelIdHex
            _notice.value = DiscoverCopy.GONE
            return
        }
        onVerified(
            me.lakko.app.channels.PublicChannelFound(
                idHex = resolved.idHex,
                handle = resolved.handle.ifBlank { card.handle },
                // The record's name; the listing's while a record carries none.
                name = resolved.name.ifBlank { card.name },
                readKey = resolved.readKey,
            ),
        )
    }

    public fun savePicks(topics: List<Int>, language: String) {
        persist(_prefs.value.copy(topics = topics.distinct(), language = language, introDismissed = true))
        scope.launch { refresh(force = true) }
    }

    public fun dismissIntro() {
        persist(_prefs.value.copy(introDismissed = true))
    }

    public fun setMixRandom(on: Boolean) {
        persist(_prefs.value.copy(mixRandom = on))
    }

    public fun notInterested(channelIdHex: String) {
        val list = (_prefs.value.notInterested.filter { it != channelIdHex } + channelIdHex).takeLast(MAX_NOT_INTERESTED)
        persist(_prefs.value.copy(notInterested = list))
    }

    public fun hideTopic(topic: Int) {
        persist(_prefs.value.copy(hiddenTopics = (_prefs.value.hiddenTopics + topic).distinct()))
    }
}
