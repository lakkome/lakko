// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import android.content.Context
import android.content.SharedPreferences
import android.os.Build
import android.util.Log
import io.ktor.client.HttpClient
import io.ktor.client.request.get
import io.ktor.client.statement.bodyAsText
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import me.lakko.app.account.AccountStoreGate
import uniffi.lakko_ffi_uniffi.FfiDeviceCard
import uniffi.lakko_ffi_uniffi.FfiDevicePlatform

/**
 * Device cards (ADR-0080): the stateful half on Android. Holds the cards the
 * account's other devices sent, this device's own last card, the city answer
 * and the "Show my city to my other devices" switch; decides on the person's
 * actions whether to send a fresh own card; re-sends it when the DeviceGroup
 * grows. The rules are [DeviceCards].
 *
 * Storage: one `SharedPreferences` file, keys per account
 * (`AccountLocalWipe` removes them). Bound by `DeviceGroupSyncRepository`
 * for the signed-in account; the send path is that repository's DeviceGroup
 * `ServiceEnvelope`.
 */
public object DeviceCardCenter {

    private const val PREFS_NAME = "lakko.device-cards"
    private const val TAG = "DeviceCards"

    @Serializable
    private data class Persisted(
        val cards: Map<String, StoredDeviceCard> = emptyMap(),
        val own: StoredDeviceCard? = null,
    )

    @Serializable
    private data class CityAnswer(val city: String, val country: String, val fetchedAtMs: Long)

    private val json = Json { ignoreUnknownKeys = true }
    private val mutableCards = MutableStateFlow<Map<String, StoredDeviceCard>>(emptyMap())
    private val mutableShowCity = MutableStateFlow(true)

    /** Received cards, keyed by the sender's leaf key (lower-case hex). */
    public val cards: StateFlow<Map<String, StoredDeviceCard>> = mutableCards.asStateFlow()

    /** The Privacy switch (per device, on by default). */
    public val showCity: StateFlow<Boolean> = mutableShowCity.asStateFlow()

    @Volatile private var own: StoredDeviceCard? = null
    @Volatile private var city: CityAnswer? = null
    @Volatile private var accountIdB64: String? = null
    @Volatile private var prefs: SharedPreferences? = null
    @Volatile private var sender: (suspend (ByteArray) -> Unit)? = null
    @Volatile private var growthJob: Job? = null
    private val deciding = AtomicBoolean(false)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** Whether the app is showing; set by `MainActivity`. */
    @Volatile public var foreground: Boolean = false

    /** Whether the anti-censorship connection is armed (then no city is
     *  asked for: the device's address is Lakko's relay). Replaceable in a
     *  test. */
    @Volatile public var antiCensorshipArmed: () -> Boolean =
        { me.lakko.app.transport.AntiCensorshipConfig.current.armed }

    /** The locate route; replaceable in a test. */
    @Volatile public var locate: suspend () -> Pair<String, String>? = { fetchLocation() }

    /** This device's platform facts; replaceable in a test. */
    @Volatile public var platformFacts: () -> Triple<String, String, String> = {
        Triple(
            DeviceCards.androidModelName(Build.MANUFACTURER ?: "", Build.MODEL ?: ""),
            "Android ${Build.VERSION.RELEASE}",
            me.lakko.app.BuildConfig.VERSION_NAME,
        )
    }

    @Synchronized
    public fun bind(context: Context, accountIdB64: String, sender: suspend (ByteArray) -> Unit) {
        this.sender = sender
        if (this.accountIdB64 == accountIdB64) return
        unbind()
        val p = context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        bindStorage(p, accountIdB64, sender)
    }

    @Synchronized
    internal fun bindStorage(p: SharedPreferences, accountIdB64: String, sender: suspend (ByteArray) -> Unit) {
        prefs = p
        this.accountIdB64 = accountIdB64
        this.sender = sender
        val row = p.getString(DeviceCards.cardsKey(accountIdB64), null)
            ?.let { runCatching { json.decodeFromString(Persisted.serializer(), it) }.getOrNull() }
        mutableCards.value = row?.cards ?: emptyMap()
        own = row?.own
        city = p.getString(DeviceCards.cityKey(accountIdB64), null)
            ?.let { runCatching { json.decodeFromString(CityAnswer.serializer(), it) }.getOrNull() }
        mutableShowCity.value = p.getBoolean(DeviceCards.showCityKey(accountIdB64), true)
    }

