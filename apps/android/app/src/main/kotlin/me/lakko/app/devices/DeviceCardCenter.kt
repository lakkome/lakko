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
import kotlinx.coroutines.withTimeoutOrNull
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

    /** The account this device has just joined the DeviceGroup of (from a
     *  pairing Welcome) and not yet sent its own card to; sent at once when
     *  the center is bound for it ([noteJoined]). In memory: a restart in
     *  between leaves the card to the person's next action. */
    @Volatile private var joinedOwed: String? = null
    private val deciding = AtomicBoolean(false)
    private val sending = AtomicBoolean(false)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /**
     * The stored own card is owed to the account's other devices: its send
     * failed, or a growth re-send came due while the app was not showing.
     * Persisted, so a restart keeps it owed. Sent on the next DeviceGroup
     * poll pass or the next time the app shows, whichever comes first, and
     * only while the app is showing (never from the background).
     */
    @Volatile private var owed: Boolean = false

    /** Whether the app is showing; set by `MainActivity`. Becoming shown
     *  sends an owed own card. */
    @Volatile public var foreground: Boolean = false
        set(value) {
            val was = field
            field = value
            if (value && !was) sendOwedIfAny()
        }

    /** The own card's envelope; the native composer, replaceable in a test
     *  (the JVM suite has no native library). */
    @Volatile public var composeEnvelope: (StoredDeviceCard) -> ByteArray? = { card ->
        runCatching { uniffi.lakko_ffi_uniffi.composeDeviceCardEnvelope(toFfi(card)) }.getOrNull()
    }

    /** One plain-text log line; no identifier beyond a platform word. Never
     *  throws (a host test has no `android.util.Log`). */
    private fun log(message: String) {
        try {
            Log.i(TAG, message)
        } catch (_: Throwable) {
            // no log in a host test
        }
    }

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
        owed = own != null && p.getBoolean(DeviceCards.owedKey(accountIdB64), false)
        if (joinedOwed == accountIdB64) sendJoined(accountIdB64)
    }

    @Synchronized
    public fun unbind() {
        accountIdB64 = null
        prefs = null
        sender = null
        city = null
        own = null
        owed = false
        growthJob?.cancel()
        growthJob = null
        mutableCards.value = emptyMap()
        mutableShowCity.value = true
    }

    /** Forget a just-joined card still waiting for the bind (tests). */
    internal fun resetJoined() {
        joinedOwed = null
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
                    if (answer != null && accountIdB64 == bound) storeCity(bound, answer, nowMs)
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
        return composeEnvelope(card)
    }

    /** Whether the stored own card is owed (visible for tests). */
    public fun isOwed(): Boolean = owed

    private fun setOwed(account: String, value: Boolean) {
        owed = value
        val p = prefs ?: return
        AccountStoreGate.write(account) { p.edit().putBoolean(DeviceCards.owedKey(account), value).apply() }
    }

    /** Send the stored own card. A failure keeps it owed. */
    private suspend fun sendOwn() {
        val bound = accountIdB64 ?: return
        val envelope = ownEnvelope() ?: return
        val send = sender ?: return
        try {
            send(envelope)
            if (accountIdB64 == bound) setOwed(bound, false)
            log("own device card sent (android)")
        } catch (cancellation: kotlinx.coroutines.CancellationException) {
            if (accountIdB64 == bound) setOwed(bound, true)
            throw cancellation
        } catch (cause: Throwable) {
            if (accountIdB64 == bound) setOwed(bound, true)
            log("own device card not sent, kept owed (${cause.javaClass.simpleName})")
        }
    }

    /**
     * Send the own card when it is owed and the app is showing. Called on
     * every DeviceGroup poll pass and when the app comes to the foreground;
     * one send at a time.
     */
    public fun sendOwedIfAny() {
        val bound = accountIdB64 ?: return
        if (!owed || !foreground || own == null) return
        if (!sending.compareAndSet(false, true)) return
        scope.launch {
            try {
                if (accountIdB64 == bound && owed && foreground) sendOwn()
            } finally {
                sending.set(false)
            }
        }
    }

    /**
     * This device's own add commit just landed (a local pairing): send the
     * stored own card at once on the post-add handle, so the new device
     * learns this one without waiting for the growth timer.
     */
    public fun sendHandOver() {
        val bound = accountIdB64 ?: return
        if (own == null) return
        if (!sending.compareAndSet(false, true)) {
            setOwed(bound, true)
            return
        }
        scope.launch {
            try {
                if (accountIdB64 == bound) sendOwn()
            } finally {
                sending.set(false)
            }
        }
    }

    /**
     * The card this device puts into a pairing payload it shows as a code or
     * seals for the code route (UX second-device.md section 2.1): its current
     * fields with [place] as the city ([PairingPlace.settled]; `null` = no
     * place), kept out exactly as for its own card (the switch, the
     * anti-censorship connection). `null` when it cannot be composed: the
     * payload then carries no card, and the pairing goes on.
     */
    public fun pairingCardEnvelope(
        place: Pair<String, String>?,
        nowMs: Long = System.currentTimeMillis(),
    ): ByteArray? = try {
        val allowed = place != null && mutableShowCity.value && !antiCensorshipArmed()
        val fields = currentFields(nowMs).copy(
            city = if (allowed) place?.first.orEmpty() else "",
            country = if (allowed) place?.second.orEmpty() else "",
        )
        composeEnvelope(fields)
    } catch (_: Throwable) {
        null
    }

    /** How long a code waits for the city before it is shown (UX
     *  second-device.md section 2.1). Chosen, not measured. */
    public const val PAIRING_PLACE_WAIT_MS: Long = 1_500L

    /**
     * The city for ONE pairing (UX second-device.md section 2.1, "The place
     * in the payload card"). Started when the screen that shows a code or
     * takes the six-digit code opens; [settled] waits for the answer until
     * [PAIRING_PLACE_WAIT_MS] after that moment at most, then returns what
     * has arrived, else a bound account's cached city, else `null` (no
     * place). With no account bound the answer is held here only, for this
     * pairing.
     */
    public class PairingPlace internal constructor(
        private val deadlineMs: Long,
        private val clock: () -> Long,
        initial: Pair<String, String>?,
    ) {
        @Volatile internal var answer: Pair<String, String>? = initial
        @Volatile internal var job: Job? = null

        /** Whether this pairing asked the locate route. */
        public val requested: Boolean get() = job != null

        /** The answer, waiting no later than the bound from the start. The
         *  QR route calls it before building the code (and never again);
         *  the code route when it seals, long after the bound in practice. */
        public suspend fun settled(): Pair<String, String>? {
            val running = job
            if (running != null && !running.isCompleted) {
                val remaining = deadlineMs - clock()
                if (remaining > 0) withTimeoutOrNull(remaining) { running.join() }
            }
            return answer
        }

        /** The screen closed: stop asking. */
        public fun cancel() {
            job?.cancel()
        }
    }

    /**
     * Start the city for one pairing (see [PairingPlace]). Asks nothing with
     * the anti-censorship connection armed or the switch off (a device with
     * no account bound has the default, on); a bound account whose cached
     * city is fresh uses it and asks nothing; otherwise one locate request,
     * whose answer a bound account keeps with its city cache.
     */
    public fun startPairingPlace(
        scope: CoroutineScope = this.scope,
        clock: () -> Long = System::currentTimeMillis,
    ): PairingPlace {
        val startedAtMs = clock()
        val deadline = startedAtMs + PAIRING_PLACE_WAIT_MS
        if (!mutableShowCity.value || antiCensorshipArmed()) return PairingPlace(deadline, clock, null)
        val bound = accountIdB64
        if (bound != null && !cityDue(startedAtMs)) {
            return PairingPlace(deadline, clock, city?.let { it.city to it.country })
        }
        // A bound account's stale cached city is what its own card already
        // states: it stands until a fresh answer replaces it, so no answer
        // by the bound still gives that place. With nothing cached, no
        // answer by the bound gives no place.
        val cached = if (bound != null) city?.let { it.city to it.country } else null
        val place = PairingPlace(deadline, clock, cached)
        place.job = scope.launch {
            val answer = try {
                locate()
            } catch (cancellation: kotlinx.coroutines.CancellationException) {
                throw cancellation
            } catch (_: Throwable) {
                null
            } ?: return@launch
            place.answer = answer
            if (bound != null && accountIdB64 == bound) storeCity(bound, answer, startedAtMs)
        }
        return place
    }

    private fun storeCity(bound: String, answer: Pair<String, String>, nowMs: Long) {
        val fresh = CityAnswer(answer.first, answer.second, nowMs)
        city = fresh
        prefs?.let { p ->
            val row = json.encodeToString(CityAnswer.serializer(), fresh)
            AccountStoreGate.write(bound) { p.edit().putString(DeviceCards.cityKey(bound), row).apply() }
        }
    }

    /**
     * This device let in the device whose leaf key is [keyHex] for
     * [forAccountB64], and the scanned (or code-route) payload carried [card]:
     * keep it as that device's card until its own arrives (the
     * [DeviceCards.pairingCard] rule). A card already stored for that leaf
     * stays.
     */
    @Synchronized
    public fun storePairingCard(forAccountB64: String, keyHex: String, card: StoredDeviceCard?) {
        val account = accountIdB64 ?: return
        if (account != forAccountB64) return
        val next = DeviceCards.pairingCard(mutableCards.value, keyHex, card) ?: return
        mutableCards.value = next
        persist(account)
        log("device card stored from the pairing payload (${card?.platform?.name?.lowercase() ?: "none"})")
    }

    /**
     * This device has just joined [joinedAccountB64]'s DeviceGroup from a
     * pairing Welcome (code scanned or code route): it sends its own card at
     * once, not after the person's next action or a growth wait (the person
     * is looking at it). When the center is not yet bound for that account,
     * the send happens when it is.
     */
    public fun noteJoined(joinedAccountB64: String) {
        joinedOwed = joinedAccountB64
        if (accountIdB64 == joinedAccountB64) sendJoined(joinedAccountB64)
    }

    /** Whether a just-joined card is still waiting for the bind (tests). */
    public fun isJoinedOwed(): Boolean = joinedOwed != null

    private fun sendJoined(account: String) {
        joinedOwed = null
        scope.launch {
            if (accountIdB64 != account) return@launch
            own = currentFields(System.currentTimeMillis())
            persist(account)
            sendOwn()
        }
    }

    /** The DeviceGroup grew: re-send the stored own card, unchanged, after up
     *  to 300 ms (the ADR-0058 discipline), so the new device learns this one.
     *  When the wait ends while the app is not showing, the re-send stays
     *  owed and goes out at the next foreground. */
    @Synchronized
    public fun scheduleResend(random: Double = kotlin.random.Random.nextDouble()) {
        val bound = accountIdB64 ?: return
        if (own == null || growthJob?.isActive == true) return
        val wait = DeviceCards.growthDelayMs(random)
        growthJob = scope.launch {
            delay(wait)
            if (accountIdB64 != bound) return@launch
            if (foreground) {
                sendOwn()
            } else {
                setOwed(bound, true)
                log("device card re-send owed until the app is shown")
            }
        }
    }

    /** A card arrived from [senderKeyHex], resolved at receipt;
     *  [rosterKeysHex] is the DeviceGroup roster at that moment. */
    @Synchronized
    public fun receive(senderKeyHex: String, card: StoredDeviceCard, rosterKeysHex: Collection<String>) {
        val account = accountIdB64 ?: return
        val word = card.platform.name.lowercase()
        val next = DeviceCards.receive(mutableCards.value, senderKeyHex, card, rosterKeysHex)
        if (next == null) {
            log("device card received and refused ($word)")
            return
        }
        mutableCards.value = next
        persist(account)
        log("device card received and stored ($word)")
    }

    /** The received cards and own card stored for [account] in [p]: the
     *  live state when bound to it, else the persisted row. */
    private fun rowFor(p: SharedPreferences?, account: String): Persisted {
        if (accountIdB64 == account) return Persisted(mutableCards.value, own)
        return p?.getString(DeviceCards.cardsKey(account), null)
            ?.let { runCatching { json.decodeFromString(Persisted.serializer(), it) }.getOrNull() }
            ?: Persisted()
    }

    /**
     * ADR-0080 amendment (2026-11-23), confirming device: the cards to hand a
     * device being added, in the pairing state bundle, as (leaf key hex,
     * envelope). Every stored card of a leaf in [rosterKeysHex] (the
     * DeviceGroup after the add), plus this device's own: its last card as
     * sent (stamps verbatim) or, before its first one, its current fields at
     * the lowest stamp so any card it sends later replaces it. A card that
     * does not compose is left out.
     */
    public fun cardsForStateBundle(
        context: Context,
        accountIdB64: String,
        ownKeyHex: String,
        rosterKeysHex: Collection<String>,
        nowMs: Long = System.currentTimeMillis(),
    ): List<Pair<String, ByteArray>> = cardsForStateBundle(
        prefs ?: context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE),
        accountIdB64, ownKeyHex, rosterKeysHex, nowMs,
    )

    @Synchronized
    internal fun cardsForStateBundle(
        p: SharedPreferences?,
        accountIdB64: String,
        ownKeyHex: String,
        rosterKeysHex: Collection<String>,
        nowMs: Long,
    ): List<Pair<String, ByteArray>> {
        val row = rowFor(p, accountIdB64)
        val ownCard = row.own ?: currentFields(nowMs).copy(updatedAtMs = 0L)
        val picked = listOf(ownKeyHex to ownCard) +
            row.cards.filter { (key, _) -> key != ownKeyHex && key in rosterKeysHex }.toList()
        return picked.mapNotNull { (key, card) -> composeEnvelope(card)?.let { key to it } }
    }

    /**
     * ADR-0080 amendment (2026-11-23), new device: store the cards the
     * confirming device handed over ([DeviceCards.acceptCarried]). Works
     * before the center is bound to the account (a restore route lands the
     * session later): the persisted row is updated and read at the bind.
     * Returns how many were kept.
     */
    public fun storeCarried(
        context: Context,
        accountIdB64: String,
        entries: List<Pair<String, StoredDeviceCard>>,
        rosterKeysHex: Collection<String>,
        ownKeyHex: String,
    ): Int = storeCarried(
        prefs ?: context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE),
        accountIdB64, entries, rosterKeysHex, ownKeyHex,
    )

    @Synchronized
    internal fun storeCarried(
        p: SharedPreferences,
        accountIdB64: String,
        entries: List<Pair<String, StoredDeviceCard>>,
        rosterKeysHex: Collection<String>,
        ownKeyHex: String,
    ): Int {
        val row = rowFor(p, accountIdB64)
        val next = DeviceCards.acceptCarried(row.cards, entries, rosterKeysHex, ownKeyHex) ?: return 0
        val kept = next.count { (key, card) -> row.cards[key] != card }
        if (this.accountIdB64 == accountIdB64) {
            mutableCards.value = next
            persist(accountIdB64)
        } else {
            val text = json.encodeToString(Persisted.serializer(), Persisted(next, row.own))
            AccountStoreGate.write(accountIdB64) {
                p.edit().putString(DeviceCards.cardsKey(accountIdB64), text).apply()
            }
        }
        log("device cards from the state bundle: $kept stored")
        return kept
    }

    /** A card arrived that could not be bound to a device of the roster. */
    public fun noteRefused(reason: String) {
        log("device card received and refused ($reason)")
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
            .remove(DeviceCards.owedKey(accountIdB64))
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
