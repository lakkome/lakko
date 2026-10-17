// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import java.time.ZoneId
import java.util.Locale
import kotlinx.serialization.Serializable
import me.lakko.app.ui.chat.formatListTime

/**
 * Device cards (ADR-0080, docs/ux/second-device.md section 3): the pure
 * rules. The same rules are written for the web (`device-cards.ts`) and iOS
 * (`DeviceCards.swift`); the wire format is Rust's
 * (`lakko-contacts::device_card`).
 */

/** The closed set of platforms a card may name. */
@Serializable
public enum class DeviceCardPlatform { IOS, IPADOS, MACOS, ANDROID, WEB, DESKTOP }

/** A device card as this client keeps it. Times are UNIX milliseconds. */
@Serializable
public data class StoredDeviceCard(
    val platform: DeviceCardPlatform,
    val model: String,
    val appVersion: String,
    val os: String,
    val city: String,
    val country: String,
    val updatedAtMs: Long,
    val lastActiveMs: Long,
) {
    /** `true` when the two differ in a field other than the times. */
    public fun fieldsDiffer(other: StoredDeviceCard): Boolean =
        platform != other.platform || model != other.model || appVersion != other.appVersion ||
            os != other.os || city != other.city || country != other.country
}

public object DeviceCards {
    /** A device sends a fresh card after an action when its last one is older
     *  than this (ADR-0080 section 3). */
    public const val REFRESH_MS: Long = 30L * 60 * 1000

    /** The longest wait before the growth re-send of the own card. */
    public const val GROWTH_MAX_DELAY_MS: Long = 30_000

    /** The city is asked for again at most this often (chosen, not measured). */
    public const val CITY_REFRESH_MS: Long = 6L * 60 * 60 * 1000

    /** Keep a received card, keyed by the sender's leaf key (hex) resolved at
     *  receipt. `null` when nothing changes: the sender is not in the roster,
     *  or the card is not strictly newer than the stored one. */
    public fun receive(
        cards: Map<String, StoredDeviceCard>,
        senderKeyHex: String,
        card: StoredDeviceCard,
        rosterKeysHex: Collection<String>,
    ): Map<String, StoredDeviceCard>? {
        if (senderKeyHex !in rosterKeysHex) return null
        val stored = cards[senderKeyHex]
        if (stored != null && card.updatedAtMs <= stored.updatedAtMs) return null
        return cards + (senderKeyHex to card)
    }

    /** Drop the card of every leaf that left. `null` when none did. */
    public fun prune(
        cards: Map<String, StoredDeviceCard>,
        rosterKeysHex: Collection<String>,
    ): Map<String, StoredDeviceCard>? {
        val keep = cards.filterKeys { it in rosterKeysHex }
        return if (keep.size == cards.size) null else keep
    }

    /** Whether the person's action at [nowMs] must send a fresh own card:
     *  none yet, a field other than the times changed, or the last one is
     *  older than [REFRESH_MS]. A stamp from the future counts as old. */
    public fun shouldSend(last: StoredDeviceCard?, current: StoredDeviceCard, nowMs: Long): Boolean {
        if (last == null) return true
        if (last.fieldsDiffer(current)) return true
        val age = nowMs - last.updatedAtMs
        return age < 0 || age >= REFRESH_MS
    }

    /** A random delay in `[0, 30 s)` for the growth re-send. */
    public fun growthDelayMs(random: Double): Long {
        val clamped = if (random.isFinite()) random.coerceIn(0.0, 0.999_999) else 0.0
        return (clamped * GROWTH_MAX_DELAY_MS).toLong()
    }

    /** Which icon a row shows. */
    public enum class IconKind { PHONE, TABLET, COMPUTER, BROWSER, UNKNOWN }

    public fun iconKind(platform: DeviceCardPlatform?): IconKind = when (platform) {
        DeviceCardPlatform.IOS, DeviceCardPlatform.ANDROID -> IconKind.PHONE
        DeviceCardPlatform.IPADOS -> IconKind.TABLET
        DeviceCardPlatform.MACOS, DeviceCardPlatform.DESKTOP -> IconKind.COMPUTER
        DeviceCardPlatform.WEB -> IconKind.BROWSER
        null -> IconKind.UNKNOWN
    }

    /** A device of the roster, with its card when one arrived. */
    public data class RowInput(
        val keyHex: String,
        val leafIndex: UInt,
        val isSelf: Boolean,
        val card: StoredDeviceCard?,
    )

    /** This device, then the others newest activity first; cardless last, in
     *  roster order. */
    public fun order(rows: List<RowInput>): Pair<RowInput?, List<RowInput>> {
        val self = rows.firstOrNull { it.isSelf }
        val others = rows.filter { !it.isSelf }.sortedWith { a, b ->
            val ca = a.card
            val cb = b.card
            when {
                ca != null && cb != null ->
                    if (ca.lastActiveMs != cb.lastActiveMs) cb.lastActiveMs.compareTo(ca.lastActiveMs)
                    else a.leafIndex.compareTo(b.leafIndex)
                ca != null -> -1
                cb != null -> 1
                else -> a.leafIndex.compareTo(b.leafIndex)
            }
        }
        return self to others
    }

    /** The words a row needs, translated by the caller. */
    public data class Words(
        val device: String,
        val online: String,
        val app: (String) -> String,
        val place: (String, String) -> String,
        val countryName: (String) -> String,
    )

    /** What a row shows. */
    public data class RowView(
        val title: String,
        val detail: String,
        val place: String,
        val activity: String,
        val icon: IconKind,
    )

    public fun view(
        row: RowInput,
        words: Words,
        nowMs: Long,
        zone: ZoneId,
        use24h: Boolean,
        locale: Locale,
    ): RowView {
        val card = row.card
        val activity = when {
            row.isSelf -> words.online
            card == null || card.lastActiveMs <= 0 -> ""
            else -> formatListTime(card.lastActiveMs, nowMs, zone, use24h, locale)
        }
        if (card == null) return RowView(words.device, "", "", activity, IconKind.UNKNOWN)
        val app = words.app(card.appVersion)
        val detail = if (card.os.isEmpty()) app else "$app · ${card.os}"
        val country = if (card.country.isEmpty()) "" else words.countryName(card.country)
        val place = when {
            card.city.isNotEmpty() && country.isNotEmpty() -> words.place(card.city, country)
            card.city.isNotEmpty() -> card.city
            else -> country
        }
        return RowView(
            title = card.model.ifEmpty { words.device },
            detail = detail,
            place = place,
            activity = activity,
            icon = iconKind(card.platform),
        )
    }

    /** "Google Pixel 7a" from `Build.MANUFACTURER` / `Build.MODEL`, without
     *  repeating a maker the model already names. */
    public fun androidModelName(manufacturer: String, model: String): String {
        val maker = manufacturer.trim()
        val name = model.trim()
        if (maker.isEmpty()) return name
        if (name.isEmpty()) return maker.replaceFirstChar { it.uppercase() }
        if (name.startsWith(maker, ignoreCase = true)) return name
        return "${maker.replaceFirstChar { it.uppercase() }} $name"
    }

    /** Storage keys, per account (`AccountLocalDataWipe` removes them). */
    public fun cardsKey(accountIdB64: String): String = "lakko.device-cards.v1.$accountIdB64"
    public fun cityKey(accountIdB64: String): String = "lakko.device-city.v1.$accountIdB64"
    public fun showCityKey(accountIdB64: String): String = "lakko.device-city-shown.v1.$accountIdB64"
}