    @Synchronized
    public fun unbind() {
        accountIdB64 = null
        prefs = null
        sender = null
        city = null
        own = null
        growthJob?.cancel()
        growthJob = null
        mutableCards.value = emptyMap()
        mutableShowCity.value = true
    }

    /** This device's own last card. */
    public fun ownCard(): StoredDeviceCard? = own

    private fun persist(account: String) {
        val p = prefs ?: return
        val row = json.encodeToString(Persisted.serializer(), Persisted(mutableCards.value, own))
        AccountStoreGate.write(account) { p.edit().putString(DeviceCards.cardsKey(account), row).apply() }
    }

    /** What this device says about itself now. */
    public fun currentFields(nowMs: Long): StoredDeviceCard {
        val (model, os, appVersion) = platformFacts()
        val place = if (mutableShowCity.value && !antiCensorshipArmed()) city else null
        return StoredDeviceCard(
            platform = DeviceCardPlatform.ANDROID,
            model = model,
            appVersion = appVersion,
            os = os,
            city = place?.city ?: "",
            country = place?.country ?: "",
            updatedAtMs = nowMs,
            lastActiveMs = nowMs,
        )
    }

    private fun cityDue(nowMs: Long): Boolean {
        if (!mutableShowCity.value || antiCensorshipArmed()) return false
        val answer = city ?: return true
        val age = nowMs - answer.fetchedAtMs
        return age < 0 || age >= DeviceCards.CITY_REFRESH_MS
    }

    /**
     * The person did something on this device. Sends a fresh own card when
     * the rules say so; never on a timer, never in the background.
     */
    public fun noteAction(nowMs: Long = System.currentTimeMillis()) {
        val bound = accountIdB64 ?: return
        if (!foreground) return
        val due = cityDue(nowMs)
        if (!due && !DeviceCards.shouldSend(own, currentFields(nowMs), nowMs)) return
        if (!deciding.compareAndSet(false, true)) return
        scope.launch {
            try {
                if (due) {
                    val answer = locate()
                    if (answer != null && accountIdB64 == bound) {
                        val fresh = CityAnswer(answer.first, answer.second, nowMs)
                        city = fresh
                        prefs?.let { p ->
                            val row = json.encodeToString(CityAnswer.serializer(), fresh)
                            AccountStoreGate.write(bound) { p.edit().putString(DeviceCards.cityKey(bound), row).apply() }
                        }
                    }
                }
                if (accountIdB64 != bound) return@launch
                val fields = currentFields(nowMs)
                if (!DeviceCards.shouldSend(own, fields, nowMs)) return@launch
                own = fields
                persist(bound)
                sendOwn()
            } finally {
                deciding.set(false)
            }
        }
    }

    /** The Privacy switch. Turning it off empties the city and sends at once. */
    public fun setShowCity(on: Boolean) {
        mutableShowCity.value = on
        val account = accountIdB64
        val p = prefs
        if (account != null && p != null) {
            AccountStoreGate.write(account) { p.edit().putBoolean(DeviceCards.showCityKey(account), on).apply() }
        }
        noteAction()
    }

    /** The own card's envelope, stamps verbatim; `null` before the first. */
    public fun ownEnvelope(): ByteArray? {
        val card = own ?: return null
        return runCatching { uniffi.lakko_ffi_uniffi.composeDeviceCardEnvelope(toFfi(card)) }.getOrNull()
    }

    private suspend fun sendOwn() {
        val envelope = ownEnvelope() ?: return
        val send = sender ?: return
        try {
            send(envelope)
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            throw cancellation
        } catch (cause: Throwable) {
            Log.w(TAG, "own device card not sent (${cause.javaClass.simpleName})")
        }
    }

    /** The DeviceGroup grew: re-send the stored own card, unchanged, after up
     *  to 30 s (the ADR-0058 discipline), so the new device learns this one. */
    @Synchronized
    public fun scheduleResend(random: Double = kotlin.random.Random.nextDouble()) {
        val bound = accountIdB64 ?: return
        if (own == null || growthJob?.isActive == true) return
        val wait = DeviceCards.growthDelayMs(random)
        growthJob = scope.launch {
            delay(wait)
            if (accountIdB64 == bound && foreground) sendOwn()
        }
    }

