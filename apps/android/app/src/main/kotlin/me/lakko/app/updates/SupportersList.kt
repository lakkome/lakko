// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.updates

import android.content.Context
import android.util.Log
import io.ktor.client.HttpClient
import io.ktor.client.request.get
import io.ktor.client.statement.readBytes
import io.ktor.http.isSuccess
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.transport.lakkoHttpEngine
import uniffi.lakko_ffi_uniffi.verifySupportersList
import java.net.URI
import kotlin.coroutines.cancellation.CancellationException

/*
 * The supporters list (ADR-0097 section 2, `docs/ux/supporters.md`
 * section 2): a static file signed by the release publisher key, fetched when
 * Settings > About is shown and at most once a day otherwise, kept at the
 * highest verified `list_version`. A build with no release key pinned fetches
 * nothing and shows no row.
 */

/** A verified list as this device keeps it. */
@Serializable
public data class HeldSupportersList(
    val listVersion: Long,
    val signedAtMs: Long,
    val names: List<String>,
)

/** What the About row and the Supporters screen draw. */
public data class SupportersUiState(
    /** The held list, or `null` before one was ever verified. */
    val held: HeldSupportersList? = null,
    /** A fetch is running. */
    val loading: Boolean = false,
    /** The last fetch failed (no answer, or a list that did not verify). */
    val failed: Boolean = false,
) {
    /** UX 2.1: the row shows only for a checked list with a name in it. */
    val rowVisible: Boolean get() = held?.names?.isNotEmpty() == true
}

/** Persists the held list. */
public interface SupportersListStore {
    public fun load(): HeldSupportersList?
    public fun save(list: HeldSupportersList)
    public fun lastFetchAtMs(): Long?
    public fun setLastFetchAtMs(at: Long)
}

/** The fetch and the signature check, behind an interface for host tests. */
public interface SupportersListSource {
    /** The raw signed bytes, or `null` on any transport failure. */
    public suspend fun fetch(): ByteArray?

    /** Verify [bytes]; `null` when they do not verify. */
    public fun verify(bytes: ByteArray): HeldSupportersList?
}

/**
 * Keeps the highest verified list. [verifyingKey] `null` (no release key
 * pinned) makes the whole feature dormant.
 */
public class SupportersListController(
    private val store: SupportersListStore,
    private val source: SupportersListSource?,
    private val scope: CoroutineScope,
    private val nowMs: () -> Long = System::currentTimeMillis,
) {
    private val mutableState = MutableStateFlow(SupportersUiState(held = if (source == null) null else load()))

    public val state: StateFlow<SupportersUiState> = mutableState.asStateFlow()

    private var inFlight: Job? = null

    private fun load(): HeldSupportersList? = try {
        store.load()
    } catch (_: Throwable) {
        null
    }

    /** Settings > About is shown, or `Try again` on the screen: fetch now. */
    public fun refresh() {
        if (source == null) return
        start()
    }

    /** At launch: fetch when the last attempt is a day old. */
    public fun refreshIfDue() {
        if (source == null) return
        val last = try {
            store.lastFetchAtMs()
        } catch (_: Throwable) {
            null
        }
        val now = nowMs()
        if (last != null && now >= last && now - last < DAY_MS) return
        start()
    }

    private fun start() {
        val src = source ?: return
        synchronized(this) {
            if (inFlight?.isActive == true) return
            mutableState.update { it.copy(loading = true) }
            inFlight = scope.launch { run(src) }
        }
    }

    private suspend fun run(src: SupportersListSource) {
        try {
            val bytes = src.fetch()
            runCatching { store.setLastFetchAtMs(nowMs()) }
            val verified = bytes?.let { src.verify(it) }
            if (verified == null) {
                mutableState.update { it.copy(loading = false, failed = true) }
                return
            }
            val held = mutableState.value.held
            if (held == null || verified.listVersion > held.listVersion) {
                runCatching { store.save(verified) }
                mutableState.update { it.copy(held = verified, loading = false, failed = false) }
            } else {
                // An equal or older list is ignored (UX 2.1).
                mutableState.update { it.copy(loading = false, failed = false) }
            }
        } catch (cancel: CancellationException) {
            mutableState.update { it.copy(loading = false) }
            throw cancel
        } catch (_: Throwable) {
            mutableState.update { it.copy(loading = false, failed = true) }
        }
    }

    public companion object {
        public const val DAY_MS: Long = 24L * 60L * 60L * 1000L

        @Volatile
        private var instance: SupportersListController? = null

        /** The process-wide controller. */
        public fun get(context: Context): SupportersListController =
            instance ?: synchronized(this) {
                instance ?: build(context.applicationContext).also { instance = it }
            }

        private fun build(app: Context): SupportersListController {
            val key = ReleasePin.verifyingKey
            val source = key?.let { HttpSupportersListSource(supportersListUrl(ReleasePin.manifestUrl), it) }
            return SupportersListController(
                store = PrefsSupportersListStore(app),
                source = source,
                scope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
            )
        }
    }
}