    /** A card arrived from [senderKeyHex], resolved at receipt;
     *  [rosterKeysHex] is the DeviceGroup roster at that moment. */
    @Synchronized
    public fun receive(senderKeyHex: String, card: StoredDeviceCard, rosterKeysHex: Collection<String>) {
        val account = accountIdB64 ?: return
        val next = DeviceCards.receive(mutableCards.value, senderKeyHex, card, rosterKeysHex) ?: return
        mutableCards.value = next
        persist(account)
    }

    /** The roster changed: drop cards of leaves that left. */
    @Synchronized
    public fun prune(rosterKeysHex: Collection<String>) {
        val account = accountIdB64 ?: return
        val next = DeviceCards.prune(mutableCards.value, rosterKeysHex) ?: return
        mutableCards.value = next
        persist(account)
    }

    /** Remove one account's rows (`AccountLocalWipe`). */
    public fun deleteAccountRows(context: Context, accountIdB64: String) {
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit()
            .remove(DeviceCards.cardsKey(accountIdB64))
            .remove(DeviceCards.cityKey(accountIdB64))
            .remove(DeviceCards.showCityKey(accountIdB64))
            .commit()
    }

    public fun toFfi(card: StoredDeviceCard): FfiDeviceCard = FfiDeviceCard(
        updatedAtMs = card.updatedAtMs.coerceAtLeast(0).toULong(),
        lastActiveMs = card.lastActiveMs.coerceAtLeast(0).toULong(),
        platform = when (card.platform) {
            DeviceCardPlatform.IOS -> FfiDevicePlatform.IOS
            DeviceCardPlatform.IPADOS -> FfiDevicePlatform.IPADOS
            DeviceCardPlatform.MACOS -> FfiDevicePlatform.MACOS
            DeviceCardPlatform.ANDROID -> FfiDevicePlatform.ANDROID
            DeviceCardPlatform.WEB -> FfiDevicePlatform.WEB
            DeviceCardPlatform.DESKTOP -> FfiDevicePlatform.DESKTOP
        },
        model = card.model,
        appVersion = card.appVersion,
        os = card.os,
        city = card.city,
        country = card.country,
    )

    public fun fromFfi(card: FfiDeviceCard): StoredDeviceCard = StoredDeviceCard(
        platform = when (card.platform) {
            FfiDevicePlatform.IOS -> DeviceCardPlatform.IOS
            FfiDevicePlatform.IPADOS -> DeviceCardPlatform.IPADOS
            FfiDevicePlatform.MACOS -> DeviceCardPlatform.MACOS
            FfiDevicePlatform.ANDROID -> DeviceCardPlatform.ANDROID
            FfiDevicePlatform.WEB -> DeviceCardPlatform.WEB
            FfiDevicePlatform.DESKTOP -> DeviceCardPlatform.DESKTOP
        },
        model = card.model,
        appVersion = card.appVersion,
        os = card.os,
        city = card.city,
        country = card.country,
        updatedAtMs = card.updatedAtMs.coerceAtMost(Long.MAX_VALUE.toULong()).toLong(),
        lastActiveMs = card.lastActiveMs.coerceAtMost(Long.MAX_VALUE.toULong()).toLong(),
    )

    /** `GET /discovery/locate` (never asked while the anti-censorship
     *  connection is armed). No session, no token. */
    private suspend fun fetchLocation(): Pair<String, String>? = try {
        HttpClient(me.lakko.app.anon.AnonymousHttp.engine).use { client ->
            val response = client.get("${me.lakko.app.discovery.DiscoveryClient.DEFAULT_BASE_URL}/discovery/locate")
            if (response.status.value != 200) {
                null
            } else {
                val body = json.parseToJsonElement(response.bodyAsText()) as? JsonObject
                val read = { key: String -> (body?.get(key) as? JsonPrimitive)?.takeIf { it.isString }?.content ?: "" }
                read("city") to read("country")
            }
        }
    } catch (cancellation: kotlinx.coroutines.CancellationException) {
        throw cancellation
    } catch (_: Throwable) {
        null
    }
}