/** The list lives beside the release notice: the same origin as the
 *  manifest URL, path `/.well-known/lakko-supporters`. */
public fun supportersListUrl(manifestUrl: String): String = try {
    URI(manifestUrl).resolve(SUPPORTERS_PATH).toString()
} catch (_: Throwable) {
    ReleasePin.DEFAULT_MANIFEST_URL.substringBefore("/.well-known/") + SUPPORTERS_PATH
}

private const val SUPPORTERS_PATH: String = "/.well-known/lakko-supporters"

/** At most 100 000 names and 2 MiB of names (ADR-0097 section 2.1), plus
 *  the per-name length bytes and the header. */
private const val MAX_LIST_BYTES: Int = 2 * 1024 * 1024 + 100_000 + 1024

private const val TAG: String = "SupportersList"

/** The GET through the anti-censorship engine when armed, verified with the
 *  pinned release key. */
public class HttpSupportersListSource(
    private val url: String,
    private val verifyingKey: ByteArray,
) : SupportersListSource {
    private val client = HttpClient(lakkoHttpEngine())

    override suspend fun fetch(): ByteArray? = try {
        val response = client.get(url)
        if (!response.status.isSuccess()) {
            Log.i(TAG, "supporters list fetch HTTP ${response.status.value}")
            null
        } else {
            val bytes = response.readBytes()
            if (bytes.isEmpty() || bytes.size > MAX_LIST_BYTES) {
                Log.i(TAG, "supporters list body ${bytes.size} bytes out of range")
                null
            } else {
                bytes
            }
        }
    } catch (cancel: CancellationException) {
        throw cancel
    } catch (t: Throwable) {
        Log.i(TAG, "supporters list fetch failed: ${t.javaClass.simpleName}")
        null
    }

    override fun verify(bytes: ByteArray): HeldSupportersList? = try {
        val list = verifySupportersList(bytes, verifyingKey)
        HeldSupportersList(list.listVersion.toLong(), list.signedAtMs, list.names)
    } catch (t: Throwable) {
        Log.i(TAG, "supporters list did not verify")
        null
    }
}

/** The held list in plain preferences: it is public, signed data. */
public class PrefsSupportersListStore(context: Context) : SupportersListStore {
    private val prefs = context.applicationContext.getSharedPreferences("lakko.supporters", Context.MODE_PRIVATE)
    private val json = Json { ignoreUnknownKeys = true }

    override fun load(): HeldSupportersList? =
        prefs.getString(KEY_LIST, null)?.let { runCatching { json.decodeFromString(HeldSupportersList.serializer(), it) }.getOrNull() }

    override fun save(list: HeldSupportersList) {
        prefs.edit().putString(KEY_LIST, json.encodeToString(HeldSupportersList.serializer(), list)).apply()
    }

    override fun lastFetchAtMs(): Long? = if (prefs.contains(KEY_AT)) prefs.getLong(KEY_AT, 0L) else null

    override fun setLastFetchAtMs(at: Long) {
        prefs.edit().putLong(KEY_AT, at).apply()
    }

    private companion object {
        const val KEY_LIST = "list_v1"
        const val KEY_AT = "last_fetch_at"
    }
}
